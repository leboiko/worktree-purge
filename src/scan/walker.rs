//! Parallel filesystem walk that discovers repositories and linked worktrees.
//!
//! The walk is deliberately dumb and fast: it only decides *this directory is
//! interesting, stop descending*. Everything that needs judgement — which
//! repository a worktree belongs to, what branch it is on, whether it is
//! locked — is decided afterwards in [`resolve`], from git's own answers.
//!
//! See `docs/notes-scanner.md` for the empirical rules behind the
//! classification.

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use ignore::{DirEntry, WalkBuilder, WalkState};

use crate::git;
use crate::model::{RepoGroup, ScanResult, Worktree};
use crate::scan::repo::{self, DotGit, WorktreeEntry};

/// Walk `root` and return every repository group with its linked worktrees.
///
/// Prunes on hit: once a `.git` entry identifies a directory as a repository or
/// worktree, the walk does not descend into it.
///
/// Never panics and never aborts early: unreadable directories are collected
/// into [`ScanResult::skipped`] and the rest of the tree is still scanned.
pub fn scan(root: &Path) -> ScanResult {
    let mut result = ScanResult::default();
    let scan_root = repo::normalize(root);

    match std::fs::metadata(&scan_root) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            result
                .warnings
                .push(format!("{} is not a directory", scan_root.display()));
            return result;
        }
        Err(err) => {
            result
                .warnings
                .push(format!("cannot read {}: {err}", scan_root.display()));
            return result;
        }
    }

    if !git::available() {
        result.warnings.push(
            "git executable not found: branch, lock and out-of-root worktree details are \
             unavailable"
                .to_string(),
        );
    }

    let discovered = walk(&scan_root);
    resolve(&scan_root, discovered, &mut result);
    result
}

/// Something the walk found worth keeping.
#[derive(Debug)]
enum Hit {
    /// A main checkout or a bare repository.
    Repository { root: PathBuf },
    /// A live linked worktree.
    Worktree { path: PathBuf, gitdir: PathBuf },
    /// A worktree whose gitdir target is gone.
    Orphan { path: PathBuf, gitdir: PathBuf },
}

#[derive(Debug, Default)]
struct Discovered {
    hits: Vec<Hit>,
    skipped: Vec<PathBuf>,
}

/// Phase 1: the filesystem walk. Touches no git subprocess except the bare-repo
/// confirmation, which only fires for directories that already look like one.
fn walk(root: &Path) -> Discovered {
    let shared = Mutex::new(Discovered::default());

    let mut builder = WalkBuilder::new(root);
    builder
        // Every filter off: worktrees very often live in gitignored
        // directories, and `.git` itself is hidden.
        .standard_filters(false)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .require_git(false)
        // Symlinked directories are a loop risk and would also report the same
        // worktree under two paths.
        .follow_links(false);

    builder.build_parallel().run(|| {
        let shared = &shared;
        Box::new(move |entry| visit(shared, entry))
    });

    shared.into_inner().unwrap_or_else(PoisonError::into_inner)
}

