//! Sitemaps (sitemaps.org): the pages a site lists for search engines' crawlers. They
//! name pages its navigation hides, such as a help center's thousands of articles
//! behind a search box, so a goal's words can be looked up in them directly.
//!
//! A sitemap is a list of page URLs (`<urlset>`), or an index of further sitemaps
//! (`<sitemapindex>`), often gzipped. Large sites split theirs by section and language
//! into hundreds of files; only a few are read per lookup: those in the place the lookup
//! started from first (support.microsoft.com lists `accessibility_ar-sa_1.xml` before
//! `accessibility_en-us_1.xml`, and without that preference twelve files of other
//! languages filled the budget), then those whose names share the most words with what
//! is looked for.

use std::collections::{HashSet, VecDeque};
use std::io::Read;
use std::sync::Arc;

use flate2::read::GzDecoder;
use tokio::task::JoinSet;
use url::Url;

use crate::fetch::{Fetcher, Outcome};

/// Sitemap files read per lookup, and how many at once.
const MAX_FILES: usize = 12;
const PARALLEL: usize = 4;
/// Bytes read per file, compressed, and at most this much once unpacked (the protocol's
/// own limit is 50 MB unpacked).
const FILE_CAP: usize = 10 << 20;
const UNPACKED_CAP: u64 = 50 << 20;
/// Page URLs kept per lookup.
const MAX_URLS: usize = 300_000;
const ACCEPT: &str = "application/xml, text/xml;q=0.9, */*;q=0.5";

/// What one sitemap file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sitemap {
    Pages(Vec<String>),
    Index(Vec<String>),
}

/// The pages found for a lookup.
#[derive(Debug, Default, Clone)]
pub struct SitePages {
    /// Page URLs on the site, in the order the sitemaps list them.
    pub urls: Vec<String>,
    /// The sitemap files read.
    pub files: Vec<String>,
}

/// Reads the sitemaps of `page`'s origin (those robots.txt `declared`, and
/// `/sitemap.xml`) and returns the pages they list on the same site. Of an index's
/// sitemaps, those naming `page`'s place, then those sharing most `words` with their
/// address, are read first. `load` reads one file, through a cache.
pub(crate) async fn site_pages<F, Fut>(
    page: &Url,
    declared: &[String],
    words: &[String],
    load: F,
) -> SitePages
where
    F: Fn(Url) -> Fut,
    Fut: Future<Output = Option<Arc<Sitemap>>> + Send + 'static,
{
    let mut queue: VecDeque<Url> = declared.iter().filter_map(|d| Url::parse(d).ok()).collect();
    // The conventional place too: robots.txt often declares only some sitemaps (a
    // bank's branch locator and videos), leaving out the main one.
    if let Ok(default) = page.join("/sitemap.xml")
        && !queue.contains(&default)
    {
        queue.push_back(default);
    }
    let site = site_of(page.host_str().unwrap_or_default());
    let place = place_of(page);
    let mut seen: HashSet<Url> = HashSet::new();
    let mut found = SitePages::default();
    while !queue.is_empty() && found.files.len() < MAX_FILES && found.urls.len() < MAX_URLS {
        let mut batch = JoinSet::new();
        while batch.len() < PARALLEL && found.files.len() + batch.len() < MAX_FILES {
            let Some(url) = queue.pop_front() else {
                break;
            };
            if seen.insert(url.clone()) {
                let file = load(url.clone());
                batch.spawn(async move { (url, file.await) });
            }
        }
        let mut children: Vec<Url> = Vec::new();
        let tried = batch.len();
        let mut loaded = 0;
        while let Some(Ok((url, sitemap))) = batch.join_next().await {
            found.files.push(url.to_string());
            loaded += usize::from(sitemap.is_some());
            match sitemap.as_deref() {
                Some(Sitemap::Pages(pages)) => {
                    let room = MAX_URLS.saturating_sub(found.urls.len());
                    found.urls.extend(
                        pages
                            .iter()
                            .filter(|page| on_site(page, &site))
                            .take(room)
                            .cloned(),
                    );
                }
                Some(Sitemap::Index(sitemaps)) => {
                    children.extend(sitemaps.iter().filter_map(|s| Url::parse(s).ok()));
                }
                None => {}
            }
        }
        // A batch of several files of which none loaded: the site's sitemaps are
        // failing (developer.android.com's answer 500 after 17 s each), and the rest of
        // them would only spend the lookup's time the same way.
        if tried > 1 && loaded == 0 && found.urls.is_empty() {
            break;
        }
        queue.extend(by_relevance(children, place.as_deref(), words));
    }
    found
}

