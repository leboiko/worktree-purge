//! Integration tests for the purge engine.
//!
//! Every test builds *real* git repositories and worktrees and asserts against
//! the observable state afterwards — the filesystem *and* `git worktree list`.
//! Checking only the returned `Result` would pass against an implementation
//! that deletes nothing, and checking only the filesystem would miss a
//! metadata leak, which is the specific bug that makes `rm -rf` the wrong
//! tool for this job.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use worktree_purge::app::purge::{self, OrphanTarget};
use worktree_purge::model::{PurgeItem, PurgeOutcome};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn repo_item(wt: &common::Worktree) -> PurgeItem {
    PurgeItem {
        worktree_path: wt.path.clone(),
        repo_root: Some(wt.repo_root.clone()),
        name: file_name(&wt.path),
        // Deliberately `None` everywhere: the engine must measure the tree
        // itself, so a stub echoing this value back cannot pass the
        // freed-bytes tests.
        size: None,
        requires_force: false,
    }
}

fn orphan_item(path: &Path) -> PurgeItem {
    PurgeItem {
        worktree_path: path.to_path_buf(),
        repo_root: None,
        name: file_name(path),
        size: None,
        requires_force: false,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// True when *any* directory entry survives at `path`.
///
/// `Path::exists()` follows symlinks and reports `false` for a dangling one,
/// so it cannot tell "removed" from "replaced by a broken link".
fn entry_exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn is_dir(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_dir())
}

fn listed(repo: &common::Repo, path: &Path) -> bool {
    repo.listed_worktrees().iter().any(|p| p == path)
}

fn err_of(outcome: &PurgeOutcome) -> &str {
    match &outcome.result {
        Ok(freed) => panic!(
            "expected {} to fail, but it succeeded freeing {freed} bytes",
            outcome.item.worktree_path.display()
        ),
        Err(msg) => msg,
    }
}

fn freed_of(outcome: &PurgeOutcome) -> u64 {
    match &outcome.result {
        Ok(freed) => *freed,
        Err(msg) => panic!(
            "expected {} to succeed, got error: {msg}",
            outcome.item.worktree_path.display()
        ),
    }
}

fn paths(outcomes: &[PurgeOutcome]) -> Vec<PathBuf> {
    outcomes
        .iter()
        .map(|o| o.item.worktree_path.clone())
        .collect()
}

/// Discard progress reports. Used where the callback is not under test.
fn ignore_progress(_: &PurgeOutcome) {}

// ---------------------------------------------------------------------------
// the happy path — and the metadata leak it must not leave behind
// ---------------------------------------------------------------------------

#[test]
fn purging_a_clean_worktree_clears_it_from_disk_and_from_git_metadata() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let wt = repo.worktree("wt-clean");

    // Sanity: the fixture really is in the state the test assumes, and the
    // path comparison against git's output actually matches.
    assert!(is_dir(wt.path()), "fixture worktree should exist");
    assert!(
        listed(&repo, wt.path()),
        "fixture worktree should be registered before the purge"
    );

    let outcome = purge::purge_one(&repo_item(&wt), false);

    assert!(
        outcome.succeeded(),
        "clean worktree should purge without force: {:?}",
        outcome.result
    );
    assert!(
        !entry_exists(wt.path()),
        "{} survived the purge",
        wt.path().display()
    );
    assert!(
        !listed(&repo, wt.path()),
        "worktree is gone from disk but still registered in git worktree list \
         — the metadata was leaked: {:?}",
        repo.listed_worktrees()
    );
    // `git worktree remove` (not a later prune) is what cleans this up.
    assert!(
        !entry_exists(&repo.git_common_dir().join("worktrees").join("wt-clean")),
        "the administrative directory .git/worktrees/wt-clean was left behind"
    );
    // The main checkout is untouched.
    assert_eq!(repo.listed_worktrees(), vec![repo.root.clone()]);
}

