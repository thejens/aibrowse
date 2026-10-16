//! API descriptions in OpenAPI 3.x and Swagger 2.0 (JSON or YAML): the operations a site
//! documents, so `api_list` can show documented endpoints beside the ones learned from
//! traffic, with their summaries and parameters.
//!
//! Only what an agent needs to call an operation is kept: method, path template,
//! summary, parameters and whether it takes a body. Schemas are left out; the response
//! shape comes from calling it.

use serde_json::Value;
use url::Url;

use crate::text::clip;

/// Operations kept per description: large APIs document thousands, and past this the
/// agent filters api_list anyway.
const MAX_OPERATIONS: usize = 1_000;
const SUMMARY_CHARS: usize = 160;
const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

#[derive(Debug, Clone, PartialEq)]
pub struct ApiSpec {
    /// Where the description was read.
    pub url: String,
    pub title: Option<String>,
    /// The base URL the operations' paths are relative to (`https://api.example.com/v1`).
    pub base: Url,
    pub operations: Vec<Operation>,
    /// Operations in the description, when more than were kept.
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    /// Upper case: "GET", "POST"...
    pub method: String,
    /// The path as documented, relative to the base: "/users/{id}".
    pub path: String,
    pub summary: Option<String>,
    pub params: Vec<Param>,
    /// The request body's content type, when it takes one.
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    /// "path", "query" or "header".
    pub location: String,
    pub required: bool,
}

/// The description in `body`, served at `url`, whether written in JSON or YAML; `None`
/// when it is neither, or not OpenAPI 3 or Swagger 2.
pub fn read(url: &Url, body: &[u8]) -> Option<ApiSpec> {
    let json: Value = match serde_json::from_slice(body) {
        Ok(json) => json,
        // The body comes from the site: serde-saphyr parses it without panicking and
        // within its default budget (nesting, nodes, alias expansion), so a hostile
        // document can't take the server down.
        Err(_) => serde_saphyr::from_slice(body).ok()?,
    };
    parse(url, &json)
}

/// The description at `url`, or `None` when `json` isn't OpenAPI 3 or Swagger 2.
pub fn parse(url: &Url, json: &Value) -> Option<ApiSpec> {
    let paths = json["paths"].as_object()?;
    let swagger2 = major_version(&json["swagger"]) == Some('2');
    if !swagger2 && major_version(&json["openapi"]) != Some('3') {
        return None;
    }
    let base = if swagger2 {
        swagger_base(url, json)
    } else {
        openapi_base(url, json)
    }?;
    let mut operations = Vec::new();
    let mut count = 0;
    for (path, item) in paths {
        let shared = item["parameters"].as_array().cloned().unwrap_or_default();
        for method in METHODS {
            let Some(op) = item.get(method).filter(|op| op.is_object()) else {
                continue;
            };
            count += 1;
            if operations.len() >= MAX_OPERATIONS {
                continue;
            }
            let mut params: Vec<Param> = Vec::new();
            let mut body = None;
            for raw in shared
                .iter()
                .chain(op["parameters"].as_array().into_iter().flatten())
            {
                let raw = resolve(json, raw);
                let (Some(name), Some(location)) = (raw["name"].as_str(), raw["in"].as_str())
                else {
                    continue;
                };
                match location {
                    // Swagger 2 documents bodies and form fields as parameters.
                    "body" => body = Some("application/json".to_owned()),
                    "formData" => body = Some("application/x-www-form-urlencoded".to_owned()),
                    "path" | "query" | "header" => {
                        // An operation's own parameter overrides the path item's.
                        params.retain(|p| !(p.name == name && p.location == location));
                        params.push(Param {
                            name: name.to_owned(),
                            location: location.to_owned(),
                            required: raw["required"].as_bool().unwrap_or(location == "path"),
                        });
                    }
                    _ => {}
                }
            }
            if let Some(content) = resolve(json, &op["requestBody"])["content"].as_object() {
                body = content.keys().next().cloned();
            }
            let summary = op["summary"]
                .as_str()
                .or_else(|| op["description"].as_str())
                .or_else(|| op["operationId"].as_str())
                .map(|s| clip(s.lines().next().unwrap_or_default(), SUMMARY_CHARS))
                .filter(|s| !s.is_empty());
            operations.push(Operation {
                method: method.to_ascii_uppercase(),
                path: path.clone(),
                summary,
                params,
                body,
            });
        }
    }
    Some(ApiSpec {
        url: url.to_string(),
        title: json["info"]["title"]
            .as_str()
            .map(|t| clip(t, SUMMARY_CHARS)),
        base,
        operations,
        count,
    })
}

