//! Looking a goal up across the whole site, for when the site's own navigation didn't
//! get there: many sites have no search box, or one that searches only part of them (a
//! shop's catalog, not its help pages). Two indexes cover all of a site's pages:
//!
//! 1. its sitemaps, which list its pages for crawlers: the goal's words are matched
//!    against their addresses, and Jev picks the likeliest from those;
//! 2. a web search confined to the site (`site:example.com words`).
//!
//! Either way the best few pages open side by side, like a fan-out, and one that
//! reaches the goal becomes the tab in front.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::fanout::{Collect, Found, fan_out};
use crate::jev::Jev;
use crate::pursue::{Decision, Limits, Workspace};
use crate::pursuit::{Link, Opening};
use crate::search::{Engine, results_page};

/// Pages opened at most per index: they come ranked for the goal, and past the first
/// few the words were the wrong ones.
const MAX_OPENED: usize = 3;
/// Web results that look this unlikely to fulfil the goal aren't opened.
const MIN_RESULT_FIT: f64 = 0.1;
/// Sitemap pages, known by their address alone, are opened only when Jev thinks it is
/// at least this likely the page: a sitemap holds every page on the site, close
/// matches included.
const MIN_ADDRESS_FIT: f64 = 0.4;
/// Sitemap pages whose addresses share most of the goal's words, judged by Jev in one
/// request.
const MAX_JUDGED: usize = 30;
/// Engines asked in turn, the next when one is resting after a challenge or shows one
/// (a headless browser often gets Google's). Not Bing: it treats every microsoft.com
/// page as its own, so a search of Microsoft's sites would come back empty.
const ENGINES: [Engine; 3] = [Engine::Google, Engine::DuckDuckGo, Engine::Brave];

/// The whole lookup's budget: it comes after the site's own navigation already spent
/// its own, and a lookup that finds nothing must not double the agent's wait. Reading
/// sitemaps gets part of it, and a web search starts only with enough left to open
/// what it finds.
const LOOKUP_TIME: Duration = Duration::from_secs(45);
const SITEMAP_TIME: Duration = Duration::from_secs(15);
const SEARCH_MIN_LEFT: Duration = Duration::from_secs(15);

/// What each opened page may spend: it was chosen for the goal, so it is the page or a
/// step or two from it.
const OPENED_STEPS: usize = 3;
const OPENED_TIME: Duration = Duration::from_secs(30);

pub(crate) struct SiteSearch {
    /// Where the page that reached the goal was found ("the site's sitemap"), or where
    /// it was looked for in vain.
    pub looked: Vec<String>,
    /// The page that reached the goal, its tab still open.
    pub found: Option<Found>,
    /// Pages opened that didn't, and why.
    pub tried: Vec<(String, String)>,
    /// The decisions made in pages that didn't, for the record.
    pub trail: Vec<Decision>,
    pub decisions: usize,
    pub notes: Vec<String>,
}

/// Looks `goal` up on `host`'s site by `words`: in its sitemaps, then with a web search.
/// The tab in front ends where it was.
pub(crate) async fn site_search<W: Workspace>(
    workspace: &mut W,
    jev: &Jev,
    goal: &str,
    words: &str,
    host: &str,
) -> SiteSearch {
    let mut out = SiteSearch {
        looked: Vec::new(),
        found: None,
        tried: Vec::new(),
        trail: Vec::new(),
        decisions: 0,
        notes: Vec::new(),
    };
    let deadline = Instant::now() + LOOKUP_TIME;
    let here = workspace.tab().url();
    let stems: Vec<String> = words.split_whitespace().filter_map(stem).collect();

    let reading = Instant::now();
    let pages = tokio::time::timeout(SITEMAP_TIME, workspace.site_pages(&here, &stems))
        .await
        .unwrap_or_default();
    tracing::debug!(pages = pages.len(), elapsed = ?reading.elapsed(), "sitemaps read");
    if !pages.is_empty() {
        out.looked
            .push(format!("the site's sitemap ({} pages)", pages.len()));
        let closest = closest(&pages, &stems, &here);
        match likeliest_addresses(jev, goal, &here, &closest).await {
            Ok(links) => {
                out.decisions += 1;
                explore(workspace, jev, goal, &links, deadline, &mut out).await;
                if out.found.is_some() {
                    out.looked = vec!["the site's sitemap".into()];
                    return out;
                }
            }
            Err(err) => out
                .notes
                .push(format!("could not judge the sitemap's pages: {err}")),
        }
    }

    // The whole site: www.microsoft.com's help is on support.microsoft.com.
    let domain = host.strip_prefix("www.").unwrap_or(host);
    let query = format!("site:{domain} {words}");
    for engine in ENGINES {
        if deadline.saturating_duration_since(Instant::now()) < SEARCH_MIN_LEFT {
            break;
        }
        let searched = format!("{} for \"{query}\"", engine.name());
        out.looked.push(searched.clone());
        let results = match results_page(workspace, jev, engine, &query, goal, Some(&here)).await {
            Ok(results) => results,
            Err(err) => {
                tracing::debug!(engine = engine.name(), %err, "site lookup search failed");
                out.notes.push(err);
                continue;
            }
        };
        out.decisions += 1;
        for link in &results {
            tracing::debug!(engine = engine.name(), link = %link.label, fit = link.confidence, "site lookup result");
        }
        let links: Vec<Link> = results
            .into_iter()
            .filter(|link| on_site(&link.label, domain) && link.confidence >= MIN_RESULT_FIT)
            .take(MAX_OPENED)
            .collect();
        explore(workspace, jev, goal, &links, deadline, &mut out).await;
        if out.found.is_some() {
            out.looked = vec![searched];
        }
        // Results came: another engine would find the same pages.
        break;
    }
    let _ = workspace.tab().navigate(&here).await;
    out
}