#[test]
fn a_clean_worktree_is_reported_as_freeing_its_actual_bytes() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let wt = repo.worktree("wt");

    let freed = freed_of(&purge::purge_one(&repo_item(&wt), false));

    // A worktree always contains at least a checked-out README and a `.git`
    // file, so a stub returning 0 (or the item's `None` size) fails here.
    assert!(
        freed > 0,
        "a non-empty worktree cannot free zero bytes; got {freed}"
    );
}

// ---------------------------------------------------------------------------
// git's own refusals: dirty and locked
// ---------------------------------------------------------------------------

#[test]
fn purging_a_dirty_worktree_without_force_fails_and_preserves_the_work() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let wt = repo.worktree("wt-dirty");
    wt.make_modified().make_untracked();

    let outcome = purge::purge_one(&repo_item(&wt), false);

    let msg = err_of(&outcome);
    assert!(
        !msg.trim().is_empty(),
        "a failure must carry a message the UI can show"
    );
    // The message has to be git's own reason, not a generic one. git says
    // "contains modified or untracked files, use --force to delete it"; the
    // actionable half is what makes the failure modal useful.
    assert!(
        msg.to_lowercase().contains("force"),
        "the failure must carry git's explanation, got: {msg}"
    );
    assert!(
        !msg.contains("still on disk"),
        "git's reason was replaced by our own post-removal check, losing the \
         actionable part: {msg}"
    );
    assert!(
        is_dir(wt.path()),
        "git refused the removal, so the directory must still be there"
    );
    // The whole point of the refusal: the user's uncommitted work survives.
    assert_eq!(
        std::fs::read_to_string(wt.path().join("README.md")).unwrap(),
        "locally modified\n"
    );
    assert!(
        entry_exists(&wt.path().join("scratch.txt")),
        "the untracked file must survive a refused removal"
    );
    assert!(
        listed(&repo, wt.path()),
        "a refused removal must leave the worktree registered"
    );
}

#[test]
fn purging_the_same_dirty_worktree_with_force_succeeds() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let wt = repo.worktree("wt-dirty");
    wt.make_modified().make_untracked().make_staged();

    let item = repo_item(&wt);
    assert!(
        !purge::purge_one(&item, false).succeeded(),
        "precondition: git must refuse this without force"
    );

    let outcome = purge::purge_one(&item, true);

    assert!(
        outcome.succeeded(),
        "force should override a dirty working tree: {:?}",
        outcome.result
    );
    assert!(!entry_exists(wt.path()), "the directory survived --force");
    assert!(
        !listed(&repo, wt.path()),
        "still registered after a forced removal: {:?}",
        repo.listed_worktrees()
    );
}

#[test]
fn a_locked_worktree_needs_force_and_a_single_force_flag_is_not_enough() {
    // Regression guard for the subtlest bug in this milestone: `--force` is a
    // *counter* in git. One level overrides a dirty tree; overriding
    // `git worktree lock` requires two ("use 'remove -f -f' to override").
    // An engine that passes a single `--force` fails this test at the second
    // assertion block while passing every other test in this file.
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let wt = repo.worktree("wt-locked");
    repo.git(&["worktree", "lock", &wt.path.to_string_lossy()]);

    let item = repo_item(&wt);

    let refused = purge::purge_one(&item, false);
    assert!(
        refused.trim_message().to_lowercase().contains("locked"),
        "the UI needs to know it was the lock that blocked this, got: {}",
        refused.trim_message()
    );
    assert!(
        is_dir(wt.path()),
        "a locked worktree must not be removed without force"
    );
    assert!(listed(&repo, wt.path()));

    let forced = purge::purge_one(&item, true);
    assert!(
        forced.succeeded(),
        "force must override the lock: {:?}",
        forced.result
    );
    assert!(
        !entry_exists(wt.path()),
        "the locked worktree's directory survived a forced removal"
    );
    assert!(
        !listed(&repo, wt.path()),
        "the locked worktree is still registered: {:?}",
        repo.listed_worktrees()
    );
}

/// Small extension so the locked-worktree test reads cleanly.
trait OutcomeExt {
    fn trim_message(&self) -> &str;
}

