//! Scanner integration tests.
//!
//! These build **real** git repositories, because the whole milestone is about
//! behaving correctly against real git metadata. Assertions pin exact sets of
//! names and exact paths rather than counts alone: a stub that returned one
//! plausible entry, or that dropped everything, must fail here.

mod common;

use std::path::{Path, PathBuf};

use common::{Fixture, git, orphan};
use worktree_purge::model::{ActivityState, RepoGroup, ScanResult, SizeState, Worktree};
use worktree_purge::scan::repo::{
    self, DotGit, WorktreeEntry, parse_worktree_list, resolve_gitdir_pointer,
};
use worktree_purge::scan::scan;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn group_names(result: &ScanResult) -> Vec<&str> {
    result.groups.iter().map(|g| g.name.as_str()).collect()
}

fn worktree_names(group: &RepoGroup) -> Vec<&str> {
    group.worktrees.iter().map(|w| w.name.as_str()).collect()
}

fn group<'a>(result: &'a ScanResult, name: &str) -> &'a RepoGroup {
    let mut matching = result.groups.iter().filter(|g| g.name == name);
    let found = matching.next().unwrap_or_else(|| {
        panic!(
            "no group named {name:?}; scan produced {:?}",
            group_names(result)
        )
    });
    assert!(
        matching.next().is_none(),
        "expected exactly one group named {name:?}"
    );
    found
}

fn worktree<'a>(group: &'a RepoGroup, name: &str) -> &'a Worktree {
    group
        .worktrees
        .iter()
        .find(|w| w.name == name)
        .unwrap_or_else(|| {
            panic!(
                "no worktree named {name:?} in {:?}; found {:?}",
                group.name,
                worktree_names(group)
            )
        })
}

fn all_worktree_paths(result: &ScanResult) -> Vec<PathBuf> {
    result
        .groups
        .iter()
        .flat_map(|g| g.worktrees.iter().map(|w| w.path.clone()))
        .collect()
}

/// Every path the scan surfaced, for "nothing under `.git` is ever reported".
fn all_reported_paths(result: &ScanResult) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for group in &result.groups {
        paths.push(group.root.clone());
        paths.extend(group.worktrees.iter().map(|w| w.path.clone()));
    }
    paths.extend(result.skipped.iter().cloned());
    paths
}

fn mkdir(path: &Path) {
    std::fs::create_dir_all(path).unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
}

fn write(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// porcelain parsing — literal strings, no filesystem
// ---------------------------------------------------------------------------

#[test]
fn porcelain_records_are_parsed_field_by_field() {
    let output = "\
worktree /dev/proj
HEAD de0def2d4d28dc65e721dcf32a986c3ae8e610d1
branch refs/heads/main

worktree /dev/wt-detached
HEAD aaaabbbbccccddddeeeeffff0000111122223333
detached

worktree /dev/wt-locked
HEAD de0def2d4d28dc65e721dcf32a986c3ae8e610d1
branch refs/heads/locked
locked in use by CI

worktree /dev/wt-plainlock
HEAD de0def2d4d28dc65e721dcf32a986c3ae8e610d1
branch refs/heads/plain
locked

worktree /dev/wt-gone
HEAD de0def2d4d28dc65e721dcf32a986c3ae8e610d1
branch refs/heads/gone
prunable gitdir file points to non-existent location
";

    let entries = parse_worktree_list(output);
    assert_eq!(entries.len(), 5);

    assert_eq!(
        entries[0],
        WorktreeEntry {
            path: PathBuf::from("/dev/proj"),
            head: Some("de0def2d4d28dc65e721dcf32a986c3ae8e610d1".into()),
            // `refs/heads/` is stripped.
            branch: Some("main".into()),
            bare: false,
            detached: false,
            locked: None,
            prunable: false,
        }
    );
    assert_eq!(
        entries[1],
        WorktreeEntry {
            path: PathBuf::from("/dev/wt-detached"),
            head: Some("aaaabbbbccccddddeeeeffff0000111122223333".into()),
            branch: None,
            bare: false,
            detached: true,
            locked: None,
            prunable: false,
        }
    );
    assert_eq!(entries[2].locked.as_deref(), Some("in use by CI"));
    assert_eq!(entries[2].branch.as_deref(), Some("locked"));

    // Locked with no reason must stay distinguishable from "not locked".
    assert_eq!(entries[3].locked.as_deref(), Some(""));
    assert!(entries[3].locked.is_some());

    assert!(entries[4].prunable);
    assert_eq!(entries[4].branch.as_deref(), Some("gone"));
    assert!(!entries[0].prunable);
}

#[test]
fn porcelain_bare_record_has_no_head_or_branch() {
    let output = "\
worktree /dev/proj.git
bare

worktree /dev/wt-one
HEAD de0def2d4d28dc65e721dcf32a986c3ae8e610d1
branch refs/heads/one
";
    let entries = parse_worktree_list(output);
    assert_eq!(entries.len(), 2);
    assert!(entries[0].bare);
    assert_eq!(entries[0].path, PathBuf::from("/dev/proj.git"));
    assert_eq!(entries[0].head, None);
    assert_eq!(entries[0].branch, None);
    assert!(!entries[1].bare);
}

#[test]
fn porcelain_lock_reasons_are_unquoted() {
    // git C-quotes a reason that contains control characters and emits it on a
    // single line.
    let output = "worktree /dev/wt\nlocked \"first line\\nsecond line\"\n";
    let entries = parse_worktree_list(output);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].locked.as_deref(),
        Some("first line\nsecond line")
    );
}

