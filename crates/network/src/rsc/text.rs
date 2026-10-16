//! What of a server-rendered element tree is worth an agent's attention: the text it
//! shows and the data in its props, not markup, styling or event wiring.

/// Visible text pieces kept per payload.
const MAX_PIECES: usize = 5_000;
/// Visible text characters kept per payload.
const MAX_CHARS: usize = 200_000;

/// Elements dropped with their subtree: icons, styles, scripts and document metadata
/// carry no data an agent reads.
pub(super) fn is_dropped_tag(tag: &str) -> bool {
    matches!(
        tag,
        "svg" | "path" | "style" | "script" | "link" | "meta" | "noscript" | "template" | "iframe"
    )
}

/// Props that are presentation or wiring rather than data: class names, inline styles,
/// event handlers, layout and image-loading hints.
pub(super) fn is_noise_prop(key: &str) -> bool {
    let handler = key.len() > 2
        && key.starts_with("on")
        && key[2..].starts_with(|c: char| c.is_ascii_uppercase());
    handler
        || matches!(
            key,
            "className"
                | "class"
                | "style"
                | "ref"
                | "key"
                | "tabIndex"
                | "role"
                | "hidden"
                | "aria-hidden"
                | "focusable"
                | "loading"
                | "decoding"
                | "fetchPriority"
                | "sizes"
                | "srcSet"
                | "crossOrigin"
                | "referrerPolicy"
                | "rel"
                | "target"
                | "width"
                | "height"
                | "viewBox"
                | "xmlns"
                | "fill"
                | "stroke"
                | "suppressHydrationWarning"
                | "dangerouslySetInnerHTML"
        )
}

/// The visible text, in document order, as runs of adjacent text joined together.
#[derive(Debug, Default)]
pub(super) struct TextSink {
    pieces: Vec<String>,
    run: String,
    chars: usize,
}

impl TextSink {
    /// Adds text to the current run (text nodes next to each other read as one).
    pub fn push(&mut self, text: &str) {
        if self.chars < MAX_CHARS {
            self.run.push_str(text);
        }
    }

    /// Ends the current run: an element boundary.
    pub fn end_run(&mut self) {
        let run = std::mem::take(&mut self.run);
        let text = run.split_whitespace().collect::<Vec<_>>().join(" ");
        let repeated = self.pieces.last().is_some_and(|last| *last == text);
        if text.is_empty() || repeated || self.pieces.len() >= MAX_PIECES {
            return;
        }
        self.chars += text.len();
        self.pieces.push(text);
    }

    pub fn into_pieces(mut self) -> Vec<String> {
        self.end_run();
        self.pieces
    }
}
