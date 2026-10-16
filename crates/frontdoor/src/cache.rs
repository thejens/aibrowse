//! The per-session record of what each origin publishes for agents.
//!
//! Each origin is probed at most once per freshness period: the first request starts the
//! probe in the background, later ones share its result (or wait for it). Misses are
//! cached like hits. A `SiteStore` can persist results across sessions (the Site
//! Knowledge Base); without one, the record lives in memory.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::OnceCell;
use url::Url;

use crate::api_catalog::ApiLink;
use crate::doc::{Document, fetch_document};
use crate::fetch::Fetcher;
use crate::fetch::Outcome;
use crate::openapi::{self, ApiSpec};
use crate::probe::{SiteInfo, probe};
use crate::sitemap::{self, SitePages, Sitemap};

/// Sitemap files kept, and for how long: a lookup reads a few of a site's files, and the
/// next goal on the same site mostly reads the same ones.
const SITEMAP_CACHE_SIZE: usize = 64;
const SITEMAP_TTL: Duration = Duration::from_secs(30 * 60);

/// Bytes read of an API description: large APIs' run to a few megabytes.
const SPEC_CAP: usize = 8 << 20;

/// Documents kept for paging through with `site_doc`, and for how long.
const DOC_CACHE_SIZE: usize = 8;
const DOC_TTL: Duration = Duration::from_secs(10 * 60);

/// Durable storage for probe results, so a later session doesn't re-probe. The Site
/// Knowledge Base implements it.
pub trait SiteStore: Send + Sync {
    /// The last stored result for `origin`, fresh or not.
    fn load(&self, origin: &str) -> Option<SiteInfo>;
    fn save(&self, info: &SiteInfo);
}

/// Shared handle; clones see the same record.
#[derive(Clone)]
pub struct FrontDoors {
    inner: Arc<Inner>,
}

type Slot = Arc<OnceCell<Arc<SiteInfo>>>;
type SpecSlot = Arc<OnceCell<Option<Arc<ApiSpec>>>>;
/// When a sitemap file was read, its address, and what it held.
type CachedSitemap = (Instant, Url, Option<Arc<Sitemap>>);

struct Inner {
    fetcher: Fetcher,
    store: Option<Arc<dyn SiteStore>>,
    sites: Mutex<HashMap<String, Slot>>,
    /// API description links found in the origin's pages.
    page_links: Mutex<HashMap<String, Vec<ApiLink>>>,
    /// Recently read documents: when, the URL asked for, the document.
    docs: Mutex<Vec<(Instant, Url, Arc<Document>)>>,
    /// API descriptions by URL, each read once per session (`None`: there was none).
    specs: Mutex<HashMap<String, SpecSlot>>,
    /// Recently read sitemap files (`None`: there was none).
    sitemaps: Mutex<Vec<CachedSitemap>>,
}

impl FrontDoors {
    pub fn new(fetcher: Fetcher) -> Self {
        Self::build(fetcher, None)
    }

    pub fn with_store(fetcher: Fetcher, store: Arc<dyn SiteStore>) -> Self {
        Self::build(fetcher, Some(store))
    }

    fn build(fetcher: Fetcher, store: Option<Arc<dyn SiteStore>>) -> Self {
        Self {
            inner: Arc::new(Inner {
                fetcher,
                store,
                sites: Mutex::default(),
                page_links: Mutex::default(),
                docs: Mutex::default(),
                sitemaps: Mutex::default(),
                specs: Mutex::default(),
            }),
        }
    }

    /// Starts probing `origin` in the background unless its record is fresh or a probe
    /// is already running. Cheap; call it as early as a navigation is known.
    pub fn prefetch(&self, origin: &Url) {
        self.slot(origin);
    }

    /// The record for `origin` if the probe has finished; otherwise starts it (if
    /// needed) and returns `None` without waiting.
    pub fn peek(&self, origin: &Url) -> Option<Arc<SiteInfo>> {
        self.slot(origin).get().cloned()
    }

    /// The record for `origin`, waiting for the probe if it is still running.
    pub async fn get(&self, origin: &Url) -> Arc<SiteInfo> {
        let slot = self.slot(origin);
        let inner = Arc::clone(&self.inner);
        let origin = origin.clone();
        slot.get_or_init(|| run(inner, origin)).await.clone()
    }

    /// Remembers API description links seen in a page of `origin`.
    pub fn add_page_links(&self, origin: &Url, links: Vec<ApiLink>) {
        if links.is_empty() {
            return;
        }
        let mut all = lock(&self.inner.page_links);
        let known = all.entry(key(origin)).or_default();
        for link in links {
            if !known.contains(&link) {
                known.push(link);
            }
        }
    }

    pub fn page_links(&self, origin: &Url) -> Vec<ApiLink> {
        lock(&self.inner.page_links)
            .get(&key(origin))
            .cloned()
            .unwrap_or_default()
    }

    /// A text document for `site_doc`, kept briefly so paging through it doesn't
    /// refetch it.
    pub async fn document(&self, url: &Url) -> Result<Arc<Document>, String> {
        {
            let mut docs = lock(&self.inner.docs);
            docs.retain(|(at, _, _)| at.elapsed() < DOC_TTL);
            if let Some((_, _, doc)) = docs.iter().find(|(_, requested, _)| requested == url) {
                return Ok(Arc::clone(doc));
            }
        }
        let doc = Arc::new(fetch_document(&self.inner.fetcher, url).await?);
        let mut docs = lock(&self.inner.docs);
        if docs.len() >= DOC_CACHE_SIZE {
            docs.remove(0);
        }
        docs.push((Instant::now(), url.clone(), Arc::clone(&doc)));
        Ok(doc)
    }