#[test]
fn porcelain_paths_are_taken_verbatim() {
    // git does not quote worktree paths, not even for spaces, tabs or
    // non-ASCII, so nothing may be trimmed or unescaped.
    let output = "worktree /dev/wt with space\n\nworktree /dev/wt\ttab\n\nworktree /dev/wt-café\n";
    let entries = parse_worktree_list(output);
    assert_eq!(
        entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
        vec![
            PathBuf::from("/dev/wt with space"),
            PathBuf::from("/dev/wt\ttab"),
            PathBuf::from("/dev/wt-café"),
        ]
    );
}

#[test]
fn porcelain_tolerates_malformed_and_empty_input() {
    assert_eq!(parse_worktree_list(""), Vec::new());
    assert_eq!(parse_worktree_list("\n\n\n"), Vec::new());

    // Attributes before any `worktree` line have no record to attach to, and
    // unknown attributes must not abort parsing of the rest.
    let output = "\
branch refs/heads/orphaned-attribute
worktree /dev/proj
HEAD de0def2d4d28dc65e721dcf32a986c3ae8e610d1
some-future-attribute whatever
branch refs/heads/main";
    let entries = parse_worktree_list(output);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, PathBuf::from("/dev/proj"));
    assert_eq!(entries[0].branch.as_deref(), Some("main"));
}

#[test]
fn porcelain_keeps_non_branch_refs_intact() {
    // Only the `refs/heads/` prefix is stripped.
    let entries = parse_worktree_list("worktree /dev/wt\nbranch refs/heads/feature/nested\n");
    assert_eq!(entries[0].branch.as_deref(), Some("feature/nested"));
}

// ---------------------------------------------------------------------------
// gitdir pointers
// ---------------------------------------------------------------------------

#[test]
fn gitdir_pointers_resolve_absolute_and_relative_targets() {
    let fx = Fixture::new();

    // An absolute target is taken as-is. What counts as absolute is
    // platform-specific: `/dev/proj` is a *relative* path on Windows, so
    // hard-coding it there would silently exercise the relative branch below
    // instead of this one.
    let absolute_target = if cfg!(windows) {
        "C:/dev/proj/.git/worktrees/abs"
    } else {
        "/dev/proj/.git/worktrees/abs"
    };
    let absolute = fx.join("abs/.git");
    mkdir(absolute.parent().expect("parent"));
    write(&absolute, &format!("gitdir: {absolute_target}\n"));
    assert_eq!(
        resolve_gitdir_pointer(&absolute),
        Some(PathBuf::from(absolute_target))
    );

    // Relative targets — how git writes submodule pointers — resolve against
    // the `.git` file's own directory, with `..` folded.
    let relative = fx.join("super/vendor/lib/.git");
    mkdir(relative.parent().expect("parent"));
    write(&relative, "gitdir: ../../.git/modules/vendor/lib\n");
    assert_eq!(
        resolve_gitdir_pointer(&relative),
        Some(fx.join("super/.git/modules/vendor/lib"))
    );
}