impl OutcomeExt for PurgeOutcome {
    fn trim_message(&self) -> &str {
        err_of(self)
    }
}

// ---------------------------------------------------------------------------
// orphans
// ---------------------------------------------------------------------------

#[test]
fn purging_an_orphan_removes_the_directory_tree() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let wt = repo.worktree("wt-orphan");
    wt.write_file("nested/deep/file.txt", "work\n");
    let wt_path = wt.path.clone();

    // Owning repository deleted: nothing left to run `git worktree remove`
    // from, and no metadata to maintain.
    common::orphan(&repo.root);
    assert!(!entry_exists(&repo.root));
    assert!(
        is_dir(&wt_path),
        "the orphan directory should still be there"
    );

    let outcome = purge::purge_one(&orphan_item(&wt_path), false);

    assert!(
        outcome.succeeded(),
        "orphan removal should succeed: {:?}",
        outcome.result
    );
    assert!(
        !entry_exists(&wt_path),
        "the orphan directory tree was not removed"
    );
    assert!(freed_of(&outcome) > 0, "an orphan with files frees bytes");
}

#[test]
fn the_guard_refuses_dangerous_orphan_paths_instead_of_deleting_them() {
    // Nothing in this test is allowed to touch the real filesystem. If the
    // guard is broken, the assertions below are the least of the damage —
    // which is exactly why the guard is tested directly as well as through
    // `purge_one`.
    for dangerous in ["/", "/usr", "/etc", "/var", "/tmp", "/home", "/Users"] {
        let path = Path::new(dangerous);
        assert!(
            purge::guard_orphan_path(path).is_err(),
            "the guard accepted {dangerous} as a deletable orphan"
        );
    }

    // Through the public entry point, with force on: still refused — and
    // refused *immediately*. The structural checks have to run before the
    // size measurement, because measuring is a recursive walk: a guard that
    // stops the delete but not the traversal turns `/` into a filesystem-wide
    // walk that hangs the purge worker instead of failing.
    let started = Instant::now();
    let outcome = purge::purge_one(&orphan_item(Path::new("/")), true);
    let elapsed = started.elapsed();
    assert!(!err_of(&outcome).trim().is_empty());
    assert!(
        elapsed < Duration::from_secs(5),
        "refusing `/` took {elapsed:?} — the path was walked before it was \
         validated"
    );

    // And the filesystem is intact.
    assert!(is_dir(Path::new("/")), "the filesystem root must survive");
    assert!(is_dir(Path::new("/usr")), "/usr must survive");
    assert!(
        std::env::current_dir().is_ok(),
        "the current directory must survive"
    );

    // Relative paths and `..` escapes are refused too.
    assert!(purge::guard_orphan_path(Path::new("relative/wt")).is_err());
    assert!(purge::guard_orphan_path(Path::new("/a/b/../../..")).is_err());

    // The user's home directory, whatever it is on this machine.
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && home.is_absolute()
    {
        assert!(
            purge::guard_orphan_path(&home).is_err(),
            "the guard accepted the home directory {}",
            home.display()
        );
    }
}

#[test]
fn the_guard_accepts_a_real_orphan_directory() {
    // Without this, every refusal test above would be satisfied by a guard
    // that refuses everything — which would make the orphan path dead code.
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let wt = repo.worktree("wt");
    let wt_path = wt.path.clone();
    common::orphan(&repo.root);

    assert_eq!(
        purge::guard_orphan_path(&wt_path),
        Ok(OrphanTarget::Removable)
    );
}