/// An index's sitemaps: those naming `place` first, then those sharing most `words`
/// with their address; equally relevant ones keep the index's order.
fn by_relevance(mut sitemaps: Vec<Url>, place: Option<&str>, words: &[String]) -> Vec<Url> {
    sitemaps.sort_by_key(|s| {
        let in_place = place.is_some_and(|place| names_place(s.as_str(), place));
        std::cmp::Reverse((in_place, shared_words(s.as_str(), words)))
    });
    sitemaps
}

/// The section of the site `page` is in: the first segment of its path, which on sites
/// published in several languages is usually the locale (`/en-us/`, `/de/`). `None` at
/// the site's root.
fn place_of(page: &Url) -> Option<String> {
    page.path_segments()?
        .next()
        .filter(|segment| segment.chars().any(|c| c.is_alphabetic()))
        .map(str::to_lowercase)
}

/// Whether `address` has `place` as one of its words: `accessibility_en-us_1.xml` and
/// `/en-us/sitemap/index-0.xml` name `en-us`; `open-user.xml` doesn't.
fn names_place(address: &str, place: &str) -> bool {
    address
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .any(|word| word == place)
}

/// Fetches and parses one sitemap file; `None` when there is none or it isn't one.
pub(crate) async fn load(fetcher: &Fetcher, url: &Url) -> Option<Sitemap> {
    let Outcome::Found(fetched) = fetcher.get(url, ACCEPT, FILE_CAP).await else {
        return None;
    };
    if fetched.is_html() {
        return None;
    }
    parse(&unpack(&fetched.body)?)
}

/// The file's text, unpacking it if gzipped (by its bytes: servers label .xml.gz files
/// every which way).
fn unpack(body: &[u8]) -> Option<String> {
    if body.starts_with(&[0x1f, 0x8b]) {
        let mut text = String::new();
        GzDecoder::new(body)
            .take(UNPACKED_CAP)
            .read_to_string(&mut text)
            .ok()?;
        return Some(text);
    }
    Some(String::from_utf8_lossy(body).into_owned())
}

/// The `<loc>`s of a sitemap or sitemap index. Scanned rather than parsed as XML: a file
/// cut off at the size cap still yields every complete entry.
pub fn parse(text: &str) -> Option<Sitemap> {
    let index = text.contains("<sitemapindex");
    if !index && !text.contains("<urlset") {
        return None;
    }
    let mut locs = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<loc>") {
        rest = &rest[start + "<loc>".len()..];
        let Some(end) = rest.find("</loc>") else {
            break;
        };
        let loc = rest[..end].trim();
        let loc = loc
            .strip_prefix("<![CDATA[")
            .and_then(|l| l.strip_suffix("]]>"))
            .unwrap_or(loc)
            .trim();
        if !loc.is_empty() {
            locs.push(unescape(loc));
        }
        rest = &rest[end..];
    }
    Some(if index {
        Sitemap::Index(locs)
    } else {
        Sitemap::Pages(locs)
    })
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The site a host belongs to, for keeping pages on it: www.microsoft.com's sitemaps
/// list support.microsoft.com's pages too.
fn site_of(host: &str) -> String {
    host.strip_prefix("www.").unwrap_or(host).to_owned()
}

fn on_site(page: &str, site: &str) -> bool {
    Url::parse(page).ok().is_some_and(|url| {
        url.host_str()
            .is_some_and(|host| host == site || host.ends_with(&format!(".{site}")))
    })
}

