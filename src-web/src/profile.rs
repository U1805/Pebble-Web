//! Web profile helpers.
//!
//! The current Web runtime uses one stable profile namespace. Multi-user
//! support can replace this value with a user-scoped namespace later.

pub const STORAGE_NAMESPACE: &str = "web";

pub fn storage_namespace() -> &'static str {
    STORAGE_NAMESPACE
}
