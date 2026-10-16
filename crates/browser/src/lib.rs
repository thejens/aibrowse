//! Browser lifecycle and page interaction: launching Chrome for Testing against a named
//! profile, attaching to its tab, and executing humanized input plans over CDP.

mod aim_cover;
mod aim_view;
mod calibrate;
mod downloads;
mod early_identity;
mod egress;
mod error;
mod frames;
mod identity;
mod isolated;
mod launch;
mod live_regions;
mod locate;
mod passkeys;
mod platform;
mod profile;
mod profile_prefs;
mod range_scripts;
mod ref_node;
mod settle;
mod tab;
mod tab_aim;
mod tab_files;
mod tab_input;
mod tab_keys;
mod tab_script;
mod tab_view;
mod targets;
mod webmcp;
mod webmcp_polyfill;

pub use downloads::{Download, DownloadState, Downloads, human_size};
pub use egress::Egress;
pub use error::{BrowserError, Result};
pub use frames::{ElementRef, FrameHandle};
pub use identity::Display;
pub use launch::{Browser, LaunchOptions};
pub use locate::find_chrome;
pub use profile::Profile;
pub use settle::Dialog;
pub use tab::{ActionResult, Tab, Viewport};
pub use tab_input::{FIELD_STAYED_EMPTY, Opened};
pub use tab_script::{ActionContext, World};
pub use targets::TabInfo;
pub use webmcp::WebMcpTool;