#[test]
fn non_pointer_files_are_rejected() {
    let fx = Fixture::new();

    let garbage = fx.join("junk/.git");
    mkdir(garbage.parent().expect("parent"));
    write(&garbage, "this is not a gitdir pointer\n");
    assert_eq!(resolve_gitdir_pointer(&garbage), None);

    let empty_target = fx.join("empty/.git");
    mkdir(empty_target.parent().expect("parent"));
    write(&empty_target, "gitdir:   \n");
    assert_eq!(resolve_gitdir_pointer(&empty_target), None);

    assert_eq!(resolve_gitdir_pointer(&fx.join("does/not/exist")), None);
}

// ---------------------------------------------------------------------------
// grouping
// ---------------------------------------------------------------------------

#[test]
fn worktrees_are_grouped_under_their_own_repository() {
    let fx = Fixture::new();
    let alpha = fx.repo("alpha");
    alpha.worktree("alpha-one");
    alpha.worktree("alpha-two");
    let beta = fx.repo("beta");
    beta.worktree("beta-only");

    let result = scan(&fx.root());

    assert_eq!(group_names(&result), ["alpha", "beta"]);
    assert_eq!(result.worktree_count(), 3);
    assert!(result.skipped.is_empty(), "skipped: {:?}", result.skipped);
    assert!(
        result.warnings.is_empty(),
        "warnings: {:?}",
        result.warnings
    );

    let alpha_group = group(&result, "alpha");
    assert_eq!(worktree_names(alpha_group), ["alpha-one", "alpha-two"]);
    assert_eq!(alpha_group.root, alpha.root);
    assert_eq!(alpha_group.git_common_dir, alpha.git_common_dir());
    assert!(!alpha_group.orphaned);

    let beta_group = group(&result, "beta");
    assert_eq!(worktree_names(beta_group), ["beta-only"]);
    assert_eq!(beta_group.git_common_dir, beta.git_common_dir());

    // Branch names, exact paths, and untouched enrichment state.
    for (name, expected_path) in [
        ("alpha-one", fx.join("alpha-one")),
        ("alpha-two", fx.join("alpha-two")),
    ] {
        let found = worktree(alpha_group, name);
        assert_eq!(found.path, expected_path);
        assert_eq!(found.branch.as_deref(), Some(name));
        assert!(!found.outside_scan_root);
        assert!(!found.orphaned);
        assert!(!found.prunable);
        assert_eq!(found.locked, None);
        assert_eq!(found.size, SizeState::Pending);
        assert_eq!(found.activity, ActivityState::Pending);
        assert!(!found.selected);
    }
}

#[test]
fn the_main_checkout_is_never_a_purgeable_worktree() {
    let fx = Fixture::new();
    let repo = fx.repo("proj");
    repo.worktree("wt-one");
    repo.worktree("wt-two");

    // git itself *does* list the main checkout: the exclusion has to be ours.
    let listed = repo.listed_worktrees();
    assert_eq!(listed.len(), 3);
    assert!(
        listed.contains(&repo.root),
        "fixture precondition: git lists the main checkout"
    );

    let result = scan(&fx.root());
    let proj = group(&result, "proj");

    assert_eq!(worktree_names(proj), ["wt-one", "wt-two"]);
    assert_eq!(proj.root, repo.root, "the main checkout is the group root");
    assert!(
        !all_worktree_paths(&result).contains(&repo.root),
        "the main checkout must never appear as a purge target"
    );
}

#[test]
fn a_worktree_is_reported_exactly_once() {
    let fx = Fixture::new();
    let repo = fx.repo("dup");
    let wt = repo.worktree("dup-wt");

    // Found by the filesystem walk *and* by `git worktree list`; the two must
    // collapse into a single entry.
    let result = scan(&fx.root());

    assert_eq!(result.groups.len(), 1);
    assert_eq!(all_worktree_paths(&result), vec![wt.path.clone()]);
    assert_eq!(result.worktree_count(), 1);
}

