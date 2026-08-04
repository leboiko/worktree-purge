//! Integration tests for the enrichment milestone: size, activity, stash hints
//! and the background worker pool.
//!
//! Everything here runs against *real* git repositories built in temp dirs —
//! the whole value of this tool is being right about real git metadata, and a
//! mocked `git` would only prove that the mock matches the implementation.

mod common;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use common::{Fixture, Repo as FixtureRepo, Worktree as FixtureWorktree};
use worktree_purge::git;
use worktree_purge::model::{
    ActiveReason, ActivityState, EnrichMsg, RepoGroup, SizeState, Worktree,
};
use worktree_purge::scan::enrich;
use worktree_purge::scan::size::dir_size;
use worktree_purge::scan::stash::stash_counts;
use worktree_purge::scan::status::activity;

const MIB: u64 = 1024 * 1024;

// ---------------------------------------------------------------------------
// activity: dirty
// ---------------------------------------------------------------------------

#[test]
fn a_clean_worktree_is_clean() {
    let fx = Fixture::new();
    let repo = fx.repo_with_origin("repo");
    let wt = repo.worktree("clean-wt");

    assert_eq!(activity(wt.path()), ActivityState::Clean);
}

#[test]
fn an_untracked_file_alone_makes_a_worktree_dirty() {
    let fx = Fixture::new();
    let repo = fx.repo_with_origin("repo");
    let wt = repo.worktree("untracked-wt");
    wt.make_untracked();

    assert_eq!(
        activity(wt.path()),
        ActivityState::Active(ActiveReason::Dirty),
        "an untracked file is work that `git worktree remove` would delete"
    );
}

#[test]
fn a_modified_tracked_file_alone_makes_a_worktree_dirty() {
    let fx = Fixture::new();
    let repo = fx.repo_with_origin("repo");
    let wt = repo.worktree("modified-wt");
    wt.make_modified();

    assert_eq!(
        activity(wt.path()),
        ActivityState::Active(ActiveReason::Dirty)
    );
}

#[test]
fn a_staged_but_uncommitted_change_alone_makes_a_worktree_dirty() {
    let fx = Fixture::new();
    let repo = fx.repo_with_origin("repo");
    let wt = repo.worktree("staged-wt");
    wt.make_staged();

    assert_eq!(
        activity(wt.path()),
        ActivityState::Active(ActiveReason::Dirty)
    );
}

// ---------------------------------------------------------------------------
// activity: unpushed
// ---------------------------------------------------------------------------

#[test]
fn commits_ahead_of_an_upstream_are_unpushed_until_they_are_pushed() {
    let fx = Fixture::new();
    let repo = fx.repo_with_origin("repo");
    let wt = repo.worktree("feature");
    wt.push(); // establishes origin/feature as the upstream

    assert_eq!(
        activity(wt.path()),
        ActivityState::Clean,
        "a freshly pushed branch has nothing local"
    );

    wt.commit("local work");
    assert_eq!(
        activity(wt.path()),
        ActivityState::Active(ActiveReason::Unpushed)
    );

    wt.push();
    assert_eq!(
        activity(wt.path()),
        ActivityState::Clean,
        "pushing must clear the warning"
    );
}

#[test]
fn an_upstream_that_is_behind_outranks_the_same_commits_on_another_remote_branch() {
    let fx = Fixture::new();
    let repo = fx.repo_with_origin("repo");
    let wt = repo.worktree("feature");
    wt.push();
    wt.commit("local work");

    // Same commits, different remote branch. The weaker "on no remote ref"
    // rule now sees them as pushed, but `feature`'s own upstream is still
    // behind and `git push` would still transfer objects.
    wt.git(&["push", "--quiet", "origin", "HEAD:refs/heads/backup"]);
    wt.git(&["fetch", "--quiet", "origin"]);

    let on_no_remote = wt.git(&["rev-list", "--count", "HEAD", "--not", "--remotes"]);
    assert_eq!(
        on_no_remote.trim(),
        "0",
        "fixture must actually make the two rules disagree, otherwise this \
         test proves nothing"
    );

    assert_eq!(
        activity(wt.path()),
        ActivityState::Active(ActiveReason::Unpushed),
        "the upstream comparison must win over the any-remote fallback"
    );
}

