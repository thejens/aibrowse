//! The endpoints a session has seen the site's own code call, clustered by template.
//! Shared by every tab of a browser, so ids stay stable as the agent moves between tabs.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use crate::call::ReplayHeader;
use crate::kb::{Record, Worker};
use crate::rsc::is_flight;
use crate::shape::Shape;
use crate::template::{Template, is_mutating, site};

/// Sites with chatty frontends produce a few hundred endpoints at most; past this, new
/// ones are most likely noise (per-asset URLs misread as APIs) and are not tracked.
const MAX_ENDPOINTS: usize = 500;
/// Documented operations listed beyond the observed endpoints, at most: large APIs
/// document a thousand.
const MAX_DOCUMENTED: usize = 1_000;
/// The UI actions remembered per endpoint ("click button \"Accept\"").
const MAX_TRIGGERS: usize = 3;
/// How long closing the browser waits for learned endpoints to reach the knowledge base.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

/// The last successful concrete request to an endpoint: what `api_call` replays and
/// varies. Held in memory only, since it contains the user's data and credentials.
#[derive(Debug, Clone)]
pub struct Sample {
    pub request_id: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// Cookies sent with the request. Header values copied from a cookie (the
    /// double-submit CSRF pattern) are re-read from the cookie jar at call time.
    pub cookies: Vec<(String, String)>,
}

/// What earlier sessions learned about an endpoint, from the Site Knowledge Base.
#[derive(Debug, Clone)]
pub struct Learned {
    /// Calls seen across earlier sessions.
    pub calls: u64,
    /// When an earlier session last saw it, in Unix seconds.
    pub last_seen: i64,
    /// Headers the site's code fills from a cookie (double-submit CSRF). Names only:
    /// values are read from the cookie jar at call time.
    pub headers: Vec<ReplayHeader>,
    /// Whether the page's requests carried cookies (the fetch credentials mode).
    pub with_cookies: bool,
}

#[derive(Debug, Clone)]
pub struct Endpoint {
    /// "a1", "a2"... in order of discovery (or of loading from the knowledge base).
    pub id: String,
    /// The site of the page that called it, which it is filed under in the knowledge
    /// base: a third-party API belongs to the site whose pages use it.
    pub site: String,
    pub method: String,
    pub template: Template,
    pub mutating: bool,
    /// Served from a different site than the page that called it (analytics, ads, CDNs).
    pub third_party: bool,
    /// Every non-selector query key seen.
    pub query_keys: BTreeSet<String>,
    /// Calls seen in this session.
    pub calls: u32,
    pub last_status: Option<u16>,
    pub content_type: Option<String>,
    pub request_shape: Option<Shape>,
    pub response_shape: Option<Shape>,
    /// UI actions that caused calls to it, most recent last.
    pub triggers: Vec<String>,
    pub sample: Option<Sample>,
    /// Set when an earlier session already knew the endpoint.
    pub learned: Option<Learned>,
    /// What the site's API description says about it, when it has one.
    pub documented: Option<Documented>,
    /// How many of `calls` the knowledge base has already counted.
    pub(crate) persisted_calls: u32,
}

impl Endpoint {
    /// Whether the endpoint returns data or changes something, as opposed to fetching
    /// assets or fragments: what api_list shows by default.
    ///
    /// Data APIs often live on another site (`hn.algolia.com` queries
    /// `*.algolia.net`), so returning JSON counts wherever the endpoint is. Third-party
    /// calls that don't return data are analytics beacons and pixels, never useful.
    /// React Server Components responses are data too: decoded, they are what the page
    /// renders (server actions, pagination).
    pub fn is_api(&self) -> bool {
        self.template.graphql.is_some()
            || self
                .content_type
                .as_deref()
                .is_some_and(|ct| is_json(ct) || is_flight(ct))
            // Decoded as data whatever its label (server-component streams sent as
            // application/octet-stream).
            || self.response_shape.is_some()
            || (self.mutating && !self.third_party)
            || self.documented.is_some()
    }
}

