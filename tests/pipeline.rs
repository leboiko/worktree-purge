//! End-to-end test of the non-UI pipeline: scan discovers worktrees, then
//! background enrichment fills in every size, activity verdict and stash hint.
//!
//! The scanner and the enrichment workers are tested thoroughly in isolation
//! elsewhere; this file exists to prove they *compose* — that the paths the
//! scanner produces are the paths enrichment can key results back to, which is
//! the seam most likely to silently break.

mod common;

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use worktree_purge::model::{
    ActiveReason, ActivityState, EnrichMsg, RepoGroup, SizeState, find_worktree_mut,
};
use worktree_purge::scan::{self, enrich};

/// Run a full scan + enrichment pass and return the fully populated groups.
fn scan_and_enrich(root: &std::path::Path) -> Vec<RepoGroup> {
    let result = scan::scan(root);
    let mut groups = result.groups.clone();

    let (tx, rx) = mpsc::channel();
    let handle = enrich::spawn(&result.groups, tx);

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut saw_done = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(EnrichMsg::Done) => {
                saw_done = true;
                break;
            }
            Ok(EnrichMsg::Size { path, state }) => {
                if let Some(w) = find_worktree_mut(&mut groups, &path) {
                    w.size = state;
                }
            }
            Ok(EnrichMsg::Activity { path, state }) => {
                if let Some(w) = find_worktree_mut(&mut groups, &path) {
                    w.activity = state;
                }
            }
            Ok(EnrichMsg::Stashes {
                git_common_dir,
                counts,
            }) => apply_stashes(&mut groups, &git_common_dir, &counts),
            Err(_) => break,
        }
    }
    assert!(handle.wait(), "an enrichment worker panicked");
    assert!(saw_done, "enrichment never sent Done");
    groups
}

fn apply_stashes(
    groups: &mut [RepoGroup],
    git_common_dir: &std::path::Path,
    counts: &HashMap<String, usize>,
) {
    for group in groups
        .iter_mut()
        .filter(|g| g.git_common_dir == git_common_dir)
    {
        for worktree in &mut group.worktrees {
            if let Some(branch) = worktree.branch.as_deref() {
                worktree.stash_count = counts.get(branch).copied().unwrap_or(0);
            }
        }
    }
}

fn find<'a>(
    groups: &'a [RepoGroup],
    repo: &str,
    worktree: &str,
) -> &'a worktree_purge::model::Worktree {
    groups
        .iter()
        .find(|g| g.name == repo)
        .unwrap_or_else(|| panic!("no repo group named {repo}"))
        .worktrees
        .iter()
        .find(|w| w.name == worktree)
        .unwrap_or_else(|| panic!("no worktree named {worktree} in {repo}"))
}

#[test]
fn scan_and_enrichment_agree_on_every_worktree() {
    let fx = common::Fixture::new();

    let alpha = fx.repo_with_origin("alpha");
    alpha.worktree("wt-clean").push();
    alpha.worktree("wt-dirty").make_untracked();
    alpha.worktree("wt-ahead").commit("local only");

    let beta = fx.repo("beta");
    beta.worktree("wt-stashed").stash("work in progress");

    fx.plain_dir("not-a-repo");

    let groups = scan_and_enrich(&fx.root());

    // Grouping: exactly the two repos that own worktrees. The plain directory
    // and alpha's bare origin own none and must not appear as empty rows.
    let group_names: Vec<_> = groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(group_names, vec!["alpha", "beta"]);

    let alpha_worktrees: Vec<_> = groups[0]
        .worktrees
        .iter()
        .map(|w| w.name.as_str())
        .collect();
    assert_eq!(alpha_worktrees, vec!["wt-ahead", "wt-clean", "wt-dirty"]);

    // Every worktree got a verdict from both workers — no row left Pending,
    // which is what a broken path-keying seam would produce.
    for group in &groups {
        for worktree in &group.worktrees {
            assert!(
                matches!(worktree.size, SizeState::Ready(_)),
                "{}/{} never received a size",
                group.name,
                worktree.name
            );
            assert_ne!(
                worktree.activity,
                ActivityState::Pending,
                "{}/{} never received an activity verdict",
                group.name,
                worktree.name
            );
        }
    }

    // Each activity flavour is classified distinctly.
    assert_eq!(
        find(&groups, "alpha", "wt-clean").activity,
        ActivityState::Clean
    );
    assert_eq!(
        find(&groups, "alpha", "wt-dirty").activity,
        ActivityState::Active(ActiveReason::Dirty)
    );
    assert_eq!(
        find(&groups, "alpha", "wt-ahead").activity,
        ActivityState::Active(ActiveReason::Unpushed)
    );

    // A worktree whose only change is stashed stays Clean and still reports the
    // hint. This is the decision the whole stash design rests on.
    let stashed = find(&groups, "beta", "wt-stashed");
    assert_eq!(stashed.activity, ActivityState::Clean);
    assert_eq!(stashed.stash_count, 1);

    // A worktree carrying an extra file must measure larger than a bare one —
    // a constant-returning size implementation cannot satisfy this.
    let clean = find(&groups, "alpha", "wt-clean").size.bytes().unwrap();
    let dirty = find(&groups, "alpha", "wt-dirty").size.bytes().unwrap();
    assert!(
        dirty > clean,
        "worktree with an extra file measured {dirty} B, no larger than the clean one at {clean} B"
    );

    // Aggregates only ever count what was actually measured.
    assert_eq!(
        groups[0].known_size(),
        groups[0]
            .worktrees
            .iter()
            .filter_map(|w| w.size.bytes())
            .sum::<u64>()
    );
    assert_eq!(groups[0].active_count(), 2);
}

#[test]
fn a_worktree_outside_the_scan_root_is_still_enriched() {
    // The scanner finds these through git rather than the walk, so their paths
    // come from a different source and are the likeliest to mis-key.
    let fx = common::Fixture::new();
    let outside = tempfile::tempdir().expect("outside dir");

    let repo = fx.repo("gamma");
    let away = outside.path().join("far-away");
    repo.worktree_at(&away, "far-away").make_untracked();

    let groups = scan_and_enrich(&fx.root());

    let worktree = find(&groups, "gamma", "far-away");
    assert!(worktree.outside_scan_root, "should be flagged as outside");
    assert_eq!(
        worktree.activity,
        ActivityState::Active(ActiveReason::Dirty),
        "an out-of-root worktree must still be enriched, not left Pending"
    );
    assert!(matches!(worktree.size, SizeState::Ready(_)));
}