#[test]
fn worktrees_outside_the_scan_root_are_found_and_flagged() {
    let fx = Fixture::new();
    let elsewhere = Fixture::new();

    let repo = fx.repo("proj");
    repo.worktree("wt-inside");
    let far = repo.worktree_at(&elsewhere.join("wt-far"), "far");

    let result = scan(&fx.root());
    let proj = group(&result, "proj");

    assert_eq!(worktree_names(proj), ["wt-far", "wt-inside"]);

    let outside = worktree(proj, "wt-far");
    assert_eq!(outside.path, far.path);
    assert!(outside.outside_scan_root);
    assert_eq!(outside.branch.as_deref(), Some("far"));

    let inside = worktree(proj, "wt-inside");
    assert!(!inside.outside_scan_root);
    assert_eq!(inside.path, fx.join("wt-inside"));
}

#[test]
fn a_repository_outside_the_scan_root_is_reached_through_its_worktree() {
    // The scan root holds nothing but a worktree; the owning repository — and
    // therefore the whole group — can only be discovered by walking to it.
    // Both worktrees live in directories a default-configured walker would
    // filter out: one hidden, one matched by an `.ignore` file.
    let fx = Fixture::new();
    let elsewhere = Fixture::new();
    let hidden_owner = elsewhere.repo("hidden-owner");
    let ignored_owner = elsewhere.repo("ignored-owner");

    write(&fx.join(".ignore"), "ignored/\n");
    mkdir(&fx.join(".hidden/deep"));
    mkdir(&fx.join("ignored"));
    hidden_owner.worktree_at(&fx.join(".hidden/deep/wt-hidden"), "hidden");
    ignored_owner.worktree_at(&fx.join("ignored/wt-ignored"), "ignored");

    let result = scan(&fx.root());

    assert_eq!(group_names(&result), ["hidden-owner", "ignored-owner"]);
    assert_eq!(result.worktree_count(), 2);

    let hidden_group = group(&result, "hidden-owner");
    assert_eq!(worktree_names(hidden_group), ["wt-hidden"]);
    assert_eq!(hidden_group.root, hidden_owner.root);
    let hidden_wt = worktree(hidden_group, "wt-hidden");
    assert_eq!(hidden_wt.path, fx.join(".hidden/deep/wt-hidden"));
    assert!(!hidden_wt.outside_scan_root);

    let ignored_group = group(&result, "ignored-owner");
    assert_eq!(worktree_names(ignored_group), ["wt-ignored"]);
    assert_eq!(
        worktree(ignored_group, "wt-ignored").path,
        fx.join("ignored/wt-ignored")
    );
}

#[test]
fn scanning_a_repository_itself_lists_its_sibling_worktrees() {
    let fx = Fixture::new();
    let repo = fx.repo("proj");
    repo.worktree("wt-one");

    let result = scan(&repo.root);
    let proj = group(&result, "proj");

    assert_eq!(worktree_names(proj), ["wt-one"]);
    assert_eq!(proj.root, repo.root);
    // The worktree is a sibling of the scan root, so it really is outside it.
    assert!(worktree(proj, "wt-one").outside_scan_root);
}

// ---------------------------------------------------------------------------
// lock / detached / prunable state
// ---------------------------------------------------------------------------

#[test]
fn lock_and_detached_state_come_from_git() {
    let fx = Fixture::new();
    let repo = fx.repo("proj");

    let locked = repo.worktree("wt-locked");
    repo.git(&[
        "worktree",
        "lock",
        "--reason",
        "in use by CI",
        &s(&locked.path),
    ]);

    let silent = repo.worktree("wt-silentlock");
    repo.git(&["worktree", "lock", &s(&silent.path)]);

    let detached_path = fx.join("wt-detached");
    repo.git(&["worktree", "add", "--quiet", "--detach", &s(&detached_path)]);

    let head = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(head.len(), 40, "fixture precondition");

    let result = scan(&fx.root());
    let proj = group(&result, "proj");
    assert_eq!(
        worktree_names(proj),
        ["wt-detached", "wt-locked", "wt-silentlock"]
    );

    let locked_wt = worktree(proj, "wt-locked");
    assert_eq!(locked_wt.locked.as_deref(), Some("in use by CI"));
    assert_eq!(locked_wt.branch.as_deref(), Some("wt-locked"));

    // Locked without a reason is still locked, and stays distinguishable.
    let silent_wt = worktree(proj, "wt-silentlock");
    assert_eq!(silent_wt.locked.as_deref(), Some(""));

    let detached_wt = worktree(proj, "wt-detached");
    assert_eq!(detached_wt.branch, None, "a detached HEAD has no branch");
    assert_eq!(detached_wt.head.as_deref(), Some(&head[..7]));
    assert_eq!(detached_wt.locked, None);
    assert_eq!(detached_wt.path, detached_path);
}

