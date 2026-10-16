//! Reading response bodies: JSON behind anti-hijacking prefixes, and size caps that
//! respect character boundaries.

use serde_json::Value;

/// Parses a JSON response, tolerating the anti-hijacking prefixes some sites put before
/// it (`)]}'`, `for (;;);`, `while(1);`).
pub(crate) fn parse_json(body: &str) -> Option<Value> {
    let trimmed = body.trim_start();
    let start = trimmed.find(['{', '[']).filter(|&i| i <= 16)?;
    serde_json::from_str(&trimmed[start..]).ok()
}

/// At most `max` bytes of `text`, cut at a character boundary.
pub(crate) fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
