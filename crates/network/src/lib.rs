//! Network observation and API discovery (docs/design.md §5.4).
//!
//! Every tab records the requests its page makes. XHR and fetch calls are clustered into
//! endpoint templates with inferred request and response shapes, linked to the agent
//! action that triggered them, and can be called again from inside the page, which is
//! often far faster and more reliable than repeating the clicks. The structure of what
//! was learned persists per site in the knowledge base, for later sessions.

mod call;
mod capture;
mod catalog;
mod kb;
mod payload;
mod pick;
mod redact;
pub mod render;
pub mod rsc;
mod shape;
mod template;

pub use call::{
    ApiRequest, ApiResponse, MAX_FETCHED_CHARS, Overrides, ReplayHeader, fetch_script, plan,
    replay_for_endpoint, replay_headers, resolve_headers,
};
pub use capture::{Entry, NetworkLog};
pub use catalog::{Catalog, DocParam, Documented, DocumentedOp, Endpoint, Learned, Sample};
pub use kb::{forget_path, knowledge_base_path, learn_path, recall_path};
pub use pick::pick;
pub use redact::{REDACTED, is_secret_header};
pub use shape::Shape;
pub use template::{GraphQl, Segment, Template, site as site_of};