#[test]
fn commits_on_a_branch_with_no_upstream_are_unpushed_when_the_repo_has_a_remote() {
    let fx = Fixture::new();
    let repo = fx.repo_with_origin("repo");
    let wt = repo.worktree("feature"); // never pushed => no upstream at all
    wt.commit("local work");

    let upstream = git::run(
        wt.path(),
        &["rev-parse", "--verify", "--quiet", "@{upstream}"],
    )
    .expect("git ran");
    assert!(
        !upstream.success(),
        "fixture must have no upstream, otherwise this exercises the wrong rule"
    );

    assert_eq!(
        activity(wt.path()),
        ActivityState::Active(ActiveReason::Unpushed)
    );
}

#[test]
fn in_a_repo_with_no_remotes_only_unmerged_work_is_flagged() {
    let fx = Fixture::new();
    let repo = fx.repo("solo"); // no remote anywhere
    let merged = repo.worktree("merged"); // branched off main, no new commits
    let ahead = repo.worktree("ahead");
    ahead.commit("local only");

    assert!(
        git::stdout(repo.path(), &["remote"])
            .expect("git ran")
            .trim()
            .is_empty(),
        "fixture must have no remotes"
    );
    // Without the no-remotes fallback, every commit is "on no remote" and every
    // worktree in a local-only repo lights up. Prove the naive rule really
    // would flag the merged worktree, so the assertion below has teeth.
    let naive = git::stdout_line(
        merged.path(),
        &["rev-list", "--count", "HEAD", "--not", "--remotes"],
    )
    .expect("rev-list ran");
    assert_eq!(
        naive, "1",
        "the naive rule must flag this worktree, otherwise the fallback is \
         not what makes the next assertion pass"
    );

    assert_eq!(
        activity(merged.path()),
        ActivityState::Clean,
        "a worktree sitting at the default branch holds no unmerged work"
    );
    assert_eq!(
        activity(ahead.path()),
        ActivityState::Active(ActiveReason::Unpushed),
        "commits not on the default branch are local-only work"
    );
}

#[test]
fn a_worktree_that_is_both_dirty_and_unpushed_reports_both() {
    let fx = Fixture::new();
    let repo = fx.repo_with_origin("repo");
    let wt = repo.worktree("feature");
    wt.push();
    wt.commit("local work");
    wt.make_untracked();

    assert_eq!(
        activity(wt.path()),
        ActivityState::Active(ActiveReason::Both)
    );
}

#[test]
fn a_worktree_outside_any_repository_is_unknown_never_clean() {
    let fx = Fixture::new();
    let plain = fx.plain_dir("not-a-repo");

    assert_eq!(
        activity(&plain),
        ActivityState::Unknown,
        "a failed probe must never be reported as proven clean"
    );
}

// ---------------------------------------------------------------------------
// stashes
// ---------------------------------------------------------------------------

#[test]
fn a_worktree_whose_only_change_is_stashed_is_clean_and_hints_one_stash() {
    let fx = Fixture::new();
    let repo = fx.repo_with_origin("repo");
    let wt = repo.worktree("stashed-wt");
    wt.stash("work in progress");

    // The decision this whole module is shaped around: `refs/stash` is shared,
    // so the stash survives `git worktree remove`. Nothing would be lost.
    assert_eq!(
        activity(wt.path()),
        ActivityState::Clean,
        "a stash must never make a worktree active — it survives removal"
    );

    let counts = stash_counts(repo.path());
    assert_eq!(counts.get("stashed-wt").copied(), Some(1));
    assert_eq!(counts.len(), 1, "no other branch has stashes: {counts:?}");
}