#[test]
fn the_guard_refuses_a_symlink_and_spares_its_target() {
    let fx = common::Fixture::new();
    let treasure = fx.plain_dir("treasure");
    let link = fx.join("link-to-treasure");

    #[cfg(unix)]
    std::os::unix::fs::symlink(&treasure, &link).expect("create symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&treasure, &link).expect("create symlink");

    let outcome = purge::purge_one(&orphan_item(&link), true);

    assert!(
        err_of(&outcome).contains("symlink"),
        "expected the symlink rule to fire, got: {}",
        err_of(&outcome)
    );
    assert!(
        is_dir(&treasure),
        "deleting through the symlink would have destroyed its target"
    );
    assert_eq!(
        std::fs::read_to_string(treasure.join("notes.txt")).unwrap(),
        "not a repo\n"
    );
    assert!(entry_exists(&link), "the symlink itself was not the target");
}

#[test]
fn the_guard_refuses_a_path_that_is_a_file() {
    let fx = common::Fixture::new();
    let file = fx.join("not-a-worktree.txt");
    std::fs::write(&file, "important\n").unwrap();

    let outcome = purge::purge_one(&orphan_item(&file), true);

    assert!(!err_of(&outcome).trim().is_empty());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "important\n");
}

// ---------------------------------------------------------------------------
// vanished paths must never panic
// ---------------------------------------------------------------------------

#[test]
fn an_orphan_path_that_no_longer_exists_is_a_clean_no_op() {
    let fx = common::Fixture::new();
    let gone = fx.join("never-existed");
    assert!(!entry_exists(&gone));

    let outcome = purge::purge_one(&orphan_item(&gone), false);

    assert_eq!(
        outcome.result,
        Ok(0),
        "nothing on disk means nothing reclaimed"
    );
    assert!(!entry_exists(&gone));
}

#[test]
fn a_repo_owned_worktree_whose_directory_vanished_still_gets_its_metadata_cleaned() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let wt = repo.worktree("wt-vanished");
    let wt_path = wt.path.clone();

    // Someone deleted the directory behind git's back. The metadata is still
    // there, which is real cleanup work — so the item is still handed to git
    // rather than short-circuited.
    std::fs::remove_dir_all(&wt_path).unwrap();
    assert!(
        listed(&repo, &wt_path),
        "precondition: git still lists the vanished worktree"
    );

    let outcome = purge::purge_one(&repo_item(&wt), false);

    assert!(
        outcome.succeeded(),
        "git cleans metadata for a vanished worktree: {:?}",
        outcome.result
    );
    assert_eq!(
        outcome.result,
        Ok(0),
        "no directory on disk means no bytes reclaimed, whatever the scanner \
         recorded earlier"
    );
    assert!(
        !listed(&repo, &wt_path),
        "the stale metadata was not cleaned: {:?}",
        repo.listed_worktrees()
    );
}

#[test]
fn a_path_the_repository_never_owned_fails_without_touching_it() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let stranger = fx.plain_dir("stranger");

    let item = PurgeItem {
        worktree_path: stranger.clone(),
        repo_root: Some(repo.root.clone()),
        name: "stranger".into(),
        size: None,
        requires_force: false,
    };

    let outcome = purge::purge_one(&item, true);

    assert!(!err_of(&outcome).trim().is_empty());
    assert!(
        is_dir(&stranger),
        "a directory the repo does not own must never be deleted"
    );
    assert_eq!(
        std::fs::read_to_string(stranger.join("notes.txt")).unwrap(),
        "not a repo\n"
    );
}

#[test]
fn the_main_checkout_is_refused_even_when_an_item_points_at_it() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    repo.worktree("wt");

    let item = PurgeItem {
        worktree_path: repo.root.clone(),
        repo_root: Some(repo.root.clone()),
        name: "proj".into(),
        size: None,
        requires_force: false,
    };

    let outcome = purge::purge_one(&item, true);

    // git refuses this too ("is a main working tree"), which would satisfy a
    // weaker assertion — so this pins *our* refusal specifically. The main
    // checkout is the one thing this tool must never delete; that guarantee
    // should not rest entirely on a subprocess behaving as expected.
    assert!(
        err_of(&outcome).contains("main checkout"),
        "expected our own refusal to fire before git was invoked, got: {}",
        err_of(&outcome)
    );
    assert!(is_dir(&repo.root), "the main checkout was removed");
    assert!(is_dir(&repo.root.join(".git")));
    assert_eq!(
        std::fs::read_to_string(repo.root.join("README.md")).unwrap(),
        "fixture\n"
    );
}