/// An endpoint as the site's API description (OpenAPI, Swagger) documents it.
#[derive(Debug, Clone, PartialEq)]
pub struct Documented {
    /// The description's URL.
    pub spec: String,
    pub summary: Option<String>,
    pub params: Vec<DocParam>,
    /// The request body's content type, when it takes one.
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocParam {
    pub name: String,
    /// "path", "query" or "header".
    pub location: String,
    pub required: bool,
}

/// A documented operation, to add to the catalog.
#[derive(Debug, Clone)]
pub struct DocumentedOp {
    pub method: String,
    pub template: Template,
    pub doc: Documented,
}

pub fn is_json(content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.contains("json") || ct.contains("graphql")
}

#[derive(Clone, Default)]
pub struct Catalog {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    endpoints: Vec<Endpoint>,
    index: HashMap<(String, Template), usize>,
    /// The knowledge base's worker, when the catalog is persistent.
    kb: Option<Worker>,
    /// Sites whose known endpoints were requested from the knowledge base.
    visited: HashSet<String>,
    /// Endpoints changed since the knowledge base last saw them, by index.
    dirty: BTreeSet<usize>,
    /// Endpoints whose templates were generalized from calls that differed in a segment
    /// or two (`Template::generalize`): later calls that fit them are theirs.
    generalized: HashSet<usize>,
}

impl Inner {
    /// The endpoint a call to `template` belongs to by generalization, if any: one whose
    /// template was generalized and fits it, or a known endpoint that differs from it
    /// only in a segment or two (`Template::generalize`), which is generalized now.
    fn generalized(&mut self, method: &str, template: &Template) -> Option<usize> {
        let fitting = self.generalized.iter().copied().find(|&i| {
            let e = &self.endpoints[i];
            e.method == method && template.fit(&e.template).is_some()
        });
        if fitting.is_some() {
            return fitting;
        }
        let (i, general) = self.endpoints.iter().enumerate().find_map(|(i, e)| {
            (e.method == method && e.documented.is_none())
                .then(|| e.template.generalize(template).map(|t| (i, t)))
                .flatten()
        })?;
        let old = (method.to_owned(), self.endpoints[i].template.clone());
        self.index.remove(&old);
        self.endpoints[i].template = general.clone();
        self.index.insert((method.to_owned(), general), i);
        self.generalized.insert(i);
        self.dirty.insert(i);
        Some(i)
    }
}

/// A catalog reference that doesn't keep it alive. The knowledge base's worker thread
/// holds one, so it never outlives the browser that owns the catalog.
#[derive(Clone)]
pub(crate) struct WeakCatalog(Weak<Mutex<Inner>>);

impl WeakCatalog {
    pub(crate) fn upgrade(&self) -> Option<Catalog> {
        self.0.upgrade().map(|inner| Catalog { inner })
    }
}

impl Catalog {
    /// A catalog backed by the Site Knowledge Base at `path`: endpoints learned in earlier
    /// sessions are loaded as the browser visits their site, and what this session learns
    /// is written back in the background.
    pub fn persistent(path: PathBuf) -> Self {
        let catalog = Self::default();
        let worker = Worker::spawn(path, WeakCatalog(Arc::downgrade(&catalog.inner)));
        catalog.lock().kb = Some(worker);
        catalog
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock only means a recorder task panicked mid-update; the data is
        // still a consistent catalog.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Notes that a tab's main frame is now on `page_url`. The first visit to a site asks
    /// the knowledge base for the endpoints earlier sessions learned there.
    pub fn visit(&self, page_url: &str) {
        let Some(page_site) = url::Url::parse(page_url)
            .ok()
            .and_then(|u| u.host_str().map(site))
        else {
            return;
        };
        let mut inner = self.lock();
        if inner.kb.is_some() && inner.visited.insert(page_site.clone()) {
            inner.kb.as_ref().expect("checked above").load(page_site);
        }
    }

    /// Waits until the knowledge base has answered every visit so far, so a listing right
    /// after a navigation includes what earlier sessions learned about the site.
    pub async fn ready(&self) {
        let worker = self.lock().kb.clone();
        if let Some(worker) = worker {
            worker.barrier().await;
        }
    }

    /// Writes what this session learned to the knowledge base and waits for it (bounded),
    /// so closing the browser loses nothing.
    pub async fn flush(&self) {
        let worker = self.lock().kb.clone();
        if let Some(worker) = worker
            && tokio::time::timeout(FLUSH_TIMEOUT, worker.flush())
                .await
                .is_err()
        {
            tracing::warn!("the site knowledge base did not finish writing in time");
        }
    }

    /// The changed endpoints in their persistable form, marked as persisted. Endpoints
    /// that aren't recognizably APIs yet (no response seen) stay pending.
    pub(crate) fn take_changes(&self) -> Vec<Record> {
        let mut inner = self.lock();
        let dirty = std::mem::take(&mut inner.dirty);
        let mut records = Vec::new();
        for index in dirty {
            let endpoint = &mut inner.endpoints[index];
            if let Some(record) = Record::of(endpoint) {
                endpoint.persisted_calls = endpoint.calls;
                records.push(record);
            }
        }
        records
    }

    /// Adds endpoints an earlier session learned. One this session has already seen
    /// (same method and template) gains the earlier knowledge instead of being listed
    /// twice.
    pub(crate) fn learn(&self, records: Vec<Record>) {
        let mut inner = self.lock();
        for record in records {
            let key = (record.method.clone(), record.template.clone());
            match inner.index.get(&key).copied() {
                Some(index) => record.merge_into(&mut inner.endpoints[index]),
                None if inner.endpoints.len() >= MAX_ENDPOINTS => break,
                // Learned under a template this session (or an earlier record) has since
                // generalized: the same endpoint.
                None if let Some(index) = inner.generalized(&key.0, &key.1) => {
                    record.merge_into(&mut inner.endpoints[index]);
                }
                None => {
                    let index = inner.endpoints.len();
                    inner
                        .endpoints
                        .push(record.into_endpoint(format!("a{}", index + 1)));
                    inner.index.insert(key, index);
                }
            }
        }
    }

    /// Adds what the site's API description documents for `site`'s pages. An operation
    /// the session has already seen called gains its documentation; the others are
    /// listed as documented, not yet called. They are knowledge from the site, not from
    /// this browser, so they aren't stored: the description is read again next time.
    pub fn document(&self, site: &str, ops: Vec<DocumentedOp>) {
        let mut inner = self.lock();
        let documented_only = |inner: &Inner| {
            inner
                .endpoints
                .iter()
                .filter(|e| e.calls == 0 && e.learned.is_none() && e.documented.is_some())
                .count()
        };
        let mut room = MAX_DOCUMENTED.saturating_sub(documented_only(&inner));
        // The most specific first: `/users/me` claims the calls to it before
        // `/users/{id}` could.
        let mut ops = ops;
        ops.sort_by_key(|op| std::cmp::Reverse(op.template.fit(&op.template)));
        for op in ops {
            let fits = |e: &Endpoint| e.method == op.method && e.template.fits(&op.template);
            // Documented by an earlier look at the same description.
            if inner
                .endpoints
                .iter()
                .any(|e| fits(e) && e.documented.as_ref() == Some(&op.doc))
            {
                continue;
            }
            let seen = inner
                .endpoints
                .iter()
                .position(|e| fits(e) && e.documented.is_none());
            if let Some(index) = seen {
                inner.endpoints[index].documented = Some(op.doc);
                continue;
            }
            if room == 0 {
                break;
            }
            room -= 1;
            let index = inner.endpoints.len();
            let key = (op.method.clone(), op.template.clone());
            inner.endpoints.push(Endpoint {
                id: format!("a{}", index + 1),
                site: site.to_owned(),
                mutating: is_mutating(&op.method, None),
                method: op.method,
                template: op.template,
                third_party: false,
                query_keys: BTreeSet::new(),
                calls: 0,
                last_status: None,
                content_type: None,
                request_shape: None,
                response_shape: None,
                triggers: Vec::new(),
                sample: None,
                learned: None,
                documented: Some(op.doc),
                persisted_calls: 0,
            });
            inner.index.insert(key, index);
        }
    }

    /// The endpoint a request belongs to, registering it if new. `None` for requests
    /// that can't be templated (non-HTTP) or once the catalog is full.
    pub(crate) fn classify(
        &self,
        method: &str,
        url: &str,
        body: Option<&str>,
        headers: &[(String, String)],
        document_url: &str,
    ) -> Option<String> {
        let template = Template::of(url, body)?.with_action(headers);
        let query_keys: Vec<String> = url::Url::parse(url)
            .map(|u| {
                u.query_pairs()
                    .map(|(k, _)| k.into_owned())
                    .filter(|k| !template.selectors.iter().any(|(s, _)| s == k))
                    .collect()
            })
            .unwrap_or_default();
        let request_shape = body
            .and_then(|b| serde_json::from_str(b).ok())
            .map(|v| Shape::of(&v));
        let mut inner = self.lock();
        let key = (method.to_owned(), template);
        // A call to a documented endpoint is that endpoint, under its documented name.
        let documented = || {
            inner
                .endpoints
                .iter()
                .enumerate()
                .filter(|(_, e)| e.documented.is_some() && e.method == key.0)
                .filter_map(|(i, e)| key.1.fit(&e.template).map(|fit| (fit, i)))
                .max_by_key(|(fit, i)| (*fit, std::cmp::Reverse(*i)))
                .map(|(_, i)| i)
        };
        let known = inner.index.get(&key).copied().or_else(documented);
        let known = match known {
            Some(index) => Some(index),
            None => inner.generalized(&key.0, &key.1),
        };
        let index = match known {
            Some(index) => {
                inner.index.entry(key).or_insert(index);
                index
            }
            None if inner.endpoints.len() >= MAX_ENDPOINTS => return None,
            None => {
                let (method, template) = key.clone();
                let host = |u: &str| url::Url::parse(u).ok().and_then(|u| u.host_str().map(site));
                let (own, page) = (host(url), host(document_url));
                let third_party = matches!((&own, &page), (Some(a), Some(b)) if a != b);
                let index = inner.endpoints.len();
                inner.endpoints.push(Endpoint {
                    id: format!("a{}", index + 1),
                    site: page.or(own).unwrap_or_default(),
                    mutating: is_mutating(&method, template.graphql.as_ref()),
                    method,
                    template,
                    third_party,
                    query_keys: BTreeSet::new(),
                    calls: 0,
                    last_status: None,
                    content_type: None,
                    request_shape: None,
                    response_shape: None,
                    triggers: Vec::new(),
                    sample: None,
                    learned: None,
                    documented: None,
                    persisted_calls: 0,
                });
                inner.index.insert(key, index);
                index
            }
        };
        inner.dirty.insert(index);
        let endpoint = &mut inner.endpoints[index];
        endpoint.calls += 1;
        endpoint.query_keys.extend(query_keys);
        if let Some(shape) = request_shape {
            endpoint.request_shape = Some(match endpoint.request_shape.take() {
                Some(existing) => existing.merge(shape),
                None => shape,
            });
        }
        Some(endpoint.id.clone())
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Endpoint)) {
        let mut inner = self.lock();
        if let Some(index) = inner.endpoints.iter().position(|e| e.id == id) {
            f(&mut inner.endpoints[index]);
            inner.dirty.insert(index);
        }
    }