#[test]
fn stash_counts_are_mapped_to_the_branch_that_created_them() {
    let fx = Fixture::new();
    let repo = fx.repo_with_origin("repo");
    let alpha = repo.worktree("alpha");
    let beta = repo.worktree("beta");
    let quiet = repo.worktree("quiet"); // stashes nothing

    alpha.stash("alpha work");
    beta.stash("beta work");
    // A bare `git stash push` yields the other subject shape:
    // "WIP on <branch>: <sha> <subject>" instead of "On <branch>: <message>".
    beta.write_file("README.md", "beta edit\n");
    beta.git(&["stash", "push", "--quiet"]);

    let expected: HashMap<String, usize> =
        HashMap::from([("alpha".to_owned(), 1), ("beta".to_owned(), 2)]);
    assert_eq!(
        stash_counts(repo.path()),
        expected,
        "one `git stash list` per repo must attribute every entry to its branch"
    );
    assert_eq!(activity(quiet.path()), ActivityState::Clean);
}

#[test]
fn a_repository_with_no_stashes_reports_no_counts() {
    let fx = Fixture::new();
    let repo = fx.repo_with_origin("repo");
    repo.worktree("wt");

    assert!(stash_counts(repo.path()).is_empty());
}

#[test]
fn a_bare_repository_is_probed_through_one_of_its_worktrees() {
    let fx = Fixture::new();
    // `repo_with_origin` leaves a bare `<name>-origin.git` holding `main`, so
    // it can own worktrees of its own.
    fx.repo_with_origin("proj");
    let bare = fx.join("proj-origin.git");
    let wt_path = fx.join("bare-wt");
    common::git(
        &bare,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "bare-wt",
            &wt_path.to_string_lossy(),
        ],
    );
    fs::write(wt_path.join("parked.txt"), "parked\n").expect("write");
    common::git(&wt_path, &["add", "parked.txt"]);
    common::git(&wt_path, &["stash", "push", "--quiet", "-m", "parked"]);

    // `git stash list` refuses to run in a bare repo, so asking the repo root
    // directly can only ever answer "nothing".
    assert!(
        stash_counts(&bare).is_empty(),
        "precondition: a bare root cannot answer the stash question itself"
    );

    let mut group = RepoGroup::new(bare.clone(), bare.clone());
    group.worktrees = vec![Worktree::new(wt_path.clone())];
    let (tx, rx) = mpsc::channel();
    let handle = enrich::spawn(std::slice::from_ref(&group), tx);
    let msgs = drain(&rx);
    assert!(handle.wait(), "a worker panicked");

    assert_eq!(
        stash_messages(&msgs).get(&bare),
        Some(&HashMap::from([("bare-wt".to_owned(), 1)])),
        "a bare repository's stashes must be found through its worktree"
    );
}

// ---------------------------------------------------------------------------
// size
// ---------------------------------------------------------------------------

#[test]
fn size_grows_with_content_and_tracks_what_was_written() {
    let fx = Fixture::new();

    let small = fx.join("small");
    fs::create_dir_all(&small).expect("create small dir");
    fs::write(small.join("note.txt"), b"tiny").expect("write note");

    let big = fx.join("big");
    fs::create_dir_all(&big).expect("create big dir");
    fs::write(big.join("blob.bin"), filler(4 * MIB)).expect("write blob");

    let small_bytes = ready_bytes(dir_size(&small));
    let big_bytes = ready_bytes(dir_size(&big));

    assert!(
        big_bytes >= 4 * MIB,
        "allocation must cover the 4 MiB written, got {big_bytes}"
    );
    assert!(
        big_bytes < 5 * MIB,
        "4 MiB of content must not report {big_bytes} bytes"
    );
    assert!(
        big_bytes > small_bytes,
        "a 4 MiB tree ({big_bytes}) must measure larger than an almost empty \
         one ({small_bytes})"
    );
    assert!(
        small_bytes < 128 * 1024,
        "an almost empty directory must not measure {small_bytes} bytes"
    );

    // Before/after: a stub returning a constant cannot survive this.
    fs::write(big.join("blob2.bin"), filler(4 * MIB)).expect("write second blob");
    let grown = ready_bytes(dir_size(&big));
    assert!(
        grown >= big_bytes + 4 * MIB,
        "adding 4 MiB must move the measurement by at least 4 MiB: \
         {big_bytes} -> {grown}"
    );
    assert!(
        grown < big_bytes + 5 * MIB,
        "adding 4 MiB must not move the measurement by more than ~4 MiB: \
         {big_bytes} -> {grown}"
    );
}