#[cfg(unix)]
#[test]
fn removal_through_a_symlink_is_a_success_even_though_the_link_survives() {
    // The one reproducible case where git exits 0 while the path it was given
    // still exists: pointed at a symlink, git resolves it, deletes the real
    // worktree, cleans the metadata and returns 0 — leaving a dangling link.
    // The bytes really were reclaimed, so this must be reported as success. A
    // verification of "nothing remains at this path" would report a false
    // failure and tell the user their disk space was not freed when it was.
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let wt = repo.worktree("wt-real");
    let link = fx.join("wt-link");
    std::os::unix::fs::symlink(wt.path(), &link).expect("create symlink");

    let item = PurgeItem {
        worktree_path: link.clone(),
        repo_root: Some(repo.root.clone()),
        name: "wt-link".into(),
        size: None,
        requires_force: false,
    };

    let outcome = purge::purge_one(&item, false);

    assert!(
        outcome.succeeded(),
        "the worktree was freed, so this is a success: {:?}",
        outcome.result
    );
    assert!(
        !entry_exists(wt.path()),
        "the real worktree directory should be gone"
    );
    assert!(
        entry_exists(&link),
        "precondition for this test: git leaves the dangling link behind"
    );
    assert!(
        !is_dir(&link),
        "the link must not still resolve to a directory"
    );
    assert!(!listed(&repo, wt.path()));
    assert_eq!(repo.listed_worktrees(), vec![repo.root.clone()]);
}

// ---------------------------------------------------------------------------
// batches
// ---------------------------------------------------------------------------

#[test]
fn one_failure_does_not_abort_the_batch() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let first = repo.worktree("wt-first");
    let dirty = repo.worktree("wt-dirty");
    let last = repo.worktree("wt-last");
    dirty.make_untracked();

    let items = vec![repo_item(&first), repo_item(&dirty), repo_item(&last)];
    let outcomes = purge::purge_all(&items, false, &mut ignore_progress);

    assert_eq!(outcomes.len(), 3);
    assert_eq!(
        outcomes.iter().filter(|o| o.succeeded()).count(),
        2,
        "both clean worktrees should have been purged"
    );
    assert_eq!(outcomes.iter().filter(|o| !o.succeeded()).count(), 1);

    let failed: Vec<PathBuf> = outcomes
        .iter()
        .filter(|o| !o.succeeded())
        .map(|o| o.item.worktree_path.clone())
        .collect();
    assert_eq!(failed, vec![dirty.path.clone()]);
    assert!(
        !err_of(outcomes.iter().find(|o| !o.succeeded()).unwrap())
            .trim()
            .is_empty(),
        "the failure must be reported with a usable message"
    );

    // The item *after* the failure was still processed — this is what an
    // early `return`/`?` in the loop would break.
    assert!(
        !entry_exists(last.path()),
        "the batch stopped at the failure"
    );
    assert!(!entry_exists(first.path()));
    assert!(!listed(&repo, first.path()));
    assert!(!listed(&repo, last.path()));

    // And the failed one is untouched, still registered.
    assert!(is_dir(dirty.path()));
    assert!(listed(&repo, dirty.path()));
}

#[test]
fn progress_fires_exactly_once_per_item_in_order() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let a = repo.worktree("wt-a");
    let b = repo.worktree("wt-b");
    let c = repo.worktree("wt-c");
    b.make_untracked(); // fails without force — reporting must not skip it

    let other = fx.repo("other");
    let orphan_wt = other.worktree("wt-orphan");
    let orphan_path = orphan_wt.path.clone();
    common::orphan(&other.root);

    let items = vec![
        repo_item(&a),
        repo_item(&b),
        repo_item(&c),
        orphan_item(&orphan_path),
    ];
    let expected: Vec<PathBuf> = items.iter().map(|i| i.worktree_path.clone()).collect();

    let mut seen: Vec<PathBuf> = Vec::new();
    let outcomes = purge::purge_all(&items, false, &mut |o: &PurgeOutcome| {
        seen.push(o.item.worktree_path.clone());
    });

    assert_eq!(
        seen, expected,
        "progress must fire once per item, in the order given"
    );
    assert_eq!(
        seen.len(),
        items.len(),
        "progress fired a wrong number of times"
    );
    assert_eq!(
        paths(&outcomes),
        expected,
        "the returned outcomes must be in the same order as the reports"
    );
    // The reported failure is the one that actually failed.
    assert!(!outcomes[1].succeeded());
    assert!(outcomes[0].succeeded() && outcomes[2].succeeded() && outcomes[3].succeeded());
}