    pub(crate) fn record_response(&self, id: &str, status: u16, content_type: Option<String>) {
        self.update(id, |e| {
            e.last_status = Some(status);
            if content_type.is_some() {
                e.content_type = content_type;
            }
        });
    }

    pub(crate) fn record_body(&self, id: &str, shape: Shape) {
        self.update(id, |e| {
            e.response_shape = Some(match e.response_shape.take() {
                Some(existing) => existing.merge(shape),
                None => shape,
            });
        });
    }

    pub(crate) fn record_sample(&self, id: &str, sample: Sample) {
        self.update(id, |e| e.sample = Some(sample));
    }

    /// Cookies arrive in a separate event, possibly after the sample was recorded.
    pub(crate) fn record_cookies(&self, id: &str, request_id: &str, cookies: &[(String, String)]) {
        self.update(id, |e| {
            if let Some(sample) = e.sample.as_mut().filter(|s| s.request_id == request_id) {
                sample.cookies = cookies.to_vec();
            }
        });
    }

    pub(crate) fn add_trigger(&self, id: &str, label: &str) {
        self.update(id, |e| add_trigger(&mut e.triggers, label));
    }

    pub fn get(&self, id: &str) -> Option<Endpoint> {
        self.lock().endpoints.iter().find(|e| e.id == id).cloned()
    }