#[cfg(unix)]
#[test]
fn size_agrees_with_du() {
    let fx = Fixture::new();
    let tree = fx.join("tree");
    fs::create_dir_all(tree.join("nested/deeper")).expect("create tree");
    fs::write(tree.join("a.bin"), filler(3 * MIB)).expect("write a");
    fs::write(tree.join("nested/b.bin"), filler(MIB)).expect("write b");
    fs::write(tree.join("nested/deeper/c.bin"), filler(4096)).expect("write c");

    let Some(du) = du_bytes(&tree) else {
        eprintln!("skipping: `du -sk` unavailable");
        return;
    };
    let ours = ready_bytes(dir_size(&tree));
    let drift = ours.abs_diff(du);
    assert!(
        drift <= du / 10 + 64 * 1024,
        "dir_size reported {ours} bytes where du reports {du} (drift {drift}); \
         this is what tells apparent length apart from allocated blocks"
    );
}

#[cfg(unix)]
#[test]
fn size_counts_symlinks_without_following_them() {
    use std::os::unix::fs::symlink;

    let fx = Fixture::new();
    let target = fx.join("target");
    fs::create_dir_all(&target).expect("create target");
    fs::write(target.join("blob.bin"), filler(4 * MIB)).expect("write blob");

    let links = fx.join("links");
    fs::create_dir_all(&links).expect("create links dir");
    symlink(target.join("blob.bin"), links.join("file-link")).expect("symlink file");
    symlink(&target, links.join("dir-link")).expect("symlink dir");

    assert!(ready_bytes(dir_size(&target)) >= 4 * MIB);
    let link_bytes = ready_bytes(dir_size(&links));
    assert!(
        link_bytes < 64 * 1024,
        "two symlinks must measure as links, not as their 4 MiB target: \
         {link_bytes} bytes"
    );
}

#[test]
fn a_missing_root_is_unknown_not_zero() {
    let fx = Fixture::new();
    assert_eq!(dir_size(&fx.join("never-created")), SizeState::Unknown);
}

#[cfg(unix)]
#[test]
fn an_unreadable_root_is_unknown_not_zero() {
    use std::os::unix::fs::PermissionsExt;

    let fx = Fixture::new();
    let locked = fx.join("locked");
    fs::create_dir_all(&locked).expect("create locked dir");
    fs::write(locked.join("secret.bin"), filler(MIB)).expect("write secret");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("lock dir");

    let readable_anyway = fs::read_dir(&locked).is_ok();
    let measured = dir_size(&locked);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("unlock dir");

    if readable_anyway {
        eprintln!("skipping: this process reads mode-000 directories (root?)");
        return;
    }
    assert_eq!(
        measured,
        SizeState::Unknown,
        "a failed measurement must not masquerade as 0 bytes reclaimed"
    );
}

#[cfg(unix)]
#[test]
fn one_unreadable_subdirectory_does_not_void_the_whole_measurement() {
    use std::os::unix::fs::PermissionsExt;

    let fx = Fixture::new();
    let root = fx.join("partly-readable");
    let locked = root.join("locked");
    fs::create_dir_all(&locked).expect("create dirs");
    fs::write(root.join("visible.bin"), filler(2 * MIB)).expect("write visible");
    fs::write(locked.join("hidden.bin"), filler(MIB)).expect("write hidden");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("lock dir");

    let readable_anyway = fs::read_dir(&locked).is_ok();
    let measured = dir_size(&root);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("unlock dir");

    if readable_anyway {
        eprintln!("skipping: this process reads mode-000 directories (root?)");
        return;
    }
    let bytes = ready_bytes(measured);
    assert!(
        bytes >= 2 * MIB,
        "the readable 2 MiB must still be counted, got {bytes}"
    );
    assert!(
        bytes < 3 * MIB,
        "the unreadable subdirectory's 1 MiB cannot be known, got {bytes}"
    );
}

// ---------------------------------------------------------------------------
// enrichment worker pool
// ---------------------------------------------------------------------------

