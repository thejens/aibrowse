//! Keeping secrets out of the agent's context. Captured traffic is full of credentials
//! (cookies, bearer tokens, CSRF tokens); the agent never needs to see them, because
//! `api_call` replays them itself. Everything shown to the agent passes through here.

use serde_json::Value;
use url::Url;

pub const REDACTED: &str = "[redacted]";

/// Header names that carry credentials.
pub fn is_secret_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
    ) || [
        "csrf",
        "xsrf",
        "token",
        "secret",
        "api-key",
        "apikey",
        "auth",
        "session",
        "signature",
    ]
    .iter()
    .any(|word| name.contains(word))
}

/// Field and query parameter names whose values are credentials. Pagination tokens are
/// data the agent needs to page through results, so they stay visible.
pub fn is_secret_field(name: &str) -> bool {
    let name: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    let paging = ["page", "next", "cursor", "continuation", "sync"]
        .iter()
        .any(|word| name.contains(word));
    (name.ends_with("token") && !paging)
        || [
            "password",
            "passwd",
            "secret",
            "csrf",
            "xsrf",
            "apikey",
            "signature",
        ]
        .iter()
        .any(|word| name.contains(word))
        || matches!(
            name.as_str(),
            "otp" | "pin" | "cvv" | "cvc" | "sessionid" | "sid" | "sig" | "code" | "jwt"
        )
}

pub fn headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = if is_secret_header(name) {
                REDACTED.to_owned()
            } else {
                value.clone()
            };
            (name.clone(), value)
        })
        .collect()
}

pub fn url(raw: &str) -> String {
    let Ok(mut parsed) = Url::parse(raw) else {
        return raw.to_owned();
    };
    if !parsed.query_pairs().any(|(k, _)| is_secret_field(&k)) {
        return raw.to_owned();
    }
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(k, v)| {
            let v = if is_secret_field(&k) {
                REDACTED.to_owned()
            } else {
                v.into_owned()
            };
            (k.into_owned(), v)
        })
        .collect();
    parsed.query_pairs_mut().clear().extend_pairs(pairs);
    parsed.to_string()
}

/// Redacts a request or response body: JSON fields and form fields by name.
pub fn body(text: &str) -> String {
    if let Ok(mut value) = serde_json::from_str::<Value>(text) {
        json(&mut value);
        return serde_json::to_string(&value).unwrap_or_default();
    }
    let looks_like_form = !text.contains(char::is_whitespace)
        && text.contains('=')
        && !text.starts_with(['{', '[', '<']);
    if looks_like_form {
        return url::form_urlencoded::parse(text.as_bytes())
            .map(|(k, v)| {
                let v = if is_secret_field(&k) {
                    REDACTED.into()
                } else {
                    v
                };
                format!("{k}={v}")
            })
            .collect::<Vec<_>>()
            .join("&");
    }
    text.to_owned()
}

/// Redacts a raw React Server Components (flight) payload: each JSON model row by field
/// name, like any JSON body. Text and other rows pass through.
pub fn flight(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            let Some((id, model)) = line.split_once(':') else {
                return line.to_owned();
            };
            let row = !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit());
            match serde_json::from_str::<Value>(model).ok().filter(|_| row) {
                Some(mut value) => {
                    json(&mut value);
                    format!("{id}:{value}")
                }
                None => line.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, field) in map.iter_mut() {
                if is_secret_field(key) && !field.is_object() && !field.is_array() {
                    *field = Value::String(REDACTED.into());
                } else {
                    json(field);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(json),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_hidden_and_data_is_not() {
        assert!(is_secret_header("csrf-token"));
        assert!(is_secret_header("Authorization"));
        assert!(!is_secret_header("x-li-track"));
        assert!(is_secret_field("access_token"));
        assert!(is_secret_field("password"));
        assert!(!is_secret_field("nextPageToken"));
        assert!(!is_secret_field("author"));
        assert_eq!(
            url("https://x.com/cb?code=abc&state=1"),
            "https://x.com/cb?code=%5Bredacted%5D&state=1"
        );
        assert_eq!(
            body(r#"{"user":"a","password":"hunter2","nested":{"refresh_token":"r"}}"#),
            r#"{"nested":{"refresh_token":"[redacted]"},"password":"[redacted]","user":"a"}"#
        );
        assert_eq!(body("user=a&password=b"), "user=a&password=[redacted]");
    }
}