    pub fn endpoints(&self) -> Vec<Endpoint> {
        self.lock().endpoints.clone()
    }
}

/// Appends `label` as the most recent trigger, keeping the last few distinct ones.
pub(crate) fn add_trigger(triggers: &mut Vec<String>, label: &str) {
    triggers.retain(|t| t != label);
    triggers.push(label.to_owned());
    if triggers.len() > MAX_TRIGGERS {
        triggers.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(method: &str, path: &str, summary: &str) -> DocumentedOp {
        DocumentedOp {
            method: method.into(),
            template: Template::documented("https://api.example.com/v1", path).unwrap(),
            doc: Documented {
                spec: "https://api.example.com/openapi.json".into(),
                summary: Some(summary.into()),
                params: Vec::new(),
                body: None,
            },
        }
    }

    #[test]
    fn calls_differing_in_a_segment_or_two_are_one_endpoint() {
        let catalog = Catalog::default();
        let page = "https://github.com/";
        let call = |url: &str| catalog.classify("GET", url, None, &[], page).unwrap();
        let tokio = call("https://github.com/tokio-rs/tokio/_sidebar");
        let reqwest = call("https://github.com/seanmonstar/reqwest/_sidebar");
        assert_eq!(tokio, reqwest);
        assert_eq!(call("https://github.com/hyperium/hyper/_sidebar"), tokio);
        let sidebar = catalog.get(&tokio).unwrap();
        assert_eq!(sidebar.template.path(), "/{p1}/{p2}/_sidebar");
        assert_eq!(sidebar.calls, 3);
        // Different resources stay apart.
        let issues = call("https://github.com/tokio-rs/tokio/issues/views.json");
        assert_ne!(issues, tokio);
        let users = call("https://api.example.com/v1/users");
        let orders = call("https://api.example.com/v1/orders");
        assert_ne!(users, orders);
    }

    #[test]
    fn learned_endpoints_differing_in_a_segment_or_two_are_one() {
        let earlier = |url: &str| {
            let catalog = Catalog::default();
            let id = catalog
                .classify("GET", url, None, &[], "https://github.com/")
                .unwrap();
            catalog.record_response(&id, 200, Some("application/json".into()));
            catalog.take_changes()
        };
        let catalog = Catalog::default();
        catalog.learn(earlier("https://github.com/tokio-rs/tokio/_sidebar"));
        catalog.learn(earlier("https://github.com/seanmonstar/reqwest/_sidebar"));
        let endpoints = catalog.endpoints();
        assert_eq!(endpoints.len(), 1, "{endpoints:?}");
        assert_eq!(endpoints[0].template.path(), "/{p1}/{p2}/_sidebar");
    }

    #[test]
    fn documentation_joins_what_was_seen() {
        let catalog = Catalog::default();
        let seen = catalog
            .classify(
                "GET",
                "https://api.example.com/v1/pets/8f3a9c2e41",
                None,
                &[],
                "https://example.com/",
            )
            .unwrap();
        catalog.document(
            "example.com",
            vec![
                op("GET", "/pets/{petId}", "Show a pet"),
                op("POST", "/pets", "Add a pet"),
            ],
        );
        let endpoints = catalog.endpoints();
        assert_eq!(
            endpoints.len(),
            2,
            "the seen endpoint gains its documentation"
        );
        let pet = catalog.get(&seen).unwrap();
        assert_eq!(
            pet.documented.unwrap().summary.as_deref(),
            Some("Show a pet")
        );
        let add = endpoints.iter().find(|e| e.method == "POST").unwrap();
        assert!(add.is_api() && add.calls == 0 && add.mutating);
        assert_eq!(add.template.path(), "/v1/pets");

        // Calling the documented endpoint is that endpoint, not a new one.
        let called = catalog
            .classify(
                "POST",
                "https://api.example.com/v1/pets",
                Some("{}"),
                &[],
                "https://example.com/",
            )
            .unwrap();
        assert_eq!(called, add.id);
        assert_eq!(catalog.endpoints().len(), 2);
    }

    #[test]
    fn a_varying_segment_is_not_a_documented_word() {
        let documented = Template::documented("https://api.example.com", "/users/search").unwrap();
        let seen = Template::of("https://api.example.com/users/8f3a9c2e41", None).unwrap();
        assert!(!seen.fits(&documented));
        let documented = Template::documented("https://api.example.com", "/users/{id}").unwrap();
        assert!(seen.fits(&documented));
        let me = Template::of("https://api.example.com/users/me", None).unwrap();
        assert!(me.fits(&documented));

        // Both documented: a call to /users/me is the endpoint spelled out for it.
        let catalog = Catalog::default();
        catalog.document(
            "example.com",
            vec![
                op("GET", "/users/{id}", "A user"),
                op("GET", "/users/me", "You"),
            ],
        );
        let id = catalog
            .classify(
                "GET",
                "https://api.example.com/v1/users/me",
                None,
                &[],
                "https://example.com/",
            )
            .unwrap();
        let endpoint = catalog.get(&id).unwrap();
        assert_eq!(endpoint.documented.unwrap().summary.as_deref(), Some("You"));

        // Seen first, documented after: the same holds, and looking again adds nothing.
        let catalog = Catalog::default();
        let me = catalog
            .classify(
                "GET",
                "https://api.example.com/v1/users/me",
                None,
                &[],
                "https://example.com/",
            )
            .unwrap();
        for _ in 0..2 {
            catalog.document(
                "example.com",
                vec![
                    op("GET", "/users/{id}", "A user"),
                    op("GET", "/users/me", "You"),
                ],
            );
        }
        let summary = catalog.get(&me).unwrap().documented.unwrap().summary;
        assert_eq!(summary.as_deref(), Some("You"));
        assert_eq!(catalog.endpoints().len(), 2);
    }
}