/// How many of `words` occur in `text`, ignoring case.
pub fn shared_words(text: &str, words: &[String]) -> usize {
    let text = text.to_lowercase();
    words.iter().filter(|w| text.contains(w.as_str())).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_page_lists_and_indexes() {
        let pages = r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
            <url><loc>https://example.com/help/reset-password</loc></url>
            <url><loc> <![CDATA[https://example.com/a?b=1&c=2]]> </loc></url>
            <url><loc>https://example.com/x?y=1&amp;z=2</loc></url></urlset>"#;
        assert_eq!(
            parse(pages),
            Some(Sitemap::Pages(vec![
                "https://example.com/help/reset-password".into(),
                "https://example.com/a?b=1&c=2".into(),
                "https://example.com/x?y=1&z=2".into(),
            ]))
        );
        let index = r#"<sitemapindex><sitemap><loc>https://example.com/s1.xml.gz</loc></sitemap></sitemapindex>"#;
        assert_eq!(
            parse(index),
            Some(Sitemap::Index(vec!["https://example.com/s1.xml.gz".into()]))
        );
        assert_eq!(parse("<html><body>Not found</body></html>"), None);
        // Cut off at the cap: the complete entries still count.
        let cut = "<urlset><url><loc>https://example.com/a</loc></url><url><loc>https://exa";
        assert_eq!(
            parse(cut),
            Some(Sitemap::Pages(vec!["https://example.com/a".into()]))
        );
    }

    #[test]
    fn unpacks_gzip_by_its_bytes() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;
        let mut packed = GzEncoder::new(Vec::new(), Compression::default());
        packed
            .write_all(b"<urlset><url><loc>https://example.com/a</loc></url></urlset>")
            .unwrap();
        let text = unpack(&packed.finish().unwrap()).unwrap();
        assert!(text.contains("https://example.com/a"));
    }

    #[tokio::test]
    async fn follows_the_index_toward_the_words() {
        let origin = Url::parse("https://www.example.com/").unwrap();
        let load = |url: Url| async move {
            Some(Arc::new(match url.path() {
                "/sitemap.xml" => Sitemap::Index(vec![
                    "https://www.example.com/sitemap-blog.xml".into(),
                    "https://www.example.com/sitemap-help.xml".into(),
                ]),
                "/sitemap-help.xml" => Sitemap::Pages(vec![
                    "https://support.example.com/help/reset-password".into(),
                    "https://elsewhere.com/help/reset-password".into(),
                ]),
                _ => Sitemap::Pages(vec!["https://www.example.com/blog/launch".into()]),
            }))
        };
        let found = site_pages(&origin, &[], &["help".into()], load).await;
        assert_eq!(found.files.len(), 3);
        assert!(
            found
                .urls
                .contains(&"https://support.example.com/help/reset-password".into())
        );
        assert!(!found.urls.iter().any(|u| u.contains("elsewhere.com")));
    }

    #[tokio::test]
    async fn stops_when_a_whole_batch_fails() {
        let origin = Url::parse("https://developer.example.com/").unwrap();
        let load = |url: Url| async move {
            (url.path() == "/sitemap.xml").then(|| {
                Arc::new(Sitemap::Index(
                    (0..9)
                        .map(|i| format!("https://developer.example.com/sitemap_{i}_of_9.xml"))
                        .collect(),
                ))
            })
        };
        let found = site_pages(&origin, &[], &["permissions".into()], load).await;
        assert_eq!(found.files.len(), 1 + PARALLEL, "{:?}", found.files);
        assert!(found.urls.is_empty());
    }

    #[test]
    fn sitemaps_named_like_the_words_come_first() {
        let urls = [
            "https://e.com/sitemap-blog.xml",
            "https://e.com/sitemap-help-en.xml",
            "https://e.com/sitemap-news.xml",
        ]
        .map(|u| Url::parse(u).unwrap())
        .to_vec();
        let ordered = by_relevance(urls, None, &["help".into(), "en".into()]);
        assert_eq!(ordered[0].path(), "/sitemap-help-en.xml");
        assert_eq!(ordered[1].path(), "/sitemap-blog.xml");
    }

    #[test]
    fn sitemaps_in_the_place_looked_from_come_first() {
        let page = Url::parse("https://support.microsoft.com/en-us/windows").unwrap();
        let place = place_of(&page);
        assert_eq!(place.as_deref(), Some("en-us"));
        let urls = [
            "https://support.microsoft.com/_sitemaps/store_ar-sa_1.xml",
            "https://support.microsoft.com/_sitemaps/accessibility_ar-sa_1.xml",
            "https://support.microsoft.com/_sitemaps/accessibility_en-us_1.xml",
            "https://support.microsoft.com/en-us/sitemap/index-0.xml",
            "https://support.microsoft.com/_sitemaps/open-user.xml",
        ]
        .map(|u| Url::parse(u).unwrap())
        .to_vec();
        let ordered = by_relevance(urls, place.as_deref(), &["store".into()]);
        let names: Vec<&str> = ordered.iter().map(Url::path).collect();
        assert_eq!(
            names,
            [
                "/_sitemaps/accessibility_en-us_1.xml",
                "/en-us/sitemap/index-0.xml",
                "/_sitemaps/store_ar-sa_1.xml",
                "/_sitemaps/accessibility_ar-sa_1.xml",
                "/_sitemaps/open-user.xml",
            ]
        );
        assert_eq!(place_of(&Url::parse("https://e.com/").unwrap()), None);
        assert_eq!(place_of(&Url::parse("https://e.com/2024/x").unwrap()), None);
    }
}
