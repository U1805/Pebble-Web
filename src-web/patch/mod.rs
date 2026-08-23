//! Web-only compatibility fixes for behavior inherited from upstream.
//!
//! Keep these patches isolated from `src-tauri` and the shared crates so the
//! rest of the repository can continue tracking QingJ01/Pebble verbatim.

pub(crate) mod archive;
pub(crate) mod batch_delete;
pub(crate) mod drafts;
pub(crate) mod folders;
pub(crate) mod gmail_oauth;
#[path = "issue026_imap_move.rs"]
pub(crate) mod imap_move;
pub(crate) mod outgoing;