#[test]
fn a_batch_prunes_stale_metadata_once_per_touched_repository() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let stale = repo.worktree("wt-stale");
    let target = repo.worktree("wt-target");

    // A worktree deleted behind git's back: `git worktree remove wt-target`
    // will not clean this up, only `git worktree prune` will.
    let stale_path = stale.path.clone();
    std::fs::remove_dir_all(&stale_path).unwrap();
    assert!(
        listed(&repo, &stale_path),
        "precondition: the stale entry is registered"
    );

    let outcomes = purge::purge_all(&[repo_item(&target)], false, &mut ignore_progress);

    assert!(outcomes[0].succeeded(), "{:?}", outcomes[0].result);
    assert!(!entry_exists(target.path()));
    assert!(
        !listed(&repo, &stale_path),
        "purge_all did not prune the repository: {:?}",
        repo.listed_worktrees()
    );
    assert_eq!(repo.listed_worktrees(), vec![repo.root.clone()]);
}

#[test]
fn purging_every_worktree_leaves_the_main_checkout_intact() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let a = repo.worktree_on_branch("wt-a", "feature-a");
    let b = repo.worktree_on_branch("wt-b", "feature-b");
    let c = repo.worktree_on_branch("wt-c", "feature-c");
    a.make_untracked();
    b.commit("unpushed work");
    repo.git(&["worktree", "lock", &c.path.to_string_lossy()]);

    let items = vec![repo_item(&a), repo_item(&b), repo_item(&c)];
    let outcomes = purge::purge_all(&items, true, &mut ignore_progress);

    assert_eq!(
        outcomes.iter().filter(|o| o.succeeded()).count(),
        3,
        "dirty, unpushed and locked worktrees should all yield to force: {:?}",
        outcomes.iter().map(|o| &o.result).collect::<Vec<_>>()
    );
    for wt in [&a, &b, &c] {
        assert!(!entry_exists(wt.path()), "{} survived", wt.path().display());
    }

    // The main checkout: still a repository, still has its content, and is
    // now the only registered worktree.
    assert!(
        is_dir(&repo.root),
        "the main checkout directory was removed"
    );
    assert!(
        is_dir(&repo.root.join(".git")),
        "the .git directory was removed"
    );
    assert_eq!(
        std::fs::read_to_string(repo.root.join("README.md")).unwrap(),
        "fixture\n"
    );
    assert_eq!(repo.listed_worktrees(), vec![repo.root.clone()]);
    // Still a usable repository, not a corpse.
    assert_eq!(
        repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "main"
    );

    // Removing a worktree must never delete its branch.
    let branches = repo.git(&["branch", "--list", "--format=%(refname:short)"]);
    let branches: Vec<&str> = branches.lines().map(str::trim).collect();
    for expected in ["main", "feature-a", "feature-b", "feature-c"] {
        assert!(
            branches.contains(&expected),
            "branch {expected} was deleted along with its worktree: {branches:?}"
        );
    }
}