#[test]
fn enrichment_streams_every_signal_cheapest_first_and_ends_with_one_done() {
    let fx = Fixture::new();

    let alpha = fx.repo_with_origin("alpha");
    let alpha_clean = alpha.worktree("alpha-clean");
    let alpha_dirty = alpha.worktree("alpha-dirty");
    alpha_clean.stash("parked work"); // stashed => still clean
    alpha_dirty.make_untracked();

    let beta = fx.repo("beta"); // no remotes: exercises the local-only rule
    let beta_ahead = beta.worktree("beta-ahead");
    beta_ahead.commit("local only");

    let groups = vec![
        group_of(&alpha, &[&alpha_clean, &alpha_dirty]),
        group_of(&beta, &[&beta_ahead]),
    ];

    let (tx, rx) = mpsc::channel();
    let handle = enrich::spawn(&groups, tx);
    let msgs = drain(&rx);
    assert!(handle.wait(), "a worker panicked");

    // Exactly one terminator.
    assert_eq!(
        msgs.iter().filter(|m| matches!(m, EnrichMsg::Done)).count(),
        1,
        "expected exactly one Done in {msgs:?}"
    );

    // Cheapest signal first: the phases are barriers, so every stash count
    // lands before the first activity result and every activity result lands
    // before the first (slow) size result.
    let last_stash = rposition_of(&msgs, |m| matches!(m, EnrichMsg::Stashes { .. }));
    let first_activity = position_of(&msgs, |m| matches!(m, EnrichMsg::Activity { .. }));
    let last_activity = rposition_of(&msgs, |m| matches!(m, EnrichMsg::Activity { .. }));
    let first_size = position_of(&msgs, |m| matches!(m, EnrichMsg::Size { .. }));
    assert!(
        last_stash < first_activity,
        "stash counts (one call per repo) must all land before activity: {msgs:?}"
    );
    assert!(
        last_activity < first_size,
        "activity must all land before the slow size pass: {msgs:?}"
    );

    // Stash counts, keyed by the repository's shared git dir.
    let stashes = stash_messages(&msgs);
    assert_eq!(stashes.len(), 2, "one Stashes message per repository");
    assert_eq!(
        stashes.get(&alpha.git_common_dir()),
        Some(&HashMap::from([("alpha-clean".to_owned(), 1)]))
    );
    assert_eq!(
        stashes.get(&beta.git_common_dir()),
        Some(&HashMap::new()),
        "a repo with no stashes still reports, with an empty map"
    );

    // Activity, per worktree.
    let activities = activity_messages(&msgs);
    assert_eq!(activities.len(), 3);
    assert_eq!(
        activities.get(alpha_clean.path()),
        Some(&ActivityState::Clean)
    );
    assert_eq!(
        activities.get(alpha_dirty.path()),
        Some(&ActivityState::Active(ActiveReason::Dirty))
    );
    assert_eq!(
        activities.get(beta_ahead.path()),
        Some(&ActivityState::Active(ActiveReason::Unpushed))
    );

    // Sizes, per worktree — every worktree has at least a README and a .git
    // pointer file, so nothing legitimately measures zero.
    let sizes = size_messages(&msgs);
    assert_eq!(sizes.len(), 3);
    for wt in [&alpha_clean, &alpha_dirty, &beta_ahead] {
        let state = sizes.get(wt.path()).copied().expect("size for worktree");
        assert!(
            ready_bytes(state) > 0,
            "{} measured as empty",
            wt.path().display()
        );
    }
}

