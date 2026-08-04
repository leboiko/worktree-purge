//! Removing worktrees: planning, execution and reporting.
//!
//! Removal goes through `git worktree remove` so the repository's
//! `.git/worktrees/<name>` metadata is cleaned up too; a plain directory
//! delete would leave the repo referencing a worktree that no longer exists.
//!
//! This is the only module that destroys user data, so three rules hold
//! throughout: nothing here panics or unwraps a subprocess result, success is
//! never inferred from an exit code alone, and the one code path that deletes
//! a directory without git mediating is gated behind [`guard_orphan_path`].
//!
//! See `docs/notes-purge.md` for the empirical git behaviour these decisions
//! are built on.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::git;
use crate::model::{PurgeItem, PurgeOutcome, SizeState};
use crate::scan::size;

/// How many `Normal` path components an orphan path must have below the
/// filesystem root before we are willing to delete it recursively.
///
/// Two, so `/` and every top-level directory (`/Users`, `/home`, `/tmp`, ...)
/// are refused while a plausible worktree location like `/data/wt` is allowed.
/// A truncated or half-constructed path lands in the refused range.
const MIN_ORPHAN_DEPTH: usize = 2;

/// What [`guard_orphan_path`] found at an orphan's path.
///
/// `Ok(())` would conflate "safe to delete" with "there is nothing here",
/// which are very different instructions to hand to a recursive delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanTarget {
    /// A real directory that passed every safety check.
    Removable,
    /// Nothing exists at the path. A no-op, not a failure — there is no space
    /// to reclaim and no metadata to maintain.
    AlreadyGone,
}

/// Remove a single worktree.
///
/// `force` is required for worktrees that are dirty, hold unpushed commits,
/// are locked, or whose status could not be determined.
///
/// Note that git itself only ever refuses a *dirty* (or submodule-bearing, or
/// locked) worktree — it will happily discard unpushed commits without a
/// warning. The `force` gate protecting unpushed work is entirely an
/// application-level policy, decided before this function is called.
pub fn purge_one(item: &PurgeItem, force: bool) -> PurgeOutcome {
    PurgeOutcome {
        item: item.clone(),
        result: purge_inner(item, force),
    }
}

fn purge_inner(item: &PurgeItem, force: bool) -> Result<u64, String> {
    // Structural checks come before *everything*, including the size
    // measurement. Measuring is a recursive walk, so handing it a path like
    // `/` would traverse the entire filesystem before anyone got round to
    // refusing the delete — a guard that stops the deletion but not the
    // traversal still hangs the worker. This runs on the repo-owned path too,
    // where git makes the final decision, purely so nothing walks a path that
    // could not possibly be a worktree.
    if let Some(reason) = dangerous_reason(&item.worktree_path, home_dir().as_deref()) {
        return Err(reason);
    }

    match item.repo_root.as_deref() {
        Some(repo_root) => purge_repo_owned(item, repo_root, force),
        // No owning repository means no `.git/worktrees/<id>` to maintain, so
        // the metadata argument for using git does not apply and a recursive
        // delete is the only option.
        None => purge_orphan(item),
    }
}

fn purge_repo_owned(item: &PurgeItem, repo_root: &Path, force: bool) -> Result<u64, String> {
    // Defence in depth. git refuses this itself ("is a main working tree")
    // even under `--force --force`, but the main checkout is the one thing
    // this tool must never touch, so it is refused here too with a message
    // that names the actual mistake.
    if item.worktree_path == repo_root {
        return Err(format!(
            "refusing to remove {}: that is the repository's main checkout",
            item.worktree_path.display()
        ));
    }

    // Measured before the removal: afterwards there is nothing left to count.
    // Zero when the directory had already vanished, so a repo-owned item that
    // only needed its metadata cleaning never claims to have freed the stale
    // figure the scanner recorded earlier.
    let freed = measure_freed(item);

    remove_with_git(repo_root, &item.worktree_path, force)?;
    Ok(freed)
}

fn purge_orphan(item: &PurgeItem) -> Result<u64, String> {
    let path = &item.worktree_path;
    match guard_orphan_path(path)? {
        OrphanTarget::AlreadyGone => return Ok(0),
        OrphanTarget::Removable => {}
    }

    let freed = measure_freed(item);

    std::fs::remove_dir_all(path)
        .map_err(|e| format!("could not delete {}: {e}", path.display()))?;

    verify_removed(path)?;
    Ok(freed)
}