#[test]
fn a_worktree_whose_directory_vanished_is_reported_as_prunable() {
    let fx = Fixture::new();
    let repo = fx.repo("proj");
    let gone = repo.worktree("wt-gone");
    repo.worktree("wt-live");

    std::fs::remove_dir_all(&gone.path).expect("remove worktree directory");

    let result = scan(&fx.root());
    let proj = group(&result, "proj");

    assert_eq!(worktree_names(proj), ["wt-gone", "wt-live"]);
    let vanished = worktree(proj, "wt-gone");
    assert!(vanished.prunable, "metadata-only worktree must be prunable");
    assert_eq!(vanished.path, gone.path);
    assert!(!vanished.orphaned);
    assert!(!worktree(proj, "wt-live").prunable);
}

// ---------------------------------------------------------------------------
// orphans
// ---------------------------------------------------------------------------

#[test]
fn a_worktree_whose_repository_was_deleted_is_an_orphan() {
    let fx = Fixture::new();
    let repo = fx.repo("ghost");
    let stranded = repo.worktree("ghost-wt");
    let repo_root = repo.root.clone();
    let common_dir = repo.git_common_dir();

    orphan(&repo_root);
    assert!(!repo_root.exists(), "fixture precondition");
    assert!(stranded.path.join(".git").is_file(), "the pointer survives");

    let result = scan(&fx.root());

    assert_eq!(result.groups.len(), 1);
    let ghost = &result.groups[0];
    assert!(ghost.orphaned, "the owning repository could not be located");
    assert_eq!(ghost.name, "ghost");
    // The intended repo root is derived from the dangling gitdir pointer.
    assert_eq!(ghost.root, repo_root);
    assert_eq!(ghost.git_common_dir, common_dir);

    assert_eq!(worktree_names(ghost), ["ghost-wt"]);
    let wt = &ghost.worktrees[0];
    assert!(wt.orphaned);
    assert_eq!(wt.path, stranded.path);
    assert!(!wt.outside_scan_root);
    assert_eq!(wt.branch, None, "git cannot be consulted for an orphan");
}

#[test]
fn an_orphan_whose_repository_survives_joins_that_repositorys_group() {
    let fx = Fixture::new();
    let repo = fx.repo("proj");
    let half_gone = repo.worktree("wt-half-gone");
    repo.worktree("wt-live");

    // Only the worktree's private metadata is destroyed; the repository stays.
    std::fs::remove_dir_all(repo.root.join(".git/worktrees/wt-half-gone"))
        .expect("remove worktree metadata");
    assert!(
        !repo.listed_worktrees().contains(&fx.join("wt-half-gone")),
        "fixture precondition: git no longer knows about it"
    );

    let result = scan(&fx.root());

    assert_eq!(group_names(&result), ["proj"]);
    let proj = group(&result, "proj");
    assert!(!proj.orphaned, "the repository itself is fine");
    assert_eq!(proj.git_common_dir, repo.git_common_dir());
    assert_eq!(worktree_names(proj), ["wt-half-gone", "wt-live"]);

    assert!(worktree(proj, "wt-half-gone").orphaned);
    assert_eq!(worktree(proj, "wt-half-gone").path, half_gone.path);
    assert!(!worktree(proj, "wt-live").orphaned);
}

// ---------------------------------------------------------------------------
// submodules
// ---------------------------------------------------------------------------

/// Add `lib` to `super` as a submodule at `rel`. Modern git refuses local
/// file-protocol submodules unless explicitly allowed.
fn add_submodule(super_root: &Path, lib_root: &Path, rel: &str) {
    git(
        super_root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            &s(lib_root),
            rel,
        ],
    );
    git(super_root, &["commit", "--quiet", "-m", "add submodule"]);
}