#[test]
fn spawn_returns_long_before_the_work_is_done() {
    let fx = Fixture::new();
    let (repo, worktrees) = bulky_fixture(&fx);
    let groups = vec![group_of(&repo, &worktrees.iter().collect::<Vec<_>>())];

    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    let handle = enrich::spawn(&groups, tx);
    let spawn_took = started.elapsed();

    // Deterministic half of the test, immune to how fast the machine is: an
    // implementation that did the work inline — however parallel it was
    // internally — would have queued the whole pass, terminator included,
    // before `spawn` handed control back. Tolerating a few early results keeps
    // a scheduling hiccup from failing the run; a queued `Done` cannot happen.
    let mut msgs: Vec<EnrichMsg> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        !msgs.iter().any(|m| matches!(m, EnrichMsg::Done)),
        "spawn() returned with the whole pass already queued ({} messages, \
         Done included): the work happened on the calling thread",
        msgs.len()
    );

    msgs.extend(drain(&rx));
    assert!(handle.wait(), "a worker panicked");
    assert_eq!(size_messages(&msgs).len(), worktrees.len());

    // Timing half. The budget is measured rather than guessed: this is exactly
    // the work a serial `spawn` would have to finish before returning, so the
    // comparison holds on a slow CI box and a fast laptop alike, where a
    // wall-clock constant would not.
    let serial_started = Instant::now();
    for wt in &worktrees {
        let _ = activity(wt.path());
        let _ = dir_size(wt.path());
    }
    let serial_cost = serial_started.elapsed();

    assert!(
        serial_cost > Duration::from_millis(20),
        "fixture is too cheap to distinguish sync from async ({serial_cost:?})"
    );
    assert!(
        spawn_took * 4 < serial_cost,
        "spawn() took {spawn_took:?} out of the {serial_cost:?} this pass \
         costs: it is doing the work on the calling thread instead of \
         handing it to background workers"
    );
}

#[test]
fn cancel_stops_the_pass_before_everything_is_measured() {
    let fx = Fixture::new();
    let (repo, worktrees) = bulky_fixture(&fx);
    let groups = vec![group_of(&repo, &worktrees.iter().collect::<Vec<_>>())];

    let (tx, rx) = mpsc::channel();
    let handle = enrich::spawn(&groups, tx);
    // Immediately: a pass that ran to completion inside `spawn` could not
    // possibly observe this.
    handle.cancel();
    let msgs = drain(&rx);
    assert!(handle.wait(), "a worker panicked");

    assert!(
        size_messages(&msgs).len() < worktrees.len(),
        "cancel() must stop pending work, but every one of the {} worktrees \
         was still measured",
        worktrees.len()
    );
    assert_eq!(
        msgs.iter().filter(|m| matches!(m, EnrichMsg::Done)).count(),
        1,
        "a cancelled pass must still terminate with exactly one Done"
    );
}

#[test]
fn a_scan_that_found_nothing_still_terminates() {
    let (tx, rx) = mpsc::channel();
    let handle = enrich::spawn(&[], tx);
    let msgs = drain(&rx);
    assert!(handle.wait(), "a worker panicked");

    assert_eq!(
        msgs,
        vec![EnrichMsg::Done],
        "an empty scan must send Done and nothing else"
    );
}

#[test]
fn cancelling_an_already_finished_pass_is_harmless() {
    let fx = Fixture::new();
    let repo = fx.repo_with_origin("repo");
    let wt = repo.worktree("wt");
    let groups = vec![group_of(&repo, &[&wt])];

    let (tx, rx) = mpsc::channel();
    let handle = enrich::spawn(&groups, tx);
    let msgs = drain(&rx);

    handle.cancel();
    handle.cancel(); // idempotent
    assert!(
        handle.wait(),
        "cancelling a finished pass must not panic it"
    );
    assert_eq!(size_messages(&msgs).len(), 1);
}