/// OpenAPI 3: the first server, which may be relative to the description's own URL;
/// without one, the description's origin.
/// The first digit of a version field. YAML reads an unquoted `swagger: 2.0` as a
/// number, so numbers count too.
fn major_version(field: &Value) -> Option<char> {
    match field {
        Value::String(v) => v.chars().next(),
        Value::Number(v) => v.to_string().chars().next(),
        _ => None,
    }
}

fn openapi_base(url: &Url, json: &Value) -> Option<Url> {
    match json["servers"][0]["url"].as_str() {
        // Server variables ("{region}") can't be filled in here: their defaults can.
        Some(server) => url
            .join(&fill_variables(server, &json["servers"][0]["variables"]))
            .ok(),
        None => url.join("/").ok(),
    }
}

/// Swagger 2: `host` and `basePath`, defaulting to where the description was read.
fn swagger_base(url: &Url, json: &Value) -> Option<Url> {
    let scheme = json["schemes"]
        .as_array()
        .and_then(|s| s.iter().filter_map(Value::as_str).find(|s| *s == "https"))
        .unwrap_or(url.scheme());
    let host = json["host"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| url[url::Position::BeforeHost..url::Position::AfterPort].to_owned());
    let base_path = json["basePath"].as_str().unwrap_or("/");
    Url::parse(&format!("{scheme}://{host}{base_path}")).ok()
}

fn fill_variables(server: &str, variables: &Value) -> String {
    let mut filled = server.to_owned();
    for (name, variable) in variables.as_object().into_iter().flatten() {
        if let Some(default) = variable["default"].as_str() {
            filled = filled.replace(&format!("{{{name}}}"), default);
        }
    }
    filled
}

/// Follows a local `$ref` ("#/components/parameters/Id"), once.
fn resolve<'a>(root: &'a Value, value: &'a Value) -> &'a Value {
    let Some(pointer) = value["$ref"].as_str().and_then(|r| r.strip_prefix('#')) else {
        return value;
    };
    root.pointer(pointer).unwrap_or(value)
}

/// Where API descriptions conventionally live (FastAPI, springdoc, Swashbuckle,
/// express-openapi...). Tried only on origins the site's own code already calls.
const CONVENTIONAL_PATHS: &[&str] = &[
    "/openapi.json",
    "/swagger.json",
    "/v3/api-docs",
    "/v2/api-docs",
    "/api-docs",
    "/api/openapi.json",
    "/swagger/v1/swagger.json",
    "/openapi.yaml",
    "/swagger.yaml",
];
/// Origins tried at the conventional paths, at most.
const MAX_PROBED_ORIGINS: usize = 3;

/// The URLs to look for descriptions at, in order: those the site names (`named`: its
/// API catalog, `service-desc` links, descriptions its pages loaded), then the
/// conventional paths on `api_origins`, the origins its own code calls.
pub fn candidates(named: &[String], api_origins: &[Url]) -> Vec<Url> {
    let mut urls: Vec<Url> = named.iter().filter_map(|n| Url::parse(n).ok()).collect();
    for origin in api_origins.iter().take(MAX_PROBED_ORIGINS) {
        for path in CONVENTIONAL_PATHS {
            if let Ok(url) = origin.join(path)
                && !urls.contains(&url)
            {
                urls.push(url);
            }
        }
    }
    urls
}