#[test]
fn a_submodule_checkout_is_never_a_worktree() {
    let fx = Fixture::new();
    let lib = fx.repo("lib");
    let superproject = fx.repo("super");
    add_submodule(&superproject.root, &lib.root, "vendor/lib");

    let submodule_dot_git = superproject.root.join("vendor/lib/.git");
    assert!(
        submodule_dot_git.is_file(),
        "fixture precondition: a submodule's .git is a file, like a worktree's"
    );

    // The pointer targets `.git/modules/...`, and that directory carries none
    // of the marker files a linked worktree's gitdir has.
    let target = resolve_gitdir_pointer(&submodule_dot_git).expect("submodule pointer resolves");
    assert_eq!(target, superproject.root.join(".git/modules/vendor/lib"));
    assert!(target.is_dir());
    assert!(
        !repo::is_worktree_gitdir(&target),
        "a module directory has no gitdir/commondir markers"
    );
    assert!(matches!(
        repo::classify_dot_git(&submodule_dot_git),
        DotGit::Submodule { .. }
    ));

    // Scanning the directory that *contains* the submodule checkout: a real
    // worktree sits next to it, so an empty result cannot pass this test.
    let other = fx.repo("other");
    other.worktree_at(&superproject.root.join("vendor/wt-real"), "real");

    let result = scan(&superproject.root.join("vendor"));

    assert_eq!(group_names(&result), ["other"]);
    assert_eq!(worktree_names(group(&result, "other")), ["wt-real"]);
    assert_eq!(result.worktree_count(), 1);
    assert!(
        !all_worktree_paths(&result).contains(&superproject.root.join("vendor/lib")),
        "the submodule checkout must never be offered for removal"
    );
}

// ---------------------------------------------------------------------------
// bare repositories
// ---------------------------------------------------------------------------

#[test]
fn a_bare_repository_owning_worktrees_is_handled() {
    let fx = Fixture::new();
    let seed = fx.repo("seed");
    let bare = fx.bare("proj.git");
    seed.git(&["remote", "add", "bare", &s(&bare.root)]);
    seed.git(&["push", "--quiet", "bare", "main"]);

    let first = bare.worktree("bare-one");
    bare.worktree("bare-two");

    let result = scan(&fx.root());

    // `seed` owns no worktrees, so it contributes no group.
    assert_eq!(group_names(&result), ["proj.git"]);
    let group = group(&result, "proj.git");

    // A bare repository *is* its own common dir.
    assert_eq!(group.root, bare.root);
    assert_eq!(group.git_common_dir, bare.root);
    assert!(!group.orphaned);

    assert_eq!(worktree_names(group), ["bare-one", "bare-two"]);
    assert_eq!(worktree(group, "bare-one").path, first.path);
    assert_eq!(
        worktree(group, "bare-one").branch.as_deref(),
        Some("bare-one")
    );
    assert!(
        !all_worktree_paths(&result).contains(&bare.root),
        "the bare repository itself is not purgeable"
    );
}

// ---------------------------------------------------------------------------
// pruning and noise
// ---------------------------------------------------------------------------

#[test]
fn the_walk_never_descends_into_repository_internals() {
    let fx = Fixture::new();
    let host = fx.repo("host");
    host.worktree("host-wt");

    // A complete repository, *with its own linked worktree*, buried inside the
    // host's `.git` directory. Only prune-on-hit keeps it out of the result:
    // a walker that descended would happily report `decoy` and `decoy-wt`.
    let decoy = host.root.join(".git/decoy");
    mkdir(&decoy);
    git(&decoy, &["init", "--quiet"]);
    write(&decoy.join("file.txt"), "decoy\n");
    git(&decoy, &["add", "."]);
    git(&decoy, &["commit", "--quiet", "-m", "decoy"]);
    let decoy_wt = host.root.join(".git/decoy-wt");
    git(
        &decoy,
        &["worktree", "add", "--quiet", "-b", "dw", &s(&decoy_wt)],
    );
    assert!(decoy_wt.join(".git").is_file(), "fixture precondition");

    let result = scan(&fx.root());

    assert_eq!(group_names(&result), ["host"]);
    assert_eq!(worktree_names(group(&result, "host")), ["host-wt"]);
    assert_eq!(result.worktree_count(), 1);

    for path in all_reported_paths(&result) {
        assert!(
            !path.components().any(|c| c.as_os_str() == ".git"),
            "{} lives inside a .git directory and must never be reported",
            path.display()
        );
    }
}

