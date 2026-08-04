//! Background enrichment: streams size, activity and stash data to the UI.
//!
//! `spawn` returns immediately and does everything on background threads, so a
//! multi-GB scan never freezes the terminal. Work runs in three phases, ordered
//! cheapest-first so the rows fill in from the fastest signal to the slowest:
//!
//! 1. **stashes** — one subprocess per *repository*,
//! 2. **activity** — a handful of subprocesses per worktree,
//! 3. **sizes** — thousands of `stat` calls per worktree.
//!
//! The phases are barriers; within a phase the worktrees are processed in
//! parallel.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use rayon::prelude::*;

use crate::model::{EnrichMsg, RepoGroup};
use crate::scan::{size, stash, status};

/// Handle to a running enrichment pass.
pub struct EnrichHandle {
    cancel: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}

impl EnrichHandle {
    /// Ask the workers to stop early (used on rescan and on quit).
    ///
    /// Idempotent and safe to call after the pass has already finished. The
    /// flag is checked before every phase, before every item and inside the
    /// size walk, so a cancelled pass stops within one directory read rather
    /// than churning through a tree nobody is looking at any more.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Has this pass been asked to stop (by [`cancel`](Self::cancel) or by the
    /// receiver hanging up)?
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Block until the pass has fully stopped.
    ///
    /// Returns `true` when the workers finished normally and `false` when one
    /// of them panicked. The TUI only ever calls [`cancel`](Self::cancel);
    /// this exists for tests and for a deterministic shutdown.
    #[must_use]
    pub fn wait(self) -> bool {
        self.worker.join().is_ok()
    }
}

/// One repository's stash probe, resolved before the workers start.
struct RepoTask {
    /// Directory to run `git stash list` in.
    probe_dir: PathBuf,
    /// Identity the counts are reported under.
    git_common_dir: PathBuf,
}

/// Spawn workers that compute size, activity and stash counts for every
/// worktree in `groups`, sending results to `tx` as they complete.
///
/// Returns immediately: the UI must stay responsive during a multi-GB scan.
/// Exactly one [`EnrichMsg::Done`] is sent per call, on every exit path
/// including cancellation, so a consumer can always drain to a terminator.
pub fn spawn(groups: &[RepoGroup], tx: Sender<EnrichMsg>) -> EnrichHandle {
    // Snapshot the paths up front so the workers own their inputs and `groups`
    // stays free for the UI thread to re-sort, mutate or drop.
    let repos: Vec<RepoTask> = groups
        .iter()
        .map(|group| RepoTask {
            probe_dir: stash_probe_dir(group),
            git_common_dir: group.git_common_dir.clone(),
        })
        .collect();
    let worktrees: Vec<PathBuf> = groups
        .iter()
        .flat_map(|group| group.worktrees.iter().map(|wt| wt.path.clone()))
        .collect();

    let cancel = Arc::new(AtomicBool::new(false));
    let worker = thread::spawn({
        let cancel = Arc::clone(&cancel);
        move || run(&repos, &worktrees, &tx, &cancel)
    });

    EnrichHandle { cancel, worker }
}

fn run(repos: &[RepoTask], worktrees: &[PathBuf], tx: &Sender<EnrichMsg>, cancel: &AtomicBool) {
    // Phase 1 — stash counts: one subprocess per repository, the cheapest
    // signal we have, so it lands on screen first.
    phase(repos, tx, cancel, |repo| {
        Some(EnrichMsg::Stashes {
            git_common_dir: repo.git_common_dir.clone(),
            counts: stash::stash_counts(&repo.probe_dir),
        })
    });

    // Phase 2 — activity: a few subprocesses per worktree.
    phase(worktrees, tx, cancel, |path| {
        Some(EnrichMsg::Activity {
            path: path.clone(),
            state: status::activity(path),
        })
    });

    // Phase 3 — sizes: the slow one, and the only work that can be interrupted
    // mid-item. `None` drops a partial sum instead of sending it: reporting
    // fewer bytes than a worktree really holds would understate what a purge
    // frees.
    phase(worktrees, tx, cancel, |path| {
        Some(EnrichMsg::Size {
            path: path.clone(),
            state: size::dir_size_cancellable(path, cancel)?,
        })
    });

    // Exactly one terminator per pass, whether it completed or was cancelled.
    let _ = tx.send(EnrichMsg::Done);
}

/// Run one phase over `items` in parallel, sending whatever it produces.
///
/// The cancellation protocol lives here and nowhere else: skip the phase
/// entirely if the pass is already cancelled, and re-check before each item so
/// a cancel lands within one item rather than one phase. `for_each_with` clones
/// the sender per worker — that is how a `Sender`, which is `Send` but not
/// `Sync`, is fanned out across rayon threads.
fn phase<T: Sync>(
    items: &[T],
    tx: &Sender<EnrichMsg>,
    cancel: &AtomicBool,
    produce: impl Fn(&T) -> Option<EnrichMsg> + Send + Sync,
) {
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    items.par_iter().for_each_with(tx.clone(), |tx, item| {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if let Some(msg) = produce(item) {
            send(tx, cancel, msg);
        }
    });
}

/// Send a result, treating a hung-up receiver as a cancellation.
///
/// The UI drops its receiver on quit; workers must neither panic on that nor
/// keep burning cores producing messages nobody will read.
fn send(tx: &Sender<EnrichMsg>, cancel: &AtomicBool, msg: EnrichMsg) {
    if tx.send(msg).is_err() {
        cancel.store(true, Ordering::Relaxed);
    }
}

/// Where to run `git stash list` for a group.
///
/// `git stash list` refuses to run outside a work tree, so a **bare**
/// repository has to be probed from one of its linked worktrees — they share
/// the same `refs/stash`, so the answer is identical. An orphaned group has no
/// repository left at all and falls back to its root, where the call fails and
/// yields no counts.
fn stash_probe_dir(group: &RepoGroup) -> PathBuf {
    if group.root.join(".git").exists() {
        return group.root.clone();
    }
    group
        .worktrees
        .iter()
        .map(|wt| &wt.path)
        .find(|path| path.is_dir())
        .cloned()
        .unwrap_or_else(|| group.root.clone())
}
