//! PDFs open in Chrome's viewer, whose page holds nothing but an `<embed>`: no text for
//! `read`, digests or the navigator's checks. The document is fetched again from inside
//! the page (so the user's login applies, and it usually comes from the cache) and its
//! text extracted here, page by page.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aibrowse_browser::{Result, Tab};
use base64::Engine;

/// Larger documents (scanned books, catalogs) are left to the viewer.
const MAX_BYTES: usize = 40 * 1024 * 1024;
/// Extraction runs on untrusted input; a document that takes longer is given up on.
const EXTRACT_TIME: Duration = Duration::from_secs(20);
/// Documents kept extracted, so paging through one with `read` fetches it once.
const CACHED: usize = 4;

/// Fetches the page's own document as base64, if the page is a PDF.
const FETCH: &str = r#"(async () => {
  if (document.contentType !== 'application/pdf') return null;
  try {
    const response = await fetch(location.href, { credentials: 'include' });
    if (!response.ok) return { error: `the server answered ${response.status}` };
    const declared = +response.headers.get('content-length') || 0;
    if (declared > MAX_BYTES) return { error: `it is ${Math.round(declared / 1048576)} MB` };
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (bytes.length > MAX_BYTES) return { error: `it is ${Math.round(bytes.length / 1048576)} MB` };
    let binary = '';
    for (let i = 0; i < bytes.length; i += 0x8000) {
      binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    }
    return { data: btoa(binary) };
  } catch (e) {
    return { error: String(e && e.message || e) };
  }
})()"#;

static EXTRACTED: Mutex<VecDeque<(String, Arc<Extracted>)>> = Mutex::new(VecDeque::new());

/// A document's text, or why it has none.
#[derive(Debug)]
enum Extracted {
    Pages(Vec<String>),
    Unreadable(String),
}

/// The text of the PDF the tab shows, as markdown with a heading per page; `None` when
/// the page isn't a PDF. A PDF whose text can't be had (too large, scanned, broken) says
/// why, so the agent turns to screenshots instead.
pub async fn pdf_text(tab: &mut Tab) -> Result<Option<String>> {
    let url = tab.url();
    let cached = lock()
        .iter()
        .find(|(u, _)| *u == url)
        .map(|(_, e)| Arc::clone(e));
    let extracted = match cached {
        Some(extracted) => extracted,
        None => {
            let script = FETCH.replace("MAX_BYTES", &MAX_BYTES.to_string());
            let fetched = tab.evaluate_isolated_raw(&script).await?;
            if fetched.is_null() {
                return Ok(None);
            }
            let extracted = Arc::new(
                match (fetched["data"].as_str(), fetched["error"].as_str()) {
                    (Some(data), _) => match base64::engine::general_purpose::STANDARD.decode(data)
                    {
                        Ok(bytes) => extract(bytes).await,
                        Err(_) => Extracted::Unreadable("it came back garbled".into()),
                    },
                    (None, Some(error)) => {
                        Extracted::Unreadable(format!("fetching it failed: {error}"))
                    }
                    (None, None) => Extracted::Unreadable("fetching it failed".into()),
                },
            );
            let mut cache = lock();
            if cache.len() >= CACHED {
                cache.pop_front();
            }
            cache.push_back((url.clone(), Arc::clone(&extracted)));
            extracted
        }
    };
    let title = tab.title().await;
    Ok(Some(markdown(&title, &url, &extracted)))
}

fn lock() -> std::sync::MutexGuard<'static, VecDeque<(String, Arc<Extracted>)>> {
    EXTRACTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Extracts on a blocking thread, guarded: the parser has seen less of the web's PDFs
/// than Chrome's, so a panic or a runaway document costs the text, not the server.
async fn extract(bytes: Vec<u8>) -> Extracted {
    let work = tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(&bytes))
    });
    match tokio::time::timeout(EXTRACT_TIME, work).await {
        Ok(Ok(Ok(Ok(pages)))) => Extracted::Pages(pages),
        Ok(Ok(Ok(Err(err)))) => {
            Extracted::Unreadable(format!("its text couldn't be extracted ({err})"))
        }
        Ok(Ok(Err(_)) | Err(_)) => Extracted::Unreadable("its text couldn't be extracted".into()),
        Err(_) => Extracted::Unreadable(format!(
            "extracting its text took over {}s",
            EXTRACT_TIME.as_secs()
        )),
    }
}

fn markdown(title: &str, url: &str, extracted: &Extracted) -> String {
    let name = if title.trim().is_empty() || title == url {
        url.rsplit('/').find(|s| !s.is_empty()).unwrap_or(url)
    } else {
        title
    };
    match extracted {
        Extracted::Unreadable(why) => format!(
            "# {name}\n\n(a PDF whose text couldn't be read: {why}. A screenshot shows its pages.)"
        ),
        Extracted::Pages(pages) => {
            let texts: Vec<String> = pages.iter().map(|p| tidy(p)).collect();
            if texts.iter().all(String::is_empty) {
                return format!(
                    "# {name}\n\n(a PDF of {} pages with no text layer, probably scanned. A screenshot shows its pages.)",
                    pages.len()
                );
            }
            let mut out = format!("# {name}\n\n(PDF, {} pages)\n", pages.len());
            for (i, text) in texts.iter().enumerate() {
                if !text.is_empty() {
                    out.push_str(&format!("\n## Page {}\n\n{text}\n", i + 1));
                }
            }
            out
        }
    }
}

/// Lines trimmed, runs of blank lines collapsed to one, and the page number printed at
/// the page's top or bottom left out: on its own it reads as a passage ("6").
fn tidy(page: &str) -> String {
    let mut lines: Vec<&str> = page
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let page_number = |l: &&str| l.len() <= 4 && l.chars().all(|c| c.is_ascii_digit());
    if lines.last().is_some_and(page_number) {
        lines.pop();
    }
    if lines.first().is_some_and(page_number) {
        lines.remove(0);
    }
    let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
        return String::new();
    };
    // Keep the page's own blank lines between its first and last text.
    let start = page.find(first).unwrap_or(0);
    let end = page.rfind(last).map_or(page.len(), |at| at + last.len());
    let page = &page[start..end];
    let mut out = String::with_capacity(page.len());
    let mut blank = true;
    for line in page.lines().map(str::trim) {
        if line.is_empty() {
            if !blank {
                out.push('\n');
            }
            blank = true;
        } else {
            out.push_str(line);
            out.push('\n');
            blank = false;
        }
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_get_headings_and_tidy_text() {
        let extracted = Extracted::Pages(vec![
            "  Attention Is All You Need \n\n\n\nAbstract\n".into(),
            "   \n".into(),
            "Results\n  BLEU 28.4\n\n3\n".into(),
            "\n4\n".into(),
        ]);
        assert_eq!(
            markdown("", "https://arxiv.org/pdf/1706.03762", &extracted),
            "# 1706.03762\n\n(PDF, 4 pages)\n\n## Page 1\n\nAttention Is All You Need\n\nAbstract\n\n## Page 3\n\nResults\nBLEU 28.4\n"
        );
    }

    #[test]
    fn a_pdf_without_text_says_so() {
        let scanned = Extracted::Pages(vec![String::new(), " \n".into()]);
        assert!(markdown("Scan", "https://x/s.pdf", &scanned).contains("no text layer"));
        let broken = Extracted::Unreadable("fetching it failed: 404".into());
        assert!(markdown("Doc", "https://x/d.pdf", &broken).contains("fetching it failed: 404"));
    }
}
