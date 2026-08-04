//! Shared fixture builder for integration tests.
//!
//! Builds *real* git repositories and worktrees in a temp directory, because
//! the whole point of this tool is behaving correctly against real git
//! metadata. Every helper panics loudly on failure — a broken fixture must not
//! be mistaken for a failing assertion.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// A path that is guaranteed not to exist, used to neutralise the user's
/// global git config.
///
/// Git treats an unreadable `GIT_CONFIG_GLOBAL` as empty configuration, which
/// is what we want: fixtures must not inherit the developer's `~/.gitconfig`
/// (a `status.showUntrackedFiles=no` there would silently break the dirty
/// probe). `/dev/null` is the usual idiom but is not a valid path on Windows,
/// which is a CI-tested tier for this project — a path that simply does not
/// exist works identically on every platform.
fn no_such_config() -> PathBuf {
    std::env::temp_dir().join("worktree-purge-fixture-no-global-config")
}

/// Run git in `dir`, panicking with full context on failure.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", no_such_config())
        // The documented switch for skipping system-level config; unlike
        // GIT_CONFIG_SYSTEM it needs no path at all.
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .args(["-c", "commit.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(["-c", "advice.detachedHead=false"])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?} in {}: {e}", dir.display()));

    assert!(
        out.status.success(),
        "git {args:?} in {} failed ({:?})\nstdout: {}\nstderr: {}",
        dir.display(),
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A temporary directory that acts as the scan root.
pub struct Fixture {
    dir: TempDir,
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Fixture {
    pub fn new() -> Self {
        Fixture {
            dir: tempfile::tempdir().expect("create temp dir"),
        }
    }

    /// The scan root. Canonicalised so comparisons against scanner output
    /// (which canonicalises too) hold on macOS, where `/var` is a symlink to
    /// `/private/var`.
    pub fn root(&self) -> PathBuf {
        self.dir
            .path()
            .canonicalize()
            .unwrap_or_else(|_| self.dir.path().to_path_buf())
    }

    pub fn join(&self, rel: &str) -> PathBuf {
        self.root().join(rel)
    }

    /// A repository with one commit on `main` and no remote.
    pub fn repo(&self, name: &str) -> Repo {
        let root = self.join(name);
        std::fs::create_dir_all(&root).expect("create repo dir");
        git(&root, &["init", "--quiet"]);
        let repo = Repo {
            root,
            parent: self.root(),
        };
        repo.write_file("README.md", "fixture\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "--quiet", "-m", "initial"]);
        repo
    }

    /// A repository whose `main` branch tracks a bare origin, so "unpushed"
    /// detection has something real to compare against.
    pub fn repo_with_origin(&self, name: &str) -> Repo {
        let origin = self.join(format!("{name}-origin.git").as_str());
        std::fs::create_dir_all(&origin).expect("create origin dir");
        git(&origin, &["init", "--bare", "--quiet"]);

        let repo = self.repo(name);
        repo.git(&["remote", "add", "origin", &origin.to_string_lossy()]);
        repo.git(&["push", "--quiet", "-u", "origin", "main"]);
        repo
    }

    /// A bare repository (can still own worktrees).
    pub fn bare(&self, name: &str) -> Repo {
        let root = self.join(name);
        std::fs::create_dir_all(&root).expect("create bare dir");
        git(&root, &["init", "--bare", "--quiet"]);
        Repo {
            root,
            parent: self.root(),
        }
    }

    /// A directory containing no repository at all — walk noise.
    pub fn plain_dir(&self, rel: &str) -> PathBuf {
        let p = self.join(rel);
        std::fs::create_dir_all(&p).expect("create plain dir");
        std::fs::write(p.join("notes.txt"), "not a repo\n").expect("write file");
        p
    }
}

/// A git repository inside a [`Fixture`].
pub struct Repo {
    pub root: PathBuf,
    /// The fixture root, where sibling worktrees are created.
    pub parent: PathBuf,
}

impl Repo {
    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn git(&self, args: &[&str]) -> String {
        git(&self.root, args)
    }

    pub fn write_file(&self, rel: &str, contents: &str) {
        let p = self.root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(p, contents).expect("write file");
    }

    /// The `.git` directory that owns worktree metadata.
    pub fn git_common_dir(&self) -> PathBuf {
        let out = self.git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]);
        PathBuf::from(out.trim())
            .canonicalize()
            .expect("canonicalise git common dir")
    }

    /// Add a linked worktree as a sibling of the repo, on a new branch.
    pub fn worktree(&self, name: &str) -> Worktree {
        self.worktree_on_branch(name, name)
    }

    pub fn worktree_on_branch(&self, name: &str, branch: &str) -> Worktree {
        let path = self.parent.join(name);
        self.git(&[
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
            &path.to_string_lossy(),
        ]);
        Worktree {
            path,
            repo_root: self.root.clone(),
        }
    }

    /// Add a linked worktree at an explicit path (used to place worktrees
    /// outside the scan root).
    pub fn worktree_at(&self, path: &Path, branch: &str) -> Worktree {
        self.git(&[
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
            &path.to_string_lossy(),
        ]);
        Worktree {
            path: path.to_path_buf(),
            repo_root: self.root.clone(),
        }
    }

    /// Paths reported by `git worktree list`, for cross-checking removals.
    pub fn listed_worktrees(&self) -> Vec<PathBuf> {
        self.git(&["worktree", "list", "--porcelain"])
            .lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .map(PathBuf::from)
            .collect()
    }
}

/// A linked worktree inside a [`Fixture`].
pub struct Worktree {
    pub path: PathBuf,
    pub repo_root: PathBuf,
}

impl Worktree {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn git(&self, args: &[&str]) -> String {
        git(&self.path, args)
    }

    pub fn write_file(&self, rel: &str, contents: &str) {
        let p = self.path.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(p, contents).expect("write file");
    }

    /// Leave an untracked file behind — dirty without any staging.
    pub fn make_untracked(&self) -> &Self {
        self.write_file("scratch.txt", "untracked work\n");
        self
    }

    /// Modify a tracked file — dirty in the working tree.
    pub fn make_modified(&self) -> &Self {
        self.write_file("README.md", "locally modified\n");
        self
    }

    /// Stage a change without committing — dirty in the index.
    pub fn make_staged(&self) -> &Self {
        self.write_file("staged.txt", "staged work\n");
        self.git(&["add", "staged.txt"]);
        self
    }

    /// Commit a new file. Without a push this leaves the branch ahead.
    pub fn commit(&self, msg: &str) -> &Self {
        self.write_file("work.txt", msg);
        self.git(&["add", "."]);
        self.git(&["commit", "--quiet", "-m", msg]);
        self
    }

    /// Push the current branch and set upstream, making it clean again.
    pub fn push(&self) -> &Self {
        let branch = self.branch();
        self.git(&["push", "--quiet", "-u", "origin", &branch]);
        self
    }

    /// Stash a change. The stash survives worktree removal, so this must not
    /// make a worktree count as active.
    pub fn stash(&self, msg: &str) -> &Self {
        self.write_file("stashed.txt", msg);
        self.git(&["add", "stashed.txt"]);
        self.git(&["stash", "push", "--quiet", "-m", msg]);
        self
    }

    pub fn branch(&self) -> String {
        self.git(&["rev-parse", "--abbrev-ref", "HEAD"])
            .trim()
            .to_string()
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }
}

/// Turn a linked worktree into an orphan by deleting the owning repository,
/// leaving the working directory (and its dangling `.git` file) behind.
pub fn orphan(repo_root: &Path) {
    std::fs::remove_dir_all(repo_root).expect("remove owning repo");
}