/// Remove every item in the plan, reporting each outcome to `progress` as it
/// completes.
///
/// Items are processed in the order given and every one is attempted: a
/// failure is recorded and the batch continues. Removal is serial by design —
/// concurrent `git worktree remove` invocations in one repository contend on
/// the same index lock and fail spuriously.
pub fn purge_all(
    items: &[PurgeItem],
    force: bool,
    progress: &mut dyn FnMut(&PurgeOutcome),
) -> Vec<PurgeOutcome> {
    let mut outcomes = Vec::with_capacity(items.len());
    // First-touch order, de-duplicated. A `HashSet` would make the sequence of
    // subprocesses non-deterministic for no gain; batches touch a handful of
    // repositories, so a linear scan is the cheaper structure anyway.
    let mut touched: Vec<PathBuf> = Vec::new();

    for item in items {
        let outcome = purge_one(item, force);
        // Reported immediately so the UI streams results instead of waiting
        // for the whole batch.
        progress(&outcome);

        if let Some(repo_root) = item.repo_root.as_ref()
            && !touched.iter().any(|seen| seen == repo_root)
        {
            // Failed items are included on purpose: a removal can fail *after*
            // git has already dropped the administrative directory, and it can
            // fail while leaving stale metadata behind. Both want a prune.
            touched.push(repo_root.clone());
        }

        outcomes.push(outcome);
    }

    // Once per repository, after all of its removals. `git worktree prune`
    // rescans the entire `worktrees/` directory, so running it per item would
    // be quadratic subprocess work for no benefit.
    for repo_root in &touched {
        prune_repo(repo_root);
    }

    outcomes
}

/// Drop stale `worktrees/` metadata in a repository after removals.
///
/// Best-effort by contract: a repository that vanished mid-batch, or one git
/// refuses to prune, must never fail a batch or panic.
pub fn prune_repo(repo_root: &Path) {
    if !repo_root.is_dir() {
        return;
    }
    let _ = git::run(repo_root, &["worktree", "prune"]);
}

/// Refuse obviously dangerous recursive deletes.
///
/// Only reached for orphans — worktrees whose owning repository is gone, and
/// therefore the only ones deleted without git validating the target first.
/// Public so the refusals can be tested directly against dangerous inputs
/// rather than by gambling on a delete being declined.
pub fn guard_orphan_path(path: &Path) -> Result<OrphanTarget, String> {
    // Danger checks run before the existence check: `/` exists and is a
    // directory, so asking "does it exist?" first would classify it as
    // removable.
    if let Some(reason) = dangerous_reason(path, home_dir().as_deref()) {
        return Err(reason);
    }

    match path.symlink_metadata() {
        Err(_) => Ok(OrphanTarget::AlreadyGone),
        Ok(meta) if meta.file_type().is_dir() => Ok(OrphanTarget::Removable),
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "refusing to delete {}: it is a symlink, not a worktree directory",
            path.display()
        )),
        Ok(_) => Err(format!(
            "refusing to delete {}: not a directory",
            path.display()
        )),
    }
}

fn remove_with_git(repo_root: &Path, worktree_path: &Path, force: bool) -> Result<(), String> {
    let mut args: Vec<OsString> = vec![OsString::from("worktree"), OsString::from("remove")];
    if force {
        // `--force` is a *counter* in git, not a flag. One level overrides a
        // dirty or submodule-bearing worktree; overriding `git worktree lock`
        // needs two — git's own error says "use 'remove -f -f' to override".
        // Passing two is harmless on a clean worktree and never deletes the
        // branch.
        args.push(OsString::from("--force"));
        args.push(OsString::from("--force"));
    }
    // Absolute, and as an `OsString` rather than a lossy `String`: git resolves
    // a relative path against the repository (its working directory here), and
    // a lossy conversion of a non-UTF-8 path would hand a delete a *different*
    // path than the one selected.
    args.push(worktree_path.as_os_str().to_os_string());

    let output = git::run(repo_root, &args)
        .map_err(|e| format!("could not run git in {}: {e}", repo_root.display()))?;

    if !output.success() {
        return Err(output.error_summary());
    }

    verify_removed(worktree_path)
}

/// Confirm that the worktree really is gone before reporting success.
///
/// git can exit 0 while the path we were told to remove survives: aimed at a
/// *symlink* to a worktree it resolves the link, deletes the real directory
/// and cleans the metadata, leaving the (now dangling) link behind. That is a
/// genuine success — the space was reclaimed — which is why the check is "no
/// directory remains here" rather than "nothing remains here". The case that
/// must never be reported as success is the directory still sitting on disk
/// holding the bytes we just claimed to have freed.
fn verify_removed(path: &Path) -> Result<(), String> {
    if std::fs::metadata(path).is_ok_and(|m| m.is_dir()) {
        return Err(format!(
            "removal reported success but {} is still on disk",
            path.display()
        ));
    }
    Ok(())
}

