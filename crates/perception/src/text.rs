//! Text cleanup for outline names, and link targets an agent can actually read.

pub(crate) const MAX_NAME_CHARS: usize = 100;
pub(crate) const MAX_TEXT_CHARS: usize = 200;
const MAX_HREF_CHARS: usize = 60;

/// Zero-width and bidirectional formatting characters: invisible on screen, but they
/// show up as escapes or noise in text output.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}')
}

pub(crate) fn clean(text: &str, max_chars: usize) -> String {
    let visible: String = text.chars().filter(|c| !is_invisible(*c)).collect();
    let collapsed = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        collapsed
    } else {
        let mut cut: String = collapsed.chars().take(max_chars).collect();
        cut.push('…');
        cut
    }
}

/// "https://example.com" for "https://example.com/path?q".
pub(crate) fn origin(url: &str) -> String {
    let after_scheme = url.find("://").map_or(0, |i| i + 3);
    match url[after_scheme..].find('/') {
        Some(i) => url[..after_scheme + i].to_owned(),
        None => url.to_owned(),
    }
}

/// A link target as the agent should see it: where it really goes (tracking redirects
/// unwrapped), relative to the page's origin, and short.
pub(crate) fn shorten_href(href: &str, origin: &str) -> String {
    let target = unwrap_redirect(href).unwrap_or_else(|| href.to_owned());
    let relative = target
        .strip_prefix(origin)
        .filter(|r| r.starts_with('/'))
        .unwrap_or(&target);
    clean(relative, MAX_HREF_CHARS)
}

/// Click trackers and "you are leaving the site" pages carry the real destination in a
/// query parameter (`/safety/go/?url=…`, `…/sct?destinationUrl=…`).
fn unwrap_redirect(href: &str) -> Option<String> {
    let query = href.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        let wanted = matches!(
            key.to_ascii_lowercase().as_str(),
            "url"
                | "u"
                | "destinationurl"
                | "dest"
                | "destination"
                | "redirect"
                | "redirect_uri"
                | "target"
                | "to"
        );
        let decoded = percent_decode(value);
        (wanted && (decoded.starts_with("http://") || decoded.starts_with("https://")))
            .then_some(decoded)
    })
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&text[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_tracking_redirects() {
        let origin = "https://www.linkedin.com";
        assert_eq!(
            shorten_href(
                "https://www.linkedin.com/safety/go/?url=https%3A%2F%2Ffmind%2Edev&urlhash=2GqT",
                origin
            ),
            "https://fmind.dev"
        );
        assert_eq!(
            shorten_href(
                "/li/tscp/sct?destinationUrl=https%3A%2F%2Fwww.linkedin.com%2Fpremium%2Fredeem&track=x",
                origin
            ),
            "/premium/redeem"
        );
        assert_eq!(
            shorten_href("https://www.linkedin.com/in/larjen/", origin),
            "/in/larjen/"
        );
    }

    #[test]
    fn strips_invisible_characters() {
        assert_eq!(
            clean("Språk: \u{202a}Svenska\u{202c}", 100),
            "Språk: Svenska"
        );
    }
}