fn visit(shared: &Mutex<Discovered>, entry: Result<DirEntry, ignore::Error>) -> WalkState {
    let entry = match entry {
        Ok(entry) => entry,
        Err(err) => {
            if let Some(path) = error_path(&err) {
                record(shared, |d| d.skipped.push(path.to_path_buf()));
            }
            // The walker has already given up on this entry; keep going.
            return WalkState::Continue;
        }
    };

    // Only directories can *contain* a `.git`. Symlinks are reported as
    // symlinks (we do not follow them) and are skipped here.
    if !entry.file_type().is_some_and(|ft| ft.is_dir()) {
        return WalkState::Continue;
    }
    let dir = entry.path();

    match std::fs::metadata(dir.join(".git")) {
        Ok(_) => {
            classify(shared, dir);
            // Prune regardless of what the `.git` entry turned out to be. The
            // performance property must not depend on classification
            // succeeding, and repository internals are never purge targets.
            return WalkState::Skip;
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(_) => {
            // We cannot even stat inside this directory, so we certainly
            // cannot walk it.
            record(shared, |d| d.skipped.push(dir.to_path_buf()));
            return WalkState::Skip;
        }
    }

    if repo::is_bare_repo_dir(dir) {
        record(shared, |d| {
            d.hits.push(Hit::Repository {
                root: dir.to_path_buf(),
            })
        });
        return WalkState::Skip;
    }

    WalkState::Continue
}

fn classify(shared: &Mutex<Discovered>, dir: &Path) {
    let path = dir.to_path_buf();
    match repo::classify_dot_git(&dir.join(".git")) {
        DotGit::MainCheckout => record(shared, |d| d.hits.push(Hit::Repository { root: path })),
        DotGit::LinkedWorktree { gitdir } => {
            record(shared, |d| d.hits.push(Hit::Worktree { path, gitdir }))
        }
        DotGit::OrphanedWorktree { gitdir } => {
            record(shared, |d| d.hits.push(Hit::Orphan { path, gitdir }))
        }
        // A submodule checkout is a working tree, but never a purgeable one.
        DotGit::Submodule { .. } | DotGit::Unknown => {}
    }
}

fn record(shared: &Mutex<Discovered>, f: impl FnOnce(&mut Discovered)) {
    // A sibling thread panicking must not escalate into a failed scan.
    f(&mut shared.lock().unwrap_or_else(PoisonError::into_inner));
}

/// Dig the path out of a walk error, through whatever context wrappers the
/// `ignore` crate added.
fn error_path(err: &ignore::Error) -> Option<&Path> {
    match err {
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            error_path(err)
        }
        ignore::Error::Loop { child, .. } => Some(child),
        _ => None,
    }
}

/// Everything known about one repository before git has been consulted.
#[derive(Debug, Default)]
struct Candidate {
    /// The main checkout, when the walk found it.
    root: Option<PathBuf>,
    /// Directories we can run git from, best first.
    probes: Vec<PathBuf>,
    /// Worktree paths the walk saw directly.
    from_walk: BTreeSet<PathBuf>,
}

/// Phase 2: turn raw hits into repository groups.
fn resolve(scan_root: &Path, discovered: Discovered, result: &mut ScanResult) {
    let Discovered { hits, skipped } = discovered;

    // Keyed by git common dir, which every member of a repository agrees on.
    let mut candidates: BTreeMap<PathBuf, Candidate> = BTreeMap::new();
    let mut orphans: Vec<(PathBuf, PathBuf)> = Vec::new();

    for hit in hits {
        match hit {
            Hit::Repository { root } => {
                let root = repo::normalize(&root);
                let common = common_dir_of_repo(&root);
                let candidate = candidates.entry(common).or_default();
                candidate.probes.insert(0, root.clone());
                candidate.root = Some(root);
            }
            Hit::Worktree { path, gitdir } => {
                let path = repo::normalize(&path);
                match repo::common_dir_for_gitdir(&gitdir) {
                    Some(common) => {
                        let candidate = candidates.entry(repo::normalize(&common)).or_default();
                        candidate.probes.push(path.clone());
                        candidate.from_walk.insert(path);
                    }
                    None => result.warnings.push(format!(
                        "cannot determine the owning repository of {}",
                        path.display()
                    )),
                }
            }
            Hit::Orphan { path, gitdir } => {
                orphans.push((repo::normalize(&path), gitdir));
            }
        }
    }

    let mut groups: Vec<RepoGroup> = candidates
        .into_iter()
        .filter_map(|(common, candidate)| build_group(scan_root, common, candidate, result))
        .collect();

    attach_orphans(scan_root, orphans, &mut groups, result);

    // A repository with nothing purgeable in it is noise, not information.
    groups.retain(|group| !group.worktrees.is_empty());

    for group in &mut groups {
        group
            .worktrees
            .sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
    }
    groups.sort_by(|a, b| (&a.name, &a.root).cmp(&(&b.name, &b.root)));

    let mut skipped = skipped;
    // A permission failure is normally seen twice: once by the walker's
    // readdir and once by our own `.git` probe.
    skipped.sort();
    skipped.dedup();

    result.groups = groups;
    result.skipped = skipped;
}

/// The common dir of a discovered repository, with a filesystem fallback for
/// the case where git could not be run at all.
fn common_dir_of_repo(root: &Path) -> PathBuf {
    if let Some(common) = repo::git_common_dir(root) {
        return common;
    }
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        repo::normalize(&dot_git)
    } else {
        repo::normalize(root)
    }
}

