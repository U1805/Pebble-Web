//! Web counterpart of `src-tauri/src/commands/notifications.rs`.
//!
//! Desktop notification/tray operations are platform-specific. Browser notification
//! behavior is implemented by the frontend Web platform adapter, so this module
//! intentionally contains no command handlers. Keeping the counterpart here makes
//! upstream notification changes visible during structure review.