#[test]
fn a_batch_spanning_several_repositories_purges_all_of_them() {
    let fx = common::Fixture::new();
    let one = fx.repo("one");
    let two = fx.repo("two");
    let a1 = one.worktree("one-a");
    let b1 = two.worktree("two-a");
    let a2 = one.worktree("one-b");

    // Interleaved on purpose: grouping for the prune must not reorder or drop
    // removals.
    let items = vec![repo_item(&a1), repo_item(&b1), repo_item(&a2)];
    let outcomes = purge::purge_all(&items, false, &mut ignore_progress);

    assert_eq!(outcomes.iter().filter(|o| o.succeeded()).count(), 3);
    for wt in [&a1, &b1, &a2] {
        assert!(!entry_exists(wt.path()), "{} survived", wt.path().display());
    }
    assert_eq!(one.listed_worktrees(), vec![one.root.clone()]);
    assert_eq!(two.listed_worktrees(), vec![two.root.clone()]);
}

// ---------------------------------------------------------------------------
// freed bytes
// ---------------------------------------------------------------------------

#[test]
fn freed_bytes_track_the_actual_content_of_each_worktree() {
    const BIG: usize = 4 * 1024 * 1024;

    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let small = repo.worktree("wt-small");
    let big = repo.worktree("wt-big");

    big.write_file("big.bin", &"x".repeat(BIG));
    big.git(&["add", "."]);
    big.git(&["commit", "--quiet", "-m", "big file"]);

    let small_freed = freed_of(&purge::purge_one(&repo_item(&small), false));
    let big_freed = freed_of(&purge::purge_one(&repo_item(&big), false));

    // A constant, or an echo of the item's `size` (which is `None` here),
    // cannot satisfy all three of these.
    assert!(
        big_freed >= BIG as u64,
        "a worktree holding a {BIG}-byte file must report at least that much \
         freed; got {big_freed}"
    );
    assert!(
        small_freed < BIG as u64 / 2,
        "a near-empty worktree must not report {small_freed} bytes"
    );
    assert!(
        big_freed > small_freed,
        "big ({big_freed}) should exceed small ({small_freed})"
    );
    assert!(
        big_freed < (BIG as u64) * 8,
        "the measurement escaped the worktree and counted the whole repo: \
         {big_freed}"
    );
    assert!(small_freed > 0, "even a fresh worktree holds a checkout");
}

#[test]
fn freed_bytes_do_not_follow_symlinks_out_of_the_worktree() {
    const BIG: usize = 4 * 1024 * 1024;

    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let outside = fx.plain_dir("outside");
    std::fs::write(outside.join("huge.bin"), "y".repeat(BIG)).unwrap();

    let wt = repo.worktree("wt");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, wt.path().join("link")).expect("symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&outside, wt.path().join("link")).expect("symlink");

    let freed = freed_of(&purge::purge_one(&repo_item(&wt), true));

    assert!(
        freed < BIG as u64,
        "the symlinked tree is not reclaimed by removing this worktree, so it \
         must not be counted; got {freed}"
    );
    assert!(
        is_dir(&outside),
        "the symlink target must survive the worktree removal"
    );
    assert_eq!(
        std::fs::metadata(outside.join("huge.bin")).unwrap().len(),
        BIG as u64
    );
}

// ---------------------------------------------------------------------------
// prune_repo is best-effort
// ---------------------------------------------------------------------------

#[test]
fn prune_repo_clears_stale_metadata() {
    let fx = common::Fixture::new();
    let repo = fx.repo("proj");
    let wt = repo.worktree("wt");
    let wt_path = wt.path.clone();
    std::fs::remove_dir_all(&wt_path).unwrap();
    assert!(listed(&repo, &wt_path), "precondition");

    purge::prune_repo(&repo.root);

    assert!(
        !listed(&repo, &wt_path),
        "prune_repo did nothing: {:?}",
        repo.listed_worktrees()
    );
}

#[test]
fn prune_repo_tolerates_non_repositories_and_missing_directories() {
    let fx = common::Fixture::new();
    let plain = fx.plain_dir("not-a-repo");
    let missing = fx.join("gone");

    // Must not panic, and must not damage anything.
    purge::prune_repo(&plain);
    purge::prune_repo(&missing);
    purge::prune_repo(Path::new("/"));

    assert_eq!(
        std::fs::read_to_string(plain.join("notes.txt")).unwrap(),
        "not a repo\n"
    );
    assert!(!entry_exists(&missing));
}