/// Bytes this removal will reclaim, measured before anything is deleted.
///
/// Never fails a removal: an unmeasurable size falls back to the scanner's
/// earlier figure and then to zero, which under-promises reclaimed space —
/// the safe direction to be wrong in.
fn measure_freed(item: &PurgeItem) -> u64 {
    let path = &item.worktree_path;
    if path.symlink_metadata().is_err() {
        // Nothing on disk. Trusting a stale `item.size` here would report
        // space that was already reclaimed by someone else.
        return 0;
    }
    // The same walk the scanner uses to fill the size column, so the figure
    // reported as freed is measured exactly the way the figure shown before
    // the purge was. Two implementations would be free to drift apart, and the
    // discrepancy would surface as the tool misreporting reclaimed space.
    match size::dir_size(path) {
        SizeState::Ready(bytes) => bytes,
        SizeState::Unknown | SizeState::Pending => item.size.unwrap_or(0),
    }
}

/// The guard's pure half: everything decidable from the path and the home
/// directory alone, with no filesystem access, so it is testable against
/// arbitrary inputs.
fn dangerous_reason(path: &Path, home: Option<&Path>) -> Option<String> {
    if !path.is_absolute() {
        return Some(format!(
            "refusing to delete the relative path {}",
            path.display()
        ));
    }

    // `..` can walk out of the selected tree entirely, and scanner-produced
    // paths are canonical, so its presence means the item was built by hand or
    // is corrupt.
    if path.components().any(|c| c == Component::ParentDir) {
        return Some(format!(
            "refusing to delete {}: the path contains '..'",
            path.display()
        ));
    }

    let depth = path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    if depth < MIN_ORPHAN_DEPTH {
        return Some(format!(
            "refusing to delete {}: too close to the filesystem root",
            path.display()
        ));
    }

    if let Some(home) = home {
        if home == path {
            return Some(format!(
                "refusing to delete {}: that is the home directory",
                path.display()
            ));
        }
        if home.starts_with(path) {
            return Some(format!(
                "refusing to delete {}: it contains the home directory",
                path.display()
            ));
        }
    }

    None
}