fn build_group(
    scan_root: &Path,
    common_dir: PathBuf,
    candidate: Candidate,
    result: &mut ScanResult,
) -> Option<RepoGroup> {
    // One `git worktree list` per repository, not per worktree. Any member of
    // the repository reports the same list, so the first probe that answers
    // wins.
    let entries = candidate
        .probes
        .iter()
        .find_map(|probe| repo::list_worktrees(probe))
        .unwrap_or_default();

    if entries.is_empty() && candidate.from_walk.is_empty() {
        return None;
    }
    if entries.is_empty() && !candidate.probes.is_empty() {
        result.warnings.push(format!(
            "could not list worktrees of {}",
            common_dir.display()
        ));
    }

    // The main checkout is excluded twice over: by position (git always lists
    // it first) and by value.
    let derived_root = repo::normalize(&repo::repo_root_for_common_dir(&common_dir));
    let listed_root = entries.first().map(|e| repo::normalize(&e.path));
    let root = candidate
        .root
        .or_else(|| listed_root.clone())
        .unwrap_or_else(|| derived_root.clone());

    let mut group = RepoGroup::new(root.clone(), common_dir);
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();

    for (index, entry) in entries.iter().enumerate() {
        if index == 0 || entry.bare {
            continue;
        }
        let path = repo::normalize(&entry.path);
        if path == derived_root || path == root || Some(&path) == listed_root.as_ref() {
            continue;
        }
        // Defence in depth: `git worktree list` run inside a submodule reports
        // a path under `.git/modules/...`, which must never become a purge
        // target.
        if repo::is_inside_git_dir(&path) {
            continue;
        }
        if !seen.insert(path.clone()) {
            continue;
        }
        group
            .worktrees
            .push(worktree_from_entry(scan_root, path, entry));
    }

    // Anything the walk saw that git did not mention — a copied worktree
    // directory, or any repo we could not run git in.
    for path in &candidate.from_walk {
        if *path == derived_root || *path == root || seen.contains(path) {
            continue;
        }
        seen.insert(path.clone());
        let mut worktree = Worktree::new(path.clone());
        worktree.outside_scan_root = !path.starts_with(scan_root);
        group.worktrees.push(worktree);
    }

    Some(group)
}

fn worktree_from_entry(scan_root: &Path, path: PathBuf, entry: &WorktreeEntry) -> Worktree {
    let outside_scan_root = !path.starts_with(scan_root);
    let mut worktree = Worktree::new(path);
    worktree.branch = entry.branch.clone();
    worktree.head = entry.head.as_deref().map(repo::short_sha);
    worktree.locked = entry.locked.clone();
    worktree.prunable = entry.prunable;
    worktree.outside_scan_root = outside_scan_root;
    worktree
}

/// Place orphaned worktrees, either into the live group that turns out to own
/// them or into a group of their own flagged `orphaned`.
fn attach_orphans(
    scan_root: &Path,
    orphans: Vec<(PathBuf, PathBuf)>,
    groups: &mut Vec<RepoGroup>,
    result: &mut ScanResult,
) {
    let mut orphan_groups: BTreeMap<PathBuf, RepoGroup> = BTreeMap::new();

    for (path, gitdir) in orphans {
        // `normalize` degrades to a lexical fold when the repository is gone,
        // and canonicalises when it survives — which is what lets an orphan
        // rejoin its live group.
        let Some(common) = repo::common_dir_for_gitdir(&gitdir).map(|c| repo::normalize(&c)) else {
            result.warnings.push(format!(
                "cannot determine the owning repository of orphaned worktree {}",
                path.display()
            ));
            continue;
        };

        let mut worktree = Worktree::new(path.clone());
        worktree.orphaned = true;
        worktree.outside_scan_root = !path.starts_with(scan_root);

        // The repository may still exist with only its `worktrees/<name>`
        // metadata removed; then the orphan belongs to the live group.
        if let Some(group) = groups.iter_mut().find(|g| g.git_common_dir == common) {
            if !group.worktrees.iter().any(|w| w.path == path) {
                group.worktrees.push(worktree);
            }
            continue;
        }

        let group = orphan_groups.entry(common.clone()).or_insert_with(|| {
            let root = repo::repo_root_for_common_dir(&common);
            let mut group = RepoGroup::new(root, common.clone());
            group.orphaned = true;
            group
        });
        if !group.worktrees.iter().any(|w| w.path == path) {
            group.worktrees.push(worktree);
        }
    }

    groups.extend(orphan_groups.into_values());
}
