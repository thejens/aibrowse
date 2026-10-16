//! Fetching llms.txt-style documents for reading (`site_doc`).

use url::Url;

use crate::fetch::{Fetcher, Outcome};
use crate::text::clean_document;

/// Largest document read; llms-full.txt files run to megabytes, the agent pages through.
const DOC_CAP: usize = 2 * 1024 * 1024;

/// A cleaned text document, ready to paginate.
#[derive(Debug, Clone)]
pub struct Document {
    /// Where it came from, after redirects.
    pub url: String,
    pub text: String,
    /// Only the first `DOC_CAP` bytes were read.
    pub truncated: bool,
}

/// Fetches `url` as a text document. Errors are messages for the agent.
pub async fn fetch_document(fetcher: &Fetcher, url: &Url) -> Result<Document, String> {
    let accept = "text/markdown, text/plain;q=0.9, application/json;q=0.5, */*;q=0.1";
    let fetched = match fetcher.get(url, accept, DOC_CAP).await {
        Outcome::Found(fetched) => fetched,
        Outcome::Missing => return Err(format!("{url} does not exist (the server said so)")),
        Outcome::Failed(reason) => return Err(format!("could not fetch {url}: {reason}")),
    };
    if fetched.is_html() {
        return Err(format!(
            "{} is an HTML page, not a text document; navigate to it and use read",
            fetched.url
        ));
    }
    let textual = fetched.content_type.as_deref().is_none_or(|t| {
        t.starts_with("text/") || t.contains("json") || t.contains("markdown") || t.contains("yaml")
    });
    if !textual || fetched.body.contains(&0) {
        return Err(format!(
            "{} is not a text document ({})",
            fetched.url,
            fetched.content_type.as_deref().unwrap_or("binary")
        ));
    }
    Ok(Document {
        url: fetched.url.to_string(),
        text: clean_document(&fetched.text()),
        truncated: fetched.truncated,
    })
}
