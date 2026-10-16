//! What the knowledge base keeps about an endpoint, and the privacy rules deciding it.
//!
//! Only structure is persisted: method, template, query keys, shapes, flags, counts.
//! Never samples, header or cookie values, bodies, or query values. Anything structural
//! that could still carry the user's data is reduced or dropped here:
//! - endpoints whose template literals (path words, selector values, GraphQL operation)
//!   aren't plain identifiers are not persisted at all (`/search/ada%20lovelace`);
//! - query keys and JSON field names that look like data (ids, emails, URNs) are dropped;
//! - trigger labels keep only the action and the element's role (`click button`): the
//!   element's name is page text, often personal (`button "Message Ada Lovelace"`);
//! - of the headers, only the names of those filled from a cookie (CSRF) are kept,
//!   with the cookie's name; their values are read from the jar at call time.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::call::{ReplayHeader, replay_headers};
use crate::catalog::{Endpoint, Learned, add_trigger};
use crate::shape::Shape;
use crate::template::{Segment, Template, is_variable};

/// Longest literal kept: longer ones are tokens or text, not names.
const MAX_TOKEN: usize = 64;
/// Longest kept trigger (`click button`).
const MAX_TRIGGER: usize = 40;

/// An endpoint's persistable structure.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Record {
    pub site: String,
    pub method: String,
    pub template: Template,
    pub mutating: bool,
    pub third_party: bool,
    pub query_keys: BTreeSet<String>,
    pub content_type: Option<String>,
    pub request_shape: Option<Shape>,
    pub response_shape: Option<Shape>,
    /// Reduced to action and role (`click button`).
    pub triggers: Vec<String>,
    pub headers: Vec<CookieHeader>,
    pub with_cookies: bool,
    /// When saving, the calls since the last save; when loaded, all calls so far.
    pub calls: u64,
    /// Unix seconds; set by the store.
    pub last_seen: i64,
}

/// A request header whose value the site's code copies from a cookie.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CookieHeader {
    pub name: String,
    pub cookie: String,
    pub quoted: bool,
}

impl Record {
    /// The persistable form of `endpoint`, or `None` if it isn't an API (yet) or its
    /// template could carry the user's data.
    pub(crate) fn of(endpoint: &Endpoint) -> Option<Self> {
        if !endpoint.is_api() || !persistable(&endpoint.template) {
            return None;
        }
        let (headers, with_cookies) = match (&endpoint.sample, &endpoint.learned) {
            (Some(sample), _) => (
                cookie_headers(&replay_headers(&sample.headers, &sample.cookies)),
                !sample.cookies.is_empty(),
            ),
            (None, Some(learned)) => (cookie_headers(&learned.headers), learned.with_cookies),
            (None, None) => (Vec::new(), false),
        };
        let mut triggers = Vec::new();
        for trigger in endpoint.triggers.iter().filter_map(|t| reduce_trigger(t)) {
            add_trigger(&mut triggers, trigger);
        }
        Some(Self {
            site: endpoint.site.clone(),
            method: endpoint.method.clone(),
            template: endpoint.template.clone(),
            mutating: endpoint.mutating,
            third_party: endpoint.third_party,
            query_keys: endpoint
                .query_keys
                .iter()
                .filter(|k| is_field_name(k))
                .cloned()
                .collect(),
            content_type: endpoint.content_type.as_deref().map(media_type),
            request_shape: endpoint.request_shape.as_ref().map(sanitized),
            response_shape: endpoint.response_shape.as_ref().map(sanitized),
            triggers,
            headers,
            with_cookies,
            calls: u64::from(endpoint.calls.saturating_sub(endpoint.persisted_calls)),
            last_seen: 0,
        })
    }

    /// The stored record updated with what a session learned since (`newer`).
    pub(crate) fn absorb(mut self, newer: Record) -> Record {
        for trigger in &newer.triggers {
            add_trigger(&mut self.triggers, trigger);
        }
        self.query_keys.extend(newer.query_keys);
        Record {
            request_shape: merge(self.request_shape, newer.request_shape),
            response_shape: merge(self.response_shape, newer.response_shape),
            content_type: newer.content_type.or(self.content_type),
            // The latest request is authoritative for how the site authenticates.
            headers: if newer.headers.is_empty() {
                self.headers
            } else {
                newer.headers
            },
            with_cookies: newer.with_cookies || self.with_cookies,
            calls: self.calls + newer.calls,
            mutating: newer.mutating,
            third_party: newer.third_party,
            ..self
        }
    }