/// Opens `links` side by side and pursues `goal` in each; the first page to reach it
/// goes to `out.found`.
async fn explore<W: Workspace>(
    workspace: &mut W,
    jev: &Jev,
    goal: &str,
    links: &[Link],
    deadline: Instant,
    out: &mut SiteSearch,
) {
    let left = deadline.saturating_duration_since(Instant::now());
    if links.is_empty() || left.is_zero() {
        return;
    }
    let limits = Limits {
        max_steps: OPENED_STEPS,
        max_time: OPENED_TIME.min(left),
        // Pages an index named for the goal: open as branches, which don't guess.
        guesses: 0,
        agreements: workspace.agreements(),
        ..Limits::default()
    };
    let fanned = fan_out(workspace, jev, goal, &[], links, Collect::First, &limits).await;
    out.decisions += fanned.decisions;
    out.notes.extend(fanned.notes);
    // Only a page that is the goal counts: one that wants a sign-in or text is the
    // site's own navigation again, which already stopped short.
    for waiting in fanned.waiting {
        out.trail.extend(waiting.branch.trail.iter().cloned());
        out.tried.push((
            waiting.branch.link.label.clone(),
            waiting.branch.outcome.why(),
        ));
        if let Some(browser) = workspace.browser() {
            let _ = browser.close_tab(waiting.tab.target_id()).await;
        }
    }
    for branch in fanned.gave_up {
        out.tried
            .push((branch.link.label.clone(), branch.outcome.why()));
        out.trail.extend(branch.trail);
    }
    out.found = fanned.reached.into_iter().next();
}

/// A word as it is matched in addresses: long words by their start, so "reporting"
/// matches /report-fraud and "browser" /browsing-history. Very short words match
/// everywhere and are left out.
fn stem(word: &str) -> Option<String> {
    let word = word.to_lowercase();
    match word.chars().count() {
        0..=2 => None,
        3..=6 => Some(word),
        _ => Some(word.chars().take(6).collect()),
    }
}