#[test]
fn a_receiver_that_hung_up_stops_the_pass_without_panicking() {
    let fx = Fixture::new();
    let (repo, worktrees) = bulky_fixture(&fx);
    let groups = vec![group_of(&repo, &worktrees.iter().collect::<Vec<_>>())];

    let (tx, rx) = mpsc::channel::<EnrichMsg>();
    drop(rx); // the UI quit
    let handle = enrich::spawn(&groups, tx);

    let deadline = Instant::now() + Duration::from_secs(30);
    while !handle.is_cancelled() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(
        handle.is_cancelled(),
        "a hung-up receiver must stop the pass, not be silently ignored"
    );
    assert!(
        handle.wait(),
        "a worker panicked sending to a dropped receiver"
    );
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Build the model-level group the enrichment workers consume.
fn group_of(repo: &FixtureRepo, worktrees: &[&FixtureWorktree]) -> RepoGroup {
    let mut group = RepoGroup::new(repo.root.clone(), repo.git_common_dir());
    group.worktrees = worktrees
        .iter()
        .map(|wt| Worktree::new(wt.path.clone()))
        .collect();
    group
}

/// A repository whose enrichment is expensive enough that timing and
/// cancellation are observable: several worktrees, each with a few thousand
/// files to stat and a git status to run.
fn bulky_fixture(fx: &Fixture) -> (FixtureRepo, Vec<FixtureWorktree>) {
    let repo = fx.repo_with_origin("bulky");
    let worktrees: Vec<FixtureWorktree> = (0..6)
        .map(|i| {
            let wt = repo.worktree(&format!("wt-{i}"));
            fill_with_files(wt.path(), 1_500);
            wt
        })
        .collect();
    (repo, worktrees)
}

fn fill_with_files(dir: &Path, count: usize) {
    for i in 0..count {
        let sub = dir.join("bulk").join((i % 16).to_string());
        fs::create_dir_all(&sub).expect("create bulk dir");
        fs::write(sub.join(format!("f{i}.bin")), b"payload").expect("write bulk file");
    }
}

/// Content a filesystem with transparent compression cannot shrink, so the
/// allocated size really does cover what we wrote.
fn filler(len: u64) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 7) as u8)
        .collect()
}

fn ready_bytes(state: SizeState) -> u64 {
    match state {
        SizeState::Ready(bytes) => bytes,
        other => panic!("expected a measurement, got {other:?}"),
    }
}

#[cfg(unix)]
fn du_bytes(path: &Path) -> Option<u64> {
    let out = std::process::Command::new("du")
        .arg("-sk")
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let kib: u64 = stdout.split_whitespace().next()?.parse().ok()?;
    Some(kib * 1024)
}

/// Collect every message up to and including `Done`, failing loudly rather than
/// hanging when the workers never terminate.
fn drain(rx: &Receiver<EnrichMsg>) -> Vec<EnrichMsg> {
    let mut msgs = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(EnrichMsg::Done) => {
                msgs.push(EnrichMsg::Done);
                break;
            }
            Ok(msg) => msgs.push(msg),
            Err(err) => panic!(
                "enrichment never sent Done ({err:?}) after {} messages",
                msgs.len()
            ),
        }
    }
    // The workers hold the only senders, so the channel must disconnect right
    // after Done. Anything else is a second Done or a straggler.
    match rx.recv_timeout(Duration::from_secs(10)) {
        Err(RecvTimeoutError::Disconnected) => {}
        Ok(extra) => panic!("message sent after Done: {extra:?}"),
        Err(RecvTimeoutError::Timeout) => panic!("workers still alive well after Done"),
    }
    msgs
}

fn position_of(msgs: &[EnrichMsg], pred: impl Fn(&EnrichMsg) -> bool) -> usize {
    msgs.iter()
        .position(pred)
        .unwrap_or_else(|| panic!("no matching message in {msgs:?}"))
}

fn rposition_of(msgs: &[EnrichMsg], pred: impl Fn(&EnrichMsg) -> bool) -> usize {
    msgs.iter()
        .rposition(pred)
        .unwrap_or_else(|| panic!("no matching message in {msgs:?}"))
}

fn activity_messages(msgs: &[EnrichMsg]) -> HashMap<PathBuf, ActivityState> {
    msgs.iter()
        .filter_map(|m| match m {
            EnrichMsg::Activity { path, state } => Some((path.clone(), *state)),
            _ => None,
        })
        .collect()
}

fn size_messages(msgs: &[EnrichMsg]) -> HashMap<PathBuf, SizeState> {
    msgs.iter()
        .filter_map(|m| match m {
            EnrichMsg::Size { path, state } => Some((path.clone(), *state)),
            _ => None,
        })
        .collect()
}

fn stash_messages(msgs: &[EnrichMsg]) -> HashMap<PathBuf, HashMap<String, usize>> {
    msgs.iter()
        .filter_map(|m| match m {
            EnrichMsg::Stashes {
                git_common_dir,
                counts,
            } => Some((git_common_dir.clone(), counts.clone())),
            _ => None,
        })
        .collect()
}