/// The user's home directory, read straight from the environment.
///
/// Two variable lookups, so this does not justify a dependency; and reading
/// them directly sidesteps the long-standing disagreement about what
/// `std::env::home_dir` should return on Windows. A relative or empty value is
/// discarded rather than compared against, so a broken environment cannot make
/// the guard weaker *or* block a legitimate delete.
fn home_dir() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample paths for the guard's rules, per platform.
    ///
    /// `dangerous_reason` reasons about *absolute* paths, and the platforms
    /// disagree about what that means: `/Users` is absolute on Unix but merely
    /// relative on Windows, where a drive prefix is required. Hard-coding Unix
    /// paths would therefore test the relative-path rule on Windows and leave
    /// the depth and home rules unexercised there.
    #[cfg(not(windows))]
    mod samples {
        pub const ROOT: &str = "/";
        pub const TOP_LEVEL: &[&str] = &["/Users", "/home", "/tmp", "/var", "/opt", "/etc"];
        pub const HOME: &str = "/home/tester";
        pub const DEEP_HOME: &str = "/mnt/users/tester";
        pub const HOME_ANCESTOR: &str = "/mnt/users";
        pub const ESCAPING: &str = "/data/worktrees/../../..";
        pub const PLAUSIBLE: &str = "/data/wt";
        pub const INSIDE_HOME: &str = "/home/tester/code/wt-feature";
        /// Absolute on the *other* platform, and so not absolute here.
        pub const FOREIGN_ABSOLUTE: &str = r"C:\Users\tester\wt";
    }

    #[cfg(windows)]
    mod samples {
        pub const ROOT: &str = r"C:\";
        pub const TOP_LEVEL: &[&str] = &[r"C:\Users", r"C:\Windows", r"C:\Temp", r"C:\ProgramData"];
        pub const HOME: &str = r"C:\Users\tester";
        pub const DEEP_HOME: &str = r"D:\mnt\users\tester";
        pub const HOME_ANCESTOR: &str = r"D:\mnt\users";
        pub const ESCAPING: &str = r"C:\data\worktrees\..\..\..";
        pub const PLAUSIBLE: &str = r"D:\data\wt";
        pub const INSIDE_HOME: &str = r"C:\Users\tester\code\wt-feature";
        /// Absolute on the *other* platform, and so not absolute here.
        pub const FOREIGN_ABSOLUTE: &str = "/home/tester/wt";
    }

    /// Deterministic home for the home-directory rules; the real home
    /// directory varies per machine and cannot be set safely from a threaded
    /// test.
    fn home() -> &'static Path {
        Path::new(samples::HOME)
    }

    #[test]
    fn filesystem_root_is_refused() {
        let reason = dangerous_reason(Path::new(samples::ROOT), Some(home()))
            .expect("the filesystem root must never be deletable");
        assert!(
            reason.contains("too close to the filesystem root"),
            "expected the depth rule to fire, got: {reason}"
        );
    }

    #[test]
    fn top_level_directories_are_refused() {
        for p in samples::TOP_LEVEL {
            let reason = dangerous_reason(Path::new(p), Some(home()))
                .unwrap_or_else(|| panic!("{p} must not be deletable"));
            assert!(
                reason.contains("too close to the filesystem root"),
                "expected the depth rule to fire for {p}, got: {reason}"
            );
        }
    }

    #[test]
    fn the_home_directory_itself_is_refused() {
        // Two components deep, so only the home rule can reject it — this
        // fails if that rule is dropped.
        assert_eq!(
            dangerous_reason(home(), Some(home())),
            Some(format!(
                "refusing to delete {}: that is the home directory",
                samples::HOME
            ))
        );
    }

    #[test]
    fn a_directory_containing_home_is_refused() {
        // Catches a home laid out more deeply than the depth rule protects,
        // e.g. `/mnt/users` when home is `/mnt/users/tester`.
        let deep_home = Path::new(samples::DEEP_HOME);
        let reason = dangerous_reason(Path::new(samples::HOME_ANCESTOR), Some(deep_home))
            .expect("an ancestor of home must not be deletable");
        assert!(
            reason.contains("contains the home directory"),
            "expected the home-ancestor rule to fire, got: {reason}"
        );
    }

    #[test]
    fn parent_dir_components_are_refused() {
        let reason = dangerous_reason(Path::new(samples::ESCAPING), Some(home()))
            .expect("a path that can walk out of its tree must not be deletable");
        assert!(
            reason.contains("'..'"),
            "expected the '..' rule to fire, got: {reason}"
        );
    }

    #[test]
    fn relative_paths_are_refused() {
        let reason = dangerous_reason(Path::new("some/worktree"), Some(home()))
            .expect("a relative path must not be deletable");
        assert!(
            reason.contains("relative"),
            "expected the relative-path rule to fire, got: {reason}"
        );
    }

    #[test]
    fn a_path_absolute_only_on_another_platform_is_refused() {
        // A Windows path handed to a Unix build (or the reverse) is not
        // absolute here, so it must fall to the relative-path rule rather than
        // being measured for depth against components this platform cannot
        // interpret. Pins the fail-safe direction of a genuine cross-platform
        // difference instead of leaving it to chance.
        let reason = dangerous_reason(Path::new(samples::FOREIGN_ABSOLUTE), Some(home()))
            .expect("a path this platform cannot resolve must not be deletable");
        assert!(
            reason.contains("relative"),
            "expected the relative-path rule to fire, got: {reason}"
        );
    }

    #[test]
    fn a_plausible_worktree_path_is_allowed() {
        // The guard has to say yes to something, or the orphan path is dead
        // code and every other refusal test is trivially satisfied.
        assert_eq!(
            dangerous_reason(Path::new(samples::PLAUSIBLE), Some(home())),
            None
        );
        assert_eq!(
            dangerous_reason(Path::new(samples::INSIDE_HOME), Some(home())),
            None
        );
    }

    #[test]
    fn a_missing_home_does_not_block_a_legitimate_delete() {
        assert_eq!(dangerous_reason(Path::new(samples::PLAUSIBLE), None), None);
    }

    // `verify_removed` is exercised directly because its failure branch is
    // unreachable through real git: no reproducible invocation leaves a
    // *directory* behind after exiting 0. It exists for the case we cannot
    // stage — and untested safety code is not safety code.

    #[test]
    fn verification_fails_while_a_directory_remains() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("worktree");
        std::fs::create_dir(&path).expect("create dir");

        let err = verify_removed(&path)
            .expect_err("a surviving directory must never be reported as freed");
        assert!(
            err.contains("still on disk"),
            "the message must say what went wrong, got: {err}"
        );
    }

    #[test]
    fn verification_passes_once_the_directory_is_gone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("worktree");
        std::fs::create_dir(&path).expect("create dir");
        std::fs::remove_dir(&path).expect("remove dir");

        assert_eq!(verify_removed(&path), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    fn verification_accepts_a_dangling_symlink_left_by_git() {
        // Removing through a symlink deletes the real directory and leaves the
        // link behind. The bytes really were reclaimed, so this is a success —
        // a stricter "nothing remains at this path" check would report a
        // false failure here.
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("gone");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        assert!(link.symlink_metadata().is_ok(), "the link entry survives");
        assert_eq!(verify_removed(&link), Ok(()));
    }

    #[test]
    fn depth_is_counted_below_the_root_not_including_it() {
        // One component: refused. Two: allowed. This pins MIN_ORPHAN_DEPTH so
        // widening it silently is not possible.
        assert!(dangerous_reason(Path::new("/wt"), None).is_some());
        assert!(dangerous_reason(Path::new("/a/wt"), None).is_none());
    }
}