/// Whether a JSON document looks like an API description, by its top-level keys: for
/// recognizing one that a page (Swagger UI, Redoc) loaded.
pub fn looks_like_spec(keys: &[&str]) -> bool {
    keys.contains(&"paths") && (keys.contains(&"openapi") || keys.contains(&"swagger"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_openapi_3() {
        let url = Url::parse("https://example.com/docs/openapi.json").unwrap();
        let spec = parse(
            &url,
            &json!({
                "openapi": "3.1.0",
                "info": { "title": "Pets" },
                "servers": [{ "url": "/api/{version}", "variables": { "version": { "default": "v2" } } }],
                "components": { "parameters": { "Limit": { "name": "limit", "in": "query" } } },
                "paths": {
                    "/pets/{petId}": {
                        "parameters": [{ "name": "petId", "in": "path", "required": true }],
                        "get": { "summary": "Show a pet\nLong text", "parameters": [{ "$ref": "#/components/parameters/Limit" }] },
                        "delete": { "operationId": "deletePet" }
                    },
                    "/pets": {
                        "post": { "requestBody": { "content": { "application/json": {} } } }
                    }
                }
            }),
        )
        .unwrap();
        assert_eq!(spec.base.as_str(), "https://example.com/api/v2");
        assert_eq!(spec.title.as_deref(), Some("Pets"));
        assert_eq!(spec.count, 3);
        let get = spec.operations.iter().find(|o| o.method == "GET").unwrap();
        assert_eq!(get.path, "/pets/{petId}");
        assert_eq!(get.summary.as_deref(), Some("Show a pet"));
        let names: Vec<&str> = get.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["petId", "limit"]);
        assert!(get.params[0].required && !get.params[1].required);
        let delete = spec
            .operations
            .iter()
            .find(|o| o.method == "DELETE")
            .unwrap();
        assert_eq!(delete.summary.as_deref(), Some("deletePet"));
        let post = spec.operations.iter().find(|o| o.method == "POST").unwrap();
        assert_eq!(post.body.as_deref(), Some("application/json"));
    }

    #[test]
    fn reads_swagger_2() {
        let url = Url::parse("https://example.com/swagger.json").unwrap();
        let spec = parse(
            &url,
            &json!({
                "swagger": "2.0",
                "host": "api.example.com",
                "basePath": "/v1",
                "schemes": ["http", "https"],
                "paths": { "/orders": { "post": { "parameters": [{ "name": "order", "in": "body" }] } } }
            }),
        )
        .unwrap();
        assert_eq!(spec.base.as_str(), "https://api.example.com/v1");
        assert_eq!(spec.operations[0].body.as_deref(), Some("application/json"));
    }

    #[test]
    fn reads_yaml() {
        let url = Url::parse("https://api.example.com/openapi.yaml").unwrap();
        let yaml = br#"
openapi: 3.0.3
info: { title: Shop }
paths:
  /items/{id}:
    parameters: &id
      - { name: id, in: path, required: true }
    get:
      summary: One item
      responses:
        200: { description: The item }
        "404": { description: No such item }
  /items/{id}/tags:
    parameters: *id
    put:
      requestBody:
        content:
          application/json: {}
"#;
        let spec = read(&url, yaml).unwrap();
        assert_eq!(spec.title.as_deref(), Some("Shop"));
        let get = &spec.operations[0];
        assert_eq!(
            (get.method.as_str(), get.path.as_str()),
            ("GET", "/items/{id}")
        );
        assert_eq!(get.summary.as_deref(), Some("One item"));
        assert_eq!(get.params[0].name, "id");
        let put = &spec.operations[1];
        assert_eq!(put.params[0].name, "id");
        assert_eq!(put.body.as_deref(), Some("application/json"));
    }

    #[test]
    fn reads_json_and_rejects_hostile_yaml() {
        let url = Url::parse("https://example.com/swagger").unwrap();
        let json = br#"{"swagger": "2.0", "paths": {"/a": {"get": {}}}}"#;
        assert_eq!(read(&url, json).unwrap().operations.len(), 1);
        // An alias bomb: each anchor repeats the one before twice, 2^40 nodes in all.
        let mut bomb = String::from("openapi: 3.0.0\npaths: {/a: {get: {}}}\nl0: &l0 [x, x]\n");
        for level in 1..40 {
            let below = level - 1;
            bomb.push_str(&format!("l{level}: &l{level} [*l{below}, *l{below}]\n"));
        }
        assert!(read(&url, bomb.as_bytes()).is_none());
        assert!(read(&url, b"not: [a, description").is_none());
    }

    #[test]
    fn other_json_is_no_description() {
        let url = Url::parse("https://example.com/data.json").unwrap();
        assert!(parse(&url, &json!({ "paths": {} })).is_none());
        assert!(parse(&url, &json!({ "openapi": "3.0.0" })).is_none());
        assert!(looks_like_spec(&["openapi", "info", "paths"]));
        assert!(!looks_like_spec(&["paths", "items"]));
    }
}