#[test]
fn directories_without_repositories_produce_no_groups() {
    let fx = Fixture::new();
    fx.plain_dir("notes");
    fx.plain_dir("nested/deeper/deepest");
    fx.plain_dir("build/artifacts");

    // A file named `.git` that is not a pointer at all.
    let bogus = fx.join("looks-like-a-worktree");
    mkdir(&bogus);
    write(&bogus.join(".git"), "gitdir\n");

    // A directory with a stray `HEAD` file, which must not read as bare.
    let decoy = fx.join("has-head");
    mkdir(&decoy);
    write(&decoy.join("HEAD"), "ref: refs/heads/main\n");

    // A dangling pointer into `modules/` is a broken submodule, not an orphan.
    let broken_submodule = fx.join("broken-submodule");
    mkdir(&broken_submodule);
    write(
        &broken_submodule.join(".git"),
        "gitdir: ../gone/.git/modules/broken-submodule\n",
    );

    let result = scan(&fx.root());

    assert!(
        result.groups.is_empty(),
        "groups: {:?}",
        group_names(&result)
    );
    assert_eq!(result.worktree_count(), 0);
    assert!(result.skipped.is_empty(), "skipped: {:?}", result.skipped);
    assert!(
        result.warnings.is_empty(),
        "warnings: {:?}",
        result.warnings
    );
}

#[test]
fn a_directory_that_merely_looks_like_a_bare_repo_is_still_walked() {
    // `HEAD` + `objects/` + `refs/` is only the cheap first filter; git has to
    // confirm. Here `HEAD` is garbage, so git refuses — and if the scanner
    // pruned on the structural check alone it would never reach the worktree
    // buried below, whose owning repository lives outside the scan root and so
    // cannot be reached any other way.
    let fx = Fixture::new();
    let elsewhere = Fixture::new();
    let owner = elsewhere.repo("owner");

    let decoy = fx.join("cache-dir");
    mkdir(&decoy.join("objects"));
    mkdir(&decoy.join("refs"));
    mkdir(&decoy.join("nested"));
    write(&decoy.join("HEAD"), "this is not a ref\n");
    assert!(!repo::is_bare_repo_dir(&decoy));

    owner.worktree_at(&decoy.join("nested/wt-deep"), "deep");

    let result = scan(&fx.root());

    assert_eq!(group_names(&result), ["owner"]);
    assert_eq!(worktree_names(group(&result, "owner")), ["wt-deep"]);
    assert_eq!(
        worktree(group(&result, "owner"), "wt-deep").path,
        decoy.join("nested/wt-deep")
    );
}

#[test]
fn a_missing_scan_root_yields_a_warning_not_a_panic() {
    let fx = Fixture::new();
    let result = scan(&fx.join("no/such/directory"));

    assert!(result.groups.is_empty());
    assert_eq!(result.warnings.len(), 1);
    assert!(
        result.warnings[0].contains("no/such/directory"),
        "warning should name the path: {:?}",
        result.warnings
    );
}

#[cfg(unix)]
#[test]
fn unreadable_directories_are_skipped_without_aborting_the_scan() {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    let fx = Fixture::new();
    let repo = fx.repo("proj");
    repo.worktree("wt-one");

    let forbidden = fx.join("no-entry");
    mkdir(&forbidden.join("child"));
    std::fs::set_permissions(&forbidden, Permissions::from_mode(0o000)).expect("chmod 000");

    // Running as root defeats the permission bits entirely.
    let enforced = std::fs::read_dir(&forbidden).is_err();
    let result = enforced.then(|| scan(&fx.root()));

    // Restore before asserting, so a failure still leaves a removable temp dir.
    std::fs::set_permissions(&forbidden, Permissions::from_mode(0o755)).expect("chmod 755");

    let Some(result) = result else {
        eprintln!("skipping: permissions are not enforced for this user");
        return;
    };

    assert_eq!(
        result.skipped,
        vec![forbidden],
        "the unreadable directory is reported exactly once"
    );
    // The rest of the tree is still scanned.
    assert_eq!(group_names(&result), ["proj"]);
    assert_eq!(worktree_names(group(&result, "proj")), ["wt-one"]);
}