    /// The API descriptions found at `candidates` (see `openapi::candidates`), read
    /// concurrently and once per session each, misses included.
    pub async fn api_specs(&self, candidates: &[Url]) -> Vec<Arc<ApiSpec>> {
        let mut reads = tokio::task::JoinSet::new();
        for url in candidates {
            let slot = {
                let mut specs = lock(&self.inner.specs);
                Arc::clone(specs.entry(url.to_string()).or_default())
            };
            let inner = Arc::clone(&self.inner);
            let url = url.clone();
            reads.spawn(async move { slot.get_or_init(|| read_spec(inner, url)).await.clone() });
        }
        let mut found: Vec<Arc<ApiSpec>> = Vec::new();
        while let Some(read) = reads.join_next().await {
            if let Ok(Some(spec)) = read
                // Two addresses may serve the same description (a redirect).
                && !found.iter().any(|f| f.url == spec.url)
            {
                found.push(spec);
            }
        }
        found
    }

    /// The pages the sitemaps of `page`'s site list, reading first the sitemaps in the
    /// place `page` is in, then those whose names share most of `words` (see
    /// sitemap.rs). Nothing for an address that isn't a web page's.
    pub async fn site_pages(&self, page: &str, words: &[String]) -> SitePages {
        let (Some(origin), Ok(page)) = (origin_of(page), Url::parse(page)) else {
            return SitePages::default();
        };
        let declared = self
            .get(&origin)
            .await
            .robots
            .as_ref()
            .map(|r| r.sitemaps.clone())
            .unwrap_or_default();
        let inner = Arc::clone(&self.inner);
        let load = move |url: Url| {
            let inner = Arc::clone(&inner);
            async move {
                {
                    let mut cached = lock(&inner.sitemaps);
                    cached.retain(|(at, _, _)| at.elapsed() < SITEMAP_TTL);
                    if let Some((_, _, sitemap)) = cached.iter().find(|(_, u, _)| *u == url) {
                        return sitemap.clone();
                    }
                }
                let sitemap = sitemap::load(&inner.fetcher, &url).await.map(Arc::new);
                let mut cached = lock(&inner.sitemaps);
                if cached.len() >= SITEMAP_CACHE_SIZE {
                    cached.remove(0);
                }
                cached.push((Instant::now(), url, sitemap.clone()));
                sitemap
            }
        };
        sitemap::site_pages(&page, &declared, words, load).await
    }

    /// The origin's slot, replaced when its record has gone stale. A new slot starts its
    /// probe right away.
    fn slot(&self, origin: &Url) -> Slot {
        let mut sites = lock(&self.inner.sites);
        let origin_key = key(origin);
        // A slot that exists but isn't filled yet has its probe running.
        if let Some(slot) = sites.get(&origin_key)
            && slot.get().is_none_or(|info| info.is_fresh())
        {
            return Arc::clone(slot);
        }
        let slot = Slot::default();
        sites.insert(origin_key, Arc::clone(&slot));
        let task_slot = Arc::clone(&slot);
        let inner = Arc::clone(&self.inner);
        let origin = origin.clone();
        tokio::spawn(async move {
            task_slot.get_or_init(|| run(inner, origin)).await;
        });
        slot
    }
}

async fn read_spec(inner: Arc<Inner>, url: Url) -> Option<Arc<ApiSpec>> {
    let accept = "application/json, application/yaml;q=0.9, */*;q=0.5";
    let Outcome::Found(fetched) = inner.fetcher.get(&url, accept, SPEC_CAP).await else {
        return None;
    };
    if fetched.is_html() || fetched.truncated {
        return None;
    }
    openapi::read(&fetched.url, &fetched.body).map(Arc::new)
}

/// Probes, or reuses what the store has if it is still fresh.
async fn run(inner: Arc<Inner>, origin: Url) -> Arc<SiteInfo> {
    let origin_key = key(&origin);
    if let Some(stored) = inner.store.as_ref().and_then(|s| s.load(&origin_key))
        && stored.is_fresh()
    {
        return Arc::new(stored);
    }
    let info = probe(&inner.fetcher, &origin).await;
    tracing::debug!(origin = %origin_key, interfaces = info.has_interfaces(), "probed front doors");
    if let Some(store) = &inner.store {
        store.save(&info);
    }
    Arc::new(info)
}

/// The origin of an http(s) URL ("https://example.com"), or `None` for other schemes
/// (about:, data:, chrome-error:, file:).
pub fn origin_of(url: &str) -> Option<Url> {
    let url = Url::parse(url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    Url::parse(&url.origin().ascii_serialization()).ok()
}

fn key(origin: &Url) -> String {
    origin.origin().ascii_serialization()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::origin_of;

    #[test]
    fn origins_are_scheme_host_port() {
        assert_eq!(
            origin_of("https://Example.com/a/b?c#d").unwrap().as_str(),
            "https://example.com/"
        );
        assert_eq!(
            origin_of("http://localhost:8080/x").unwrap().as_str(),
            "http://localhost:8080/"
        );
        assert!(origin_of("about:blank").is_none());
        assert!(origin_of("chrome-error://chromewebdata/").is_none());
    }
}
