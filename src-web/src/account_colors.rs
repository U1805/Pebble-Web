use pebble_core::Account;
use std::collections::HashSet;

const ACCOUNT_COLOR_PRESETS: [&str; 12] = [
    "#0ea5e9", "#22c55e", "#f59e0b", "#8b5cf6", "#f43f5e", "#14b8a6", "#6366f1", "#f97316",
    "#06b6d4", "#ec4899", "#84cc16", "#3b82f6",
];

fn is_valid_hex_color(color: &str) -> bool {
    color.len() == 7
        && color.as_bytes()[0] == b'#'
        && color.as_bytes()[1..].iter().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn default_account_color(existing_accounts: &[Account], seed: &str) -> String {
    let used_colors: HashSet<String> = existing_accounts
        .iter()
        .filter_map(|account| account.color.as_deref())
        .filter(|color| is_valid_hex_color(color))
        .map(str::to_ascii_lowercase)
        .collect();

    if let Some(color) = ACCOUNT_COLOR_PRESETS
        .iter()
        .find(|color| !used_colors.contains(**color))
    {
        return (*color).to_string();
    }

    let mut hash = 0u32;
    for byte in seed.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(byte as u32);
    }

    ACCOUNT_COLOR_PRESETS[(hash as usize) % ACCOUNT_COLOR_PRESETS.len()].to_string()
}
