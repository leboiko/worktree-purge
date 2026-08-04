//! "Would purging this lose work?" — dirty tree and unpushed commit detection.
//!
//! Two independent probes, each answering `Some(true)`, `Some(false)` or
//! `None` ("no answer"). `None` is deliberately distinct from `Some(false)`:
//! see [`activity`] for how a failed probe is folded in.
//!
//! Severity note: `git worktree remove` does not delete branches or commits —
//! those live in the shared object store and survive removal. *Dirty* therefore
//! means work would actually be **lost**, while *unpushed* means the worktree is
//! clearly still in use but its commits would survive. Both mark a worktree
//! active; `ActiveReason` keeps them apart so the UI can render them at
//! different severities. See `docs/notes-enrichment.md`.

use std::ffi::OsStr;
use std::path::Path;

use crate::git;
use crate::model::{ActiveReason, ActivityState};

/// Classify a worktree as clean, active, or unknown.
///
/// Never returns `Clean` on error: a failed probe yields `Unknown`, which the
/// purge flow treats as needing confirmation.
pub fn activity(worktree: &Path) -> ActivityState {
    let dirty = probe_dirty(worktree);
    let unpushed = probe_unpushed(worktree);

    match (dirty, unpushed) {
        (Some(dirty), Some(unpushed)) => match ActiveReason::combine(dirty, unpushed) {
            Some(reason) => ActivityState::Active(reason),
            None => ActivityState::Clean,
        },
        // Partial knowledge. A signal we *proved* still wins: downgrading it to
        // `Unknown` would drop the worktree out of `is_active()`, out of the
        // active count and out of the active-first sort. An error must never be
        // able to shrink a warning — only the other way round.
        (Some(true), None) => ActivityState::Active(ActiveReason::Dirty),
        (None, Some(true)) => ActivityState::Active(ActiveReason::Unpushed),
        // Nothing proven either way: refuse to claim the worktree is safe.
        _ => ActivityState::Unknown,
    }
}

/// Uncommitted work that removal would destroy: modified, staged or untracked.
///
/// `--untracked-files=normal` is explicit rather than implied, because
/// `status.showUntrackedFiles=no` in a repo (or the user's global config) would
/// otherwise hide untracked work and let us call a dirty worktree clean. A
/// command-line flag outranks configuration, so the probe cannot be defeated.
fn probe_dirty(worktree: &Path) -> Option<bool> {
    let out = git::run(
        worktree,
        &["status", "--porcelain", "--untracked-files=normal"],
    )
    .ok()?;
    if !out.success() {
        return None;
    }
    Some(out.stdout.lines().any(|line| !line.trim().is_empty()))
}

/// Commits that exist only here. Fallback chain, first applicable rule wins:
///
/// 1. branch with an upstream → `@{upstream}..HEAD`
/// 2. no upstream but remotes exist → `HEAD --not --remotes`
/// 3. no remotes at all → `HEAD --not <default branch>` (unmerged local work)
/// 4. detached HEAD → rule 2 or rule 3, whichever applies
///
/// Rule 3 is what keeps local-only repositories usable: rule 2 would report
/// *every* commit in a repo with no remotes as unpushed, flagging every
/// worktree and training the user to ignore the warning.
fn probe_unpushed(worktree: &Path) -> Option<bool> {
    // An unborn HEAD (freshly initialised repo) has no commits, so nothing can
    // be unpushed. Exit 1 is "HEAD does not resolve"; anything else (128) means
    // this is not a working repository and we have no answer.
    match git::run(worktree, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .ok()?
        .status
    {
        Some(0) => {}
        Some(1) => return Some(false),
        _ => return None,
    }

    // Rule 1. A tracked branch: ask git exactly how far ahead of its upstream
    // it is. This outranks rule 2 on purpose — the same commits may also sit on
    // some other remote ref (a backup branch), which would make rule 2 answer
    // "pushed" while a `git push` on this branch would still transfer them.
    let on_branch = git::run(worktree, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()?
        .success();
    if on_branch && has_upstream(worktree) {
        return count_above_zero(worktree, &["rev-list", "--count", "@{upstream}..HEAD"]);
    }

    // Rules 2 and 4: no upstream, or a detached HEAD. Anything reachable from
    // HEAD but from no remote-tracking ref exists only on this machine.
    if has_remotes(worktree)? {
        return count_above_zero(
            worktree,
            &["rev-list", "--count", "HEAD", "--not", "--remotes"],
        );
    }

    // Rule 3. Nothing can be "pushed" in a repo with no remotes, so the useful
    // question becomes "is this work merged into the default branch yet?".
    let default = default_branch(worktree)?;
    count_above_zero(
        worktree,
        &["rev-list", "--count", "HEAD", "--not", default.as_str()],
    )
}

/// Does the current branch track anything?
fn has_upstream(worktree: &Path) -> bool {
    git::run(
        worktree,
        &["rev-parse", "--verify", "--quiet", "@{upstream}"],
    )
    .is_ok_and(|out| out.success())
}

/// Are any remotes configured? `None` when git could not tell us.
fn has_remotes(worktree: &Path) -> Option<bool> {
    let out = git::stdout(worktree, &["remote"])?;
    Some(out.lines().any(|line| !line.trim().is_empty()))
}

/// Best-effort default branch of a repository with no remotes, as a full
/// refname.
///
/// The repository's shared `HEAD` — the main checkout's current branch — is the
/// closest thing a local-only repo has to a default branch; `main`/`master` are
/// the fallback when the main checkout is itself detached or unborn.
fn default_branch(worktree: &Path) -> Option<String> {
    let common = git::stdout_line(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    if let Some(common) = common.filter(|c| !c.is_empty()) {
        // `--git-dir` points HEAD resolution at the shared git directory
        // without relying on git's cwd-based repository discovery.
        let main_head = git::stdout_line(
            worktree,
            &[
                "--git-dir",
                common.as_str(),
                "symbolic-ref",
                "--quiet",
                "--short",
                "HEAD",
            ],
        );
        if let Some(name) = main_head.filter(|n| !n.is_empty()) {
            let refname = format!("refs/heads/{name}");
            if ref_exists(worktree, &refname) {
                return Some(refname);
            }
        }
    }

    ["refs/heads/main", "refs/heads/master"]
        .into_iter()
        .find(|refname| ref_exists(worktree, refname))
        .map(str::to_owned)
}

fn ref_exists(worktree: &Path, refname: &str) -> bool {
    git::run(worktree, &["rev-parse", "--verify", "--quiet", refname])
        .is_ok_and(|out| out.success())
}

/// Run a `rev-list --count` probe: `Some(true)` when the count is above zero,
/// `None` when git did not produce a number we can trust.
fn count_above_zero<S: AsRef<OsStr>>(worktree: &Path, args: &[S]) -> Option<bool> {
    let count: u64 = git::stdout_line(worktree, args)?.parse().ok()?;
    Some(count > 0)
}
