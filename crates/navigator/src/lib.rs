//! The goal navigator (docs/design.md §5.5): given a goal such as "take me to the terms
//! and conditions", it steps through the site with fast decisions from Jev, a decision
//! model, and hands control back as soon as someone else must act:
//!
//! - **the user**, for what only they can do: logging in, 2FA, CAPTCHAs, payments;
//! - **the agent**, when text must be written or a judgment made (Jev decides, it doesn't
//!   write);
//! - or when the goal is reached, or nothing on the page helps.

mod candidates;
mod consent;
mod digest;
mod fanout;
mod jev;
mod judge;
mod loops;
mod parts;
mod pursue;
mod pursuit;
mod search;
mod site_search;
mod survey;
mod today;
mod words;

pub use consent::decline_button;
pub use digest::{DEFAULT_DIGEST_TOKENS, digest};
pub use fanout::{Branch, Collect, FanOut, Found, fan_out};
pub use jev::{Jev, JevError};
pub use judge::{Agreements, CONSEQUENTIAL, consequential};
pub use parts::pursue_parts;
pub use pursue::{Decision, Limits, Outcome, PartOf, Report, Step, Workspace, pursue};
pub use pursuit::{Link, Opening};
pub use search::{Engine, Hit, MAX_PAGES, Miss, SearchReport, search};
pub use survey::{Ask, Filtering, Finding, MAX_SITES, SurveyReport, survey};
