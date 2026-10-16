//! Documented APIs for `api_list` (docs/design.md §5.4): the site's OpenAPI or Swagger
//! descriptions, read when the agent lists endpoints and merged with what the page was
//! seen calling. Looked for where the site names them (its API catalog, `service-desc`
//! links, a description its own pages loaded, as Swagger UI and Redoc do), and at the
//! conventional paths of the origins its code calls, never at random sites.

use std::time::Duration;

use aibrowse_frontdoor::{ApiSpec, looks_like_spec, origin_of, spec_candidates};
use aibrowse_network::{Catalog, DocParam, Documented, DocumentedOp, Endpoint, Shape, Template};
use url::Url;

use crate::session::Live;

/// How long listing endpoints waits for descriptions: they are read once per session,
/// so only the first listing on a site pays this.
const LOOKUP_TIME: Duration = Duration::from_secs(8);

/// Reads the API descriptions of the active page's site and adds their operations to
/// `catalog`.
pub async fn add_documented(live: &mut Live, catalog: &Catalog) {
    let Some(origin) = origin_of(&live.tab().url()) else {
        return;
    };
    let site = aibrowse_network::site_of(origin.host_str().unwrap_or_default());
    let doors = live.doors().clone();
    let endpoints = catalog.endpoints();
    let mut named: Vec<String> = Vec::new();
    if let Some(api_catalog) = &doors.get(&origin).await.api_catalog {
        named.extend(api_catalog.apis.iter().filter_map(|api| api.spec.clone()));
    }
    named.extend(
        doors
            .page_links(&origin)
            .into_iter()
            .filter(|link| link.rel == "service-desc")
            .map(|link| link.href),
    );
    named.extend(
        endpoints
            .iter()
            .filter(|e| loaded_a_spec(e))
            .filter_map(spec_url),
    );
    let candidates = spec_candidates(&named, &api_origins(&endpoints, &site));
    let Ok(specs) = tokio::time::timeout(LOOKUP_TIME, doors.api_specs(&candidates)).await else {
        tracing::debug!(%site, "reading API descriptions timed out");
        return;
    };
    for spec in specs {
        catalog.document(&site, operations(&spec));
    }
}

/// The site's own origins its pages call for data, the busiest first: where its API
/// description would be served.
fn api_origins(endpoints: &[Endpoint], site: &str) -> Vec<Url> {
    let mut origins: Vec<(String, u32)> = Vec::new();
    for e in endpoints
        .iter()
        .filter(|e| e.site == site && !e.third_party && e.calls > 0 && e.is_api())
    {
        match origins.iter_mut().find(|(o, _)| *o == e.template.origin) {
            Some((_, calls)) => *calls += e.calls,
            None => origins.push((e.template.origin.clone(), e.calls)),
        }
    }
    origins.sort_by_key(|(_, calls)| std::cmp::Reverse(*calls));
    origins
        .into_iter()
        .filter_map(|(origin, _)| Url::parse(&origin).ok())
        .collect()
}

/// Whether the page loaded an API description (a Swagger UI or Redoc page does). A JSON
/// one is known by its keys; a YAML one isn't decoded on capture, so any YAML the site
/// itself served is a candidate, and reading it tells.
fn loaded_a_spec(endpoint: &Endpoint) -> bool {
    match &endpoint.response_shape {
        Some(Shape::Object(fields)) => {
            let keys: Vec<&str> = fields.keys().map(String::as_str).collect();
            looks_like_spec(&keys)
        }
        _ => {
            !endpoint.third_party
                && (endpoint
                    .content_type
                    .as_deref()
                    .is_some_and(|ct| ct.contains("yaml"))
                    || [".yaml", ".yml"]
                        .iter()
                        .any(|ext| endpoint.template.path().ends_with(ext)))
        }
    }
}

fn spec_url(endpoint: &Endpoint) -> Option<String> {
    match &endpoint.sample {
        Some(sample) => Some(sample.url.clone()),
        None if endpoint.template.params().next().is_none() => {
            Some(format!("{}{}", endpoint.template.origin, endpoint.template))
        }
        None => None,
    }
}

fn operations(spec: &ApiSpec) -> Vec<DocumentedOp> {
    spec.operations
        .iter()
        .filter_map(|op| {
            Some(DocumentedOp {
                method: op.method.clone(),
                template: Template::documented(spec.base.as_str(), &op.path)?,
                doc: Documented {
                    spec: spec.url.clone(),
                    summary: op.summary.clone(),
                    params: op
                        .params
                        .iter()
                        .map(|p| DocParam {
                            name: p.name.clone(),
                            location: p.location.clone(),
                            required: p.required,
                        })
                        .collect(),
                    body: op.body.clone(),
                },
            })
        })
        .collect()
}
