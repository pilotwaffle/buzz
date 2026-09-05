//! Observer-feed archive default — explicit opt-in.
//!
//! Kind 24200 events can contain sensitive agent activity. They remain
//! ephemeral unless the operator explicitly enables local retention.

/// Returns `false`: fresh identities do not archive agent activity until the
/// operator explicitly opts in. SQLite subscription state is authoritative;
/// startup never recreates consent from browser storage.
#[tauri::command]
pub fn observer_archive_default_enabled() -> bool {
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn fresh_identities_default_to_no_observer_archive() {
        assert!(!super::observer_archive_default_enabled());
    }
}