/// The sitemap pages whose addresses share most of `stems`, at most `MAX_JUDGED`;
/// among equals, those in the place the lookup started from (the same first path
/// segment, usually the locale: `/en-us/` beside `/ar-sa/`), then shorter addresses (an
/// overview before its sub-pages).
fn closest<'p>(pages: &'p [String], stems: &[String], here: &str) -> Vec<&'p str> {
    let place = first_segment(here);
    let mut scored: Vec<(usize, bool, usize, &str)> = pages
        .iter()
        .filter_map(|page| {
            let path = page
                .split_once("://")
                .map_or(page.as_str(), |(_, rest)| rest);
            let path = path.to_lowercase();
            let shared = stems.iter().filter(|s| path.contains(s.as_str())).count();
            let in_place = place.is_some() && first_segment(page) == place;
            (shared > 0).then_some((shared, in_place, path.len(), page.as_str()))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    scored
        .into_iter()
        .take(MAX_JUDGED)
        .map(|(_, _, _, page)| page)
        .collect()
}

/// The first segment of `address`'s path, lowercased.
fn first_segment(address: &str) -> Option<String> {
    url::Url::parse(address)
        .ok()?
        .path_segments()?
        .next()
        .filter(|segment| !segment.is_empty())
        .map(str::to_lowercase)
}

/// Of `pages`, judged by their addresses alone in one request, those likely enough to
/// be the goal, likeliest first, as links to open by address. `here`, the page the
/// lookup started from, sets the language: a sitemap lists every translation of a page
/// (MDN's `/zh-CN/` beside `/en-US/`), and the main one isn't always first.
async fn likeliest_addresses(
    jev: &Jev,
    goal: &str,
    here: &str,
    pages: &[&str],
) -> Result<Vec<Link>, String> {
    if pages.is_empty() {
        return Ok(Vec::new());
    }
    let mut questions = serde_json::Map::new();
    for (i, page) in pages.iter().enumerate() {
        questions.insert(
            format!("page_{i}"),
            json!({
                "type": "noul",
                "instructions": format!("Judging by its address alone, is {page} the page `goal` asks for?"),
                "criteria": {
                    "true": "Likely: the address names what `goal` asks for",
                    "false": "Unlikely: it names something else, something broader, or a different language or region than `here`, the page the requester is on"
                }
            }),
        );
    }
    let answers = jev
        .ask(
            &json!({ "goal": goal, "here": here }),
            &Value::Object(questions),
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut links: Vec<Link> = pages
        .iter()
        .enumerate()
        .map(|(i, page)| Link {
            open: Opening::Address((*page).to_owned()),
            named_for_goal: true,
            label: format!("page {page}"),
            identity: (*page).to_owned(),
            confidence: answers[format!("page_{i}")]["noul"].as_f64().unwrap_or(0.0),
        })
        .filter(|link| link.confidence >= MIN_ADDRESS_FIT)
        .collect();
    links.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    links.truncate(MAX_OPENED);
    Ok(links)
}

/// Whether a result's link (`link "Title" -> support.microsoft.com/...`) is on `domain`
/// or one of its subdomains. A relative link is the engine's own.
fn on_site(label: &str, domain: &str) -> bool {
    let Some((name, target)) = label.rsplit_once(" -> ") else {
        return false;
    };
    if is_ad(target) {
        return false;
    }
    let in_domain = |address: &str| {
        let address = address
            .strip_prefix("https://")
            .or_else(|| address.strip_prefix("http://"))
            .unwrap_or(address);
        let host = address.split(['/', '?', '#']).next().unwrap_or_default();
        host.eq_ignore_ascii_case(domain) || host.to_lowercase().ends_with(&format!(".{domain}"))
    };
    if !target.starts_with('/') {
        return in_domain(target);
    }
    // A relative link is the engine's own: its navigation, or (Google) a result behind
    // an opaque `/goto?url=…` redirect. A result shows where it leads in its name
    // ("… https://support.microsoft.com › en-us"), and that address decides; a long name
    // is cut short in the outline, often inside the address ("https://developer.andro…"),
    // which then counts when it fits the domain as far as it goes.
    name.split_whitespace()
        .map(|word| word.trim_matches('"'))
        .filter(|word| word.contains('.'))
        .any(|word| match word.strip_suffix('…') {
            Some(cut) => cut_in_domain(cut, domain),
            None => in_domain(word),
        })
}

/// Whether a result's link is an engine's ad: an ad for the site is a landing page
/// bought for a campaign ("Trains to London"), not the page the index has for the goal.
/// An ad links through the engine's click counter, or straight to the site with its
/// click id (`gclid`, `msclkid`).
fn is_ad(target: &str) -> bool {
    const AD_LINKS: [&str; 7] = [
        "/aclk?",
        "/aclick?",
        "googleadservices.",
        "/pagead/",
        "y.js?ad_",
        "gclid=",
        "msclkid=",
    ];
    AD_LINKS.iter().any(|ad| target.contains(ad))
}

/// Whether `cut`, an address cut short, may be on `domain`: its host so far is the
/// domain's beginning ("developer.andro" for developer.android.com), or a subdomain's
/// whose rest begins the domain ("support.micro" for microsoft.com).
fn cut_in_domain(cut: &str, domain: &str) -> bool {
    let cut = cut
        .strip_prefix("https://")
        .or_else(|| cut.strip_prefix("http://"))
        .unwrap_or(cut)
        .to_lowercase();
    if cut.contains('/') {
        let host = cut.split('/').next().unwrap_or_default();
        return host == domain || host.ends_with(&format!(".{domain}"));
    }
    // At least the first label and a dot: "d…" alone could be any site.
    cut.contains('.')
        && std::iter::once(cut.as_str())
            .chain(cut.match_indices('.').map(|(i, _)| &cut[i + 1..]))
            .any(|rest| !rest.is_empty() && domain.starts_with(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ads_for_the_site_are_not_results() {
        assert!(!on_site(
            "link \"Trains to London Ad thetrainline.com\" -> /aclk?sa=l&ai=DChcSEw",
            "thetrainline.com"
        ));
        assert!(!on_site(
            "link \"Trainline\" -> https://www.googleadservices.com/pagead/aclk?sa=L",
            "thetrainline.com"
        ));
    }

    #[test]
    fn results_on_the_site_and_its_subdomains() {
        let link = |href: &str| format!("link \"Change default apps\" -> {href}");
        assert!(on_site(
            &link("support.microsoft.com/en-us/windows"),
            "microsoft.com"
        ));
        assert!(on_site(&link("https://microsoft.com/x"), "microsoft.com"));
        assert!(!on_site(&link("notmicrosoft.com/x"), "microsoft.com"));
        assert!(!on_site(&link("/search?q=x"), "microsoft.com"));
    }

    #[test]
    fn results_whose_shown_address_was_cut_short() {
        let google = |name: &str| format!("link \"{name}\" -> /goto?url=CAESkwEB6zsw");
        assert!(on_site(
            &google("Request location access Android Developers https://developer.andro…"),
            "developer.android.com"
        ));
        assert!(on_site(
            &google("Keyboard shortcuts Microsoft Support https://support.micro…"),
            "microsoft.com"
        ));
        assert!(!on_site(
            &google("Android tips Some Blog https://blog.exam…"),
            "developer.android.com"
        ));
        assert!(!on_site(&google("Tips https://e…"), "example.com"));
    }

    #[test]
    fn results_behind_redirects_by_the_address_they_show() {
        let google = |name: &str| format!("link \"{name}\" -> /goto?url=CAESkwEB6zsw");
        assert!(on_site(
            &google(
                "Keyboard shortcuts for Outlook Microsoft Support https://support.microsoft.com › ..."
            ),
            "microsoft.com"
        ));
        assert!(!on_site(
            &google("Outlook shortcuts Some Blog https://blog.example.com › outlook"),
            "microsoft.com"
        ));
        assert!(
            !on_site(
                &google("Keyboard Shortcuts in New Outlook Microsoft Learn"),
                "microsoft.com"
            ),
            "no address shown: it can't be told from the engine's own links"
        );
    }

    #[test]
    fn addresses_sharing_most_words_first() {
        let stems: Vec<String> = "change default browser windows"
            .split_whitespace()
            .filter_map(stem)
            .collect();
        assert_eq!(stems, ["change", "defaul", "browse", "window"]);
        let pages: Vec<String> = [
            "https://support.microsoft.com/en-us/windows/keyboard-shortcuts",
            "https://support.microsoft.com/en-us/windows/change-your-default-browser-in-windows-020c58c6",
            "https://support.microsoft.com/en-us/windows/change-default-apps-in-windows",
            "https://support.microsoft.com/en-us/office/print-a-document",
        ]
        .map(str::to_owned)
        .to_vec();
        let ranked = closest(&pages, &stems, "https://support.microsoft.com/");
        assert!(ranked[0].contains("change-your-default-browser"));
        assert!(ranked[1].contains("change-default-apps"));
        assert_eq!(ranked.len(), 3, "a page sharing no words isn't judged");
    }

    #[test]
    fn translations_come_after_the_page_in_the_place_looked_from() {
        let stems: Vec<String> = "store refund".split_whitespace().filter_map(stem).collect();
        let pages: Vec<String> = [
            "https://support.microsoft.com/ar-sa/account-billing/store-refund",
            "https://support.microsoft.com/en-us/account-billing/store-refund",
        ]
        .map(str::to_owned)
        .to_vec();
        let ranked = closest(&pages, &stems, "https://support.microsoft.com/en-us/");
        assert!(ranked[0].contains("/en-us/"));
    }
}
