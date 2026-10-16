//! Choosing files for upload, the way a person does: click what opens the file chooser,
//! then pick the files in it.
//!
//! File choosers are intercepted (see `FileChooser`), so the click is real, humanized
//! input, and "picking the files" is `DOM.setFileInputFiles` on the input behind the
//! chooser, which makes Chrome fire the `input` and `change` events a real selection
//! fires. Which files may be chosen is the caller's policy; this module only does it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tokio::time::{Instant, sleep};

use crate::ActionResult;
use crate::downloads::human_size;
use crate::error::{BrowserError, Result};
use crate::frames::ElementRef;
use crate::settle::{FileChooser, lock};
use crate::tab::{Tab, stale};

/// How long a click gets to open the file chooser. Pages that open it from a click
/// handler do so at once; the margin covers a slow renderer.
const CHOOSER_WAIT: Duration = Duration::from_secs(3);

/// What we know about an `<input type=file>` before using it.
struct FileInput {
    multiple: bool,
}

impl Tab {
    /// Chooses `files` (absolute paths of regular files) through `element`: either an
    /// `<input type=file>`, or a button or label that opens a file chooser.
    pub async fn upload(&mut self, element: ElementRef, files: &[PathBuf]) -> Result<ActionResult> {
        if files.is_empty() {
            return Err(BrowserError::Element("no files to upload".into()));
        }
        let label = self
            .element_label(element)
            .await
            .unwrap_or_else(|| "element".into());
        let input = self.file_input(element).await?;
        if input.as_ref().is_some_and(|i| !i.multiple) && files.len() > 1 {
            return Err(single_file(&label, files.len()));
        }
        let start = self.begin();
        lock(&self.activity).file_chooser = None;
        let hand = self.take_hand().await?;
        let clicked = self.click_element(element).await;
        drop(hand);
        match (clicked, input.is_some()) {
            (Ok(_), _) => match self.await_chooser().await {
                Some(chooser) => {
                    if !chooser.multiple && files.len() > 1 {
                        return Err(single_file(&label, files.len()));
                    }
                    // A chooser opened without an input (rare: some page-level APIs)
                    // has nothing to set files on, unless the ref was the input itself.
                    let target = match (chooser.backend, &input) {
                        (Some(backend), _) => ElementRef::new(chooser.target, backend),
                        (None, Some(_)) => element,
                        (None, None) => {
                            return Err(BrowserError::Element(format!(
                                "{label} opened a file chooser without a file input behind it; this kind of chooser isn't supported"
                            )));
                        }
                    };
                    self.set_files(target, files).await?;
                }
                None => {
                    return Err(BrowserError::Element(format!(
                        "clicking {label} did not open a file chooser (is it disabled?). Pass the ref of the file input, or of the button that opens it."
                    )));
                }
            },
            // A file input that can't be clicked: hidden (display:none, the usual way
            // styled upload buttons work), or visually hidden behind its label. A person
            // would click its label or button; the selection itself is the same.
            (Err(BrowserError::Element(_)), true) => self.set_files(element, files).await?,
            (Err(err), _) => return Err(err),
        }
        let chosen: Vec<String> = files.iter().map(|f| describe_file(f)).collect();
        let action = format!("upload to {label}");
        let notes = vec![format!("chose {}", chosen.join(", "))];
        self.finish(&action, start, Tab::settle_timeout(), notes)
            .await
    }

    /// The element as a file input, or `None` if it is something else.
    async fn file_input(&self, element: ElementRef) -> Result<Option<FileInput>> {
        let described = self
            .session_for(element.target)?
            .call(
                "DOM.describeNode",
                json!({ "backendNodeId": element.backend }),
            )
            .await
            .map_err(|_| stale(element))?;
        let node = &described["node"];
        let attrs: Vec<&str> = node["attributes"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let attr = |key: &str| {
            attrs
                .chunks(2)
                .find(|kv| kv[0].eq_ignore_ascii_case(key))
                .map(|kv| kv.get(1).copied().unwrap_or_default())
        };
        let is_file_input = node["localName"] == "input"
            && attr("type").is_some_and(|t| t.eq_ignore_ascii_case("file"));
        Ok(is_file_input.then(|| FileInput {
            multiple: attr("multiple").is_some(),
        }))
    }

    async fn await_chooser(&self) -> Option<FileChooser> {
        let deadline = Instant::now() + CHOOSER_WAIT;
        while Instant::now() < deadline {
            if let Some(chooser) = lock(&self.activity).file_chooser.take() {
                return Some(chooser);
            }
            sleep(Duration::from_millis(50)).await;
        }
        None
    }

    async fn set_files(&self, input: ElementRef, files: &[PathBuf]) -> Result<()> {
        let paths: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
        self.session_for(input.target)?
            .call(
                "DOM.setFileInputFiles",
                json!({ "files": paths, "backendNodeId": input.backend }),
            )
            .await
            .map_err(|err| {
                BrowserError::Element(format!("could not choose the files in {input}: {err}"))
            })?;
        Ok(())
    }
}

fn single_file(label: &str, count: usize) -> BrowserError {
    BrowserError::Element(format!(
        "{label} accepts a single file, but {count} were given; upload them one at a time"
    ))
}

/// `"report.pdf" (12.3 KB)`.
fn describe_file(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match std::fs::metadata(path) {
        Ok(meta) => format!("{name:?} ({})", human_size(meta.len())),
        Err(_) => format!("{name:?}"),
    }
}