    /// A catalog entry for an endpoint this session hasn't seen yet.
    pub(crate) fn into_endpoint(self, id: String) -> Endpoint {
        let learned = self.learned();
        Endpoint {
            id,
            site: self.site,
            method: self.method,
            template: self.template,
            mutating: self.mutating,
            third_party: self.third_party,
            query_keys: self.query_keys,
            calls: 0,
            last_status: None,
            content_type: self.content_type,
            request_shape: self.request_shape,
            response_shape: self.response_shape,
            triggers: self.triggers,
            sample: None,
            learned: Some(learned),
            documented: None,
            persisted_calls: 0,
        }
    }

    /// Adds the earlier knowledge to an endpoint this session has already seen.
    pub(crate) fn merge_into(self, endpoint: &mut Endpoint) {
        endpoint.learned = Some(self.learned());
        endpoint.query_keys.extend(self.query_keys);
        endpoint.request_shape = merge(endpoint.request_shape.take(), self.request_shape);
        endpoint.response_shape = merge(endpoint.response_shape.take(), self.response_shape);
        if endpoint.content_type.is_none() {
            endpoint.content_type = self.content_type;
        }
        // This session's labels name the element; the stored ones only its role.
        if endpoint.triggers.is_empty() {
            endpoint.triggers = self.triggers;
        }
    }

    fn learned(&self) -> Learned {
        Learned {
            calls: self.calls,
            last_seen: self.last_seen,
            headers: self
                .headers
                .iter()
                .map(|h| ReplayHeader {
                    name: h.name.clone(),
                    value: String::new(),
                    cookie: Some(h.cookie.clone()),
                    quoted: h.quoted,
                })
                .collect(),
            with_cookies: self.with_cookies,
        }
    }
}

fn merge(a: Option<Shape>, b: Option<Shape>) -> Option<Shape> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.merge(b)),
        (a, b) => a.or(b),
    }
}

/// Whether every literal in the template is a plain identifier, as API names are.
/// Anything else (spaces, encoded text, long tokens) could be the user's data.
fn persistable(template: &Template) -> bool {
    template.segments.iter().all(|s| match s {
        Segment::Literal(word) => is_token(word),
        Segment::Param(_) => true,
    }) && template
        .selectors
        .iter()
        .all(|(k, v)| is_token(k) && is_token(v))
        && template
            .graphql
            .as_ref()
            .is_none_or(|g| is_token(&g.operation) && is_token(&g.kind))
}

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_TOKEN
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.~".contains(&b))
}

/// A field or query key that names something, as opposed to a map key carrying data
/// (`{"ada@example.com": …}`, `{"urn:li:member:123": …}`). Allows the sigils JSON
/// dialects use (`$type`, `*elements`, `@context`).
fn is_field_name(key: &str) -> bool {
    let name = key.trim_start_matches(['$', '*', '@']);
    is_token(name) && !is_variable(name)
}

/// `shape` without field names that look like data.
fn sanitized(shape: &Shape) -> Shape {
    match shape {
        Shape::Object(fields) => Shape::Object(
            fields
                .iter()
                .filter(|(key, _)| is_field_name(key))
                .map(|(key, field)| (key.clone(), sanitized(field)))
                .collect(),
        ),
        Shape::Array(element) => Shape::Array(element.as_deref().map(|e| Box::new(sanitized(e)))),
        Shape::Nullable(inner) => Shape::Nullable(Box::new(sanitized(inner))),
        other => other.clone(),
    }
}

/// `click button "Message Ada Lovelace"` → `click button`.
fn reduce_trigger(label: &str) -> Option<&str> {
    let head = label.split('"').next().unwrap_or_default().trim();
    let plain = head
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b" _-".contains(&b));
    (!head.is_empty() && head.len() <= MAX_TRIGGER && plain).then_some(head)
}

fn cookie_headers(headers: &[ReplayHeader]) -> Vec<CookieHeader> {
    headers
        .iter()
        .filter_map(|h| {
            let cookie = h.cookie.as_ref()?;
            (is_token(&h.name) && is_token(cookie)).then(|| CookieHeader {
                name: h.name.clone(),
                cookie: cookie.clone(),
                quoted: h.quoted,
            })
        })
        .collect()
}

/// `application/json; charset=utf-8` → `application/json`.
fn media_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}
