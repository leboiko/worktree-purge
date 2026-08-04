//! Human-readable formatting helpers for the TUI.

/// Format a byte count the way `du -h` would: three significant figures,
/// binary units.
///
/// ```
/// # use worktree_purge::format::bytes;
/// assert_eq!(bytes(0), "0 B");
/// assert_eq!(bytes(1024), "1.0 KB");
/// ```
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    // Keep the column narrow: one decimal below 10, none above.
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Render an optional byte count, using a placeholder while it is unknown.
pub fn bytes_opt(n: Option<u64>) -> String {
    match n {
        Some(v) => bytes(v),
        None => "—".to_string(),
    }
}

/// Pluralise a count: `count_noun(1, "worktree") == "1 worktree"`.
pub fn count_noun(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Shorten a path for display by replacing the home directory with `~`.
pub fn tilde(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    if let Some(home) = home_dir() {
        let home = home.to_string_lossy();
        if !home.is_empty() && text.starts_with(home.as_ref()) {
            return format!("~{}", &text[home.len()..]);
        }
    }
    text.into_owned()
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_uses_binary_units_with_narrow_columns() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1024), "1.0 KB");
        assert_eq!(bytes(1536), "1.5 KB");
        // At and above 10 the decimal is dropped to keep the column narrow.
        assert_eq!(bytes(10 * 1024), "10 KB");
        assert_eq!(bytes(1024 * 1024), "1.0 MB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn bytes_saturates_at_the_largest_unit() {
        // Must not panic or index past the unit table.
        let huge = u64::MAX;
        assert!(bytes(huge).ends_with(" PB"), "got {}", bytes(huge));
    }

    #[test]
    fn unknown_size_renders_as_placeholder_not_zero() {
        // Showing "0 B" for an uncomputed size would understate what the user
        // reclaims, so it must be visibly distinct.
        assert_eq!(bytes_opt(None), "—");
        assert_eq!(bytes_opt(Some(0)), "0 B");
    }

    #[test]
    fn count_noun_pluralises() {
        assert_eq!(count_noun(0, "worktree"), "0 worktrees");
        assert_eq!(count_noun(1, "worktree"), "1 worktree");
        assert_eq!(count_noun(2, "worktree"), "2 worktrees");
    }

    #[test]
    fn tilde_only_replaces_a_real_home_prefix() {
        // SAFETY: single-threaded test process mutation of an env var.
        unsafe { std::env::set_var("HOME", "/Users/example") };
        assert_eq!(
            tilde(std::path::Path::new("/Users/example/dev/repo")),
            "~/dev/repo"
        );
        // A path that merely starts with a similar string is untouched below
        // the home root.
        assert_eq!(
            tilde(std::path::Path::new("/opt/other")),
            "/opt/other".to_string()
        );
    }
}
