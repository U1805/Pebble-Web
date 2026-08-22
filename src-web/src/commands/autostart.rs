//! Web counterpart of `src-tauri/src/commands/autostart.rs`.
//!
//! Browser deployments do not own an operating-system login autostart entry.
//! The shared frontend maps these desktop-only commands to the Web platform
//! noop boundary, so this module intentionally has no backend handler.
