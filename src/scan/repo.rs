//! Repository/worktree identity: `.git` back-pointers and `git worktree list`.
//!
//! Everything here answers one of two questions: *what is this `.git` entry?*
//! and *which repository does it belong to?* The answers come from two
//! independent sources — the on-disk metadata git writes for linked worktrees,
//! and `git worktree list --porcelain` — because neither alone is sufficient:
//! the filesystem cannot see worktrees outside the scan root, and git cannot
//! see worktrees whose owning repository has been deleted.
//!
//! See `docs/notes-scanner.md` for the empirical notes behind these rules.

use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::git;

/// Upper bound on the size of a `.git` pointer file we are willing to read.
/// Real ones are a single short line; anything larger is not a pointer.
const MAX_POINTER_BYTES: u64 = 8 * 1024;

/// One record from `git worktree list --porcelain`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: Option<String>,
    pub prunable: bool,
}

/// What a `.git` entry actually is.
///
/// The distinction between [`LinkedWorktree`](DotGit::LinkedWorktree) and
/// [`Submodule`](DotGit::Submodule) is the one that matters most: both are
/// `.git` *files*, but only the former may ever be offered for removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DotGit {
    /// A `.git` directory — the main checkout of a repository.
    MainCheckout,
    /// A `.git` file pointing at a live `<common>/worktrees/<name>`.
    LinkedWorktree { gitdir: PathBuf },
    /// A `.git` file whose target is gone. Pure disk waste.
    OrphanedWorktree { gitdir: PathBuf },
    /// A `.git` file pointing at `<super>/.git/modules/...`. Never purgeable.
    Submodule { gitdir: PathBuf },
    /// A `.git` entry we cannot make sense of. Pruned, never reported.
    Unknown,
}

/// Parse `git worktree list --porcelain` output.
///
/// The first record is always the main worktree (or the bare repository).
///
/// Records are separated by blank lines. Unknown attribute lines are ignored
/// rather than treated as errors, so a future git release adding an attribute
/// cannot break discovery.
pub fn parse_worktree_list(output: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;

    for line in output.lines() {
        if line.trim().is_empty() {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            continue;
        }

        // A `worktree` line always starts a new record. Paths are emitted
        // verbatim to end of line — git does not quote them, not even for
        // tabs or non-ASCII — so no trimming and no unquoting here.
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(WorktreeEntry {
                path: PathBuf::from(path),
                ..WorktreeEntry::default()
            });
            continue;
        }

        // Attribute lines before the first `worktree` line are malformed;
        // ignore them rather than inventing a record with no path.
        let Some(entry) = current.as_mut() else {
            continue;
        };

        if let Some(sha) = line.strip_prefix("HEAD ") {
            entry.head = Some(sha.trim().to_string());
        } else if let Some(reference) = line.strip_prefix("branch ") {
            let reference = reference.trim();
            let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
            entry.branch = Some(name.to_string());
        } else if line == "bare" {
            entry.bare = true;
        } else if line == "detached" {
            entry.detached = true;
        } else if line == "locked" {
            // Locked with no reason given: still locked.
            entry.locked = Some(String::new());
        } else if let Some(reason) = line.strip_prefix("locked ") {
            entry.locked = Some(unquote_c_style(reason));
        } else if line == "prunable" || line.starts_with("prunable ") {
            entry.prunable = true;
        }
    }
    if let Some(entry) = current.take() {
        entries.push(entry);
    }
    entries
}

/// Read the `gitdir: <path>` pointer out of a linked worktree's `.git` file.
///
/// Relative targets — which is how git writes submodule pointers — are
/// resolved against the `.git` file's parent directory. Returns `None` when the
/// file is missing, unreadable, oversized, not UTF-8, or not a pointer.
pub fn resolve_gitdir_pointer(dot_git_file: &Path) -> Option<PathBuf> {
    let mut text = String::new();
    File::open(dot_git_file)
        .ok()?
        .take(MAX_POINTER_BYTES)
        .read_to_string(&mut text)
        .ok()?;

    let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    if target.is_empty() {
        return None;
    }

    let target = Path::new(target);
    if target.is_absolute() {
        return Some(lexical_normalize(target));
    }

    // Canonicalise the anchor before folding `..` so the lexical fold has as
    // little chance as possible to cross a symlink.
    let parent = dot_git_file.parent()?;
    let parent = canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    Some(lexical_normalize(&parent.join(target)))
}

/// Classify the `.git` entry of a directory.
///
/// Takes the path *to* the `.git` entry, e.g. `<dir>/.git`.
pub fn classify_dot_git(dot_git: &Path) -> DotGit {
    let Ok(meta) = std::fs::metadata(dot_git) else {
        return DotGit::Unknown;
    };
    if meta.is_dir() {
        return DotGit::MainCheckout;
    }
    if !meta.is_file() {
        return DotGit::Unknown;
    }
    let Some(gitdir) = resolve_gitdir_pointer(dot_git) else {
        return DotGit::Unknown;
    };

    if is_worktree_gitdir(&gitdir) {
        return DotGit::LinkedWorktree { gitdir };
    }
    if gitdir.exists() {
        // A live target that is not a worktree gitdir. Submodule module
        // directories are the only common case and look exactly like a `.git`
        // directory, which is why the marker-file test above is the real
        // discriminator rather than any "does this look like a repo" check.
        return if has_component(&gitdir, "modules") {
            DotGit::Submodule { gitdir }
        } else {
            DotGit::Unknown
        };
    }
    // Dangling. Only claim it as purgeable waste when the target has the exact
    // shape git uses for worktrees; a dangling `modules/` pointer is a broken
    // submodule and must not be offered for deletion.
    if parent_is(&gitdir, "worktrees") {
        DotGit::OrphanedWorktree { gitdir }
    } else {
        DotGit::Unknown
    }
}

/// True when `gitdir` is the private directory of a *linked worktree*.
///
/// Linked worktrees get `<gitdir>/gitdir` (absolute path back to the `.git`
/// file) and `<gitdir>/commondir` (relative path to the shared `.git`).
/// Submodule module directories have neither.
pub fn is_worktree_gitdir(gitdir: &Path) -> bool {
    gitdir.join("gitdir").is_file() && gitdir.join("commondir").is_file()
}

/// The shared `.git` directory owning a worktree's private gitdir.
///
/// Prefers the `commondir` file (authoritative) and falls back to the
/// `<common>/worktrees/<name>` layout, which is the only option for a dangling
/// gitdir.
pub fn common_dir_for_gitdir(gitdir: &Path) -> Option<PathBuf> {
    if let Some(text) = read_capped(&gitdir.join("commondir")) {
        let rel = text.trim();
        if !rel.is_empty() {
            let rel = Path::new(rel);
            let joined = if rel.is_absolute() {
                rel.to_path_buf()
            } else {
                gitdir.join(rel)
            };
            return Some(lexical_normalize(&joined));
        }
    }
    if parent_is(gitdir, "worktrees") {
        return gitdir.parent()?.parent().map(Path::to_path_buf);
    }
    None
}

/// The working directory a repository's common dir belongs to.
///
/// `<repo>/.git` -> `<repo>`; a bare repository *is* its own common dir.
pub fn repo_root_for_common_dir(common_dir: &Path) -> PathBuf {
    if common_dir.file_name() == Some(OsStr::new(".git")) {
        common_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| common_dir.to_path_buf())
    } else {
        common_dir.to_path_buf()
    }
}

/// Ask git for the repository identity of `dir`.
///
/// This is the same value for the main checkout and for every linked worktree
/// of a repository, which is what makes it usable as a grouping key.
pub fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let out = git::stdout_line(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if out.is_empty() {
        None
    } else {
        Some(normalize(Path::new(&out)))
    }
}

/// True when `dir` is itself a bare repository (as opposed to merely living
/// inside one).
///
/// The cheap structural test runs first so that the two git subprocesses only
/// ever fire for plausible candidates.
pub fn is_bare_repo_dir(dir: &Path) -> bool {
    if !dir.join("HEAD").is_file() || !dir.join("objects").is_dir() || !dir.join("refs").is_dir() {
        return false;
    }
    // Without this equality check any subdirectory of a repository — or a
    // decoy directory holding a stray `HEAD` file — could be misread as a
    // repository of its own.
    let here = normalize(dir);
    if git_common_dir(dir) != Some(here) {
        return false;
    }
    git::stdout_line(dir, &["rev-parse", "--is-bare-repository"]).as_deref() == Some("true")
}

/// Run `git worktree list --porcelain` in `dir`.
///
/// `None` means git refused — most often because `dir` is an orphaned worktree
/// whose repository is gone, where git exits 128 with
/// `fatal: not a git repository`.
pub fn list_worktrees(dir: &Path) -> Option<Vec<WorktreeEntry>> {
    git::stdout(dir, &["worktree", "list", "--porcelain"]).map(|out| parse_worktree_list(&out))
}

/// Resolve a path to a stable, comparable form.
///
/// Canonicalises when possible — required on macOS, where a temp dir is
/// reached as `/var/...` but reported by git as `/private/var/...`. Paths that
/// do not exist (a prunable worktree's directory, an orphan's intended repo
/// root) fall back to canonicalising the parent, then to a lexical fold.
pub fn normalize(path: &Path) -> PathBuf {
    if let Ok(resolved) = canonicalize(path) {
        return resolved;
    }
    let lexical = lexical_normalize(path);
    match (lexical.parent(), lexical.file_name()) {
        (Some(parent), Some(name)) => match canonicalize(parent) {
            Ok(parent) => parent.join(name),
            Err(_) => lexical,
        },
        _ => lexical,
    }
}

/// [`Path::canonicalize`] with Windows' verbatim prefix removed.
///
/// **Always use this rather than `Path::canonicalize` directly.** A `\\?\`
/// path that reaches git makes it exit 128, and since every path this tool
/// resolves eventually reaches git, a single raw call is enough to break
/// removal on Windows. See `docs/notes-windows.md`.
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    path.canonicalize().map(strip_verbatim)
}

/// Drop Windows' `\\?\` verbatim prefix from a canonicalised path.
///
/// `Path::canonicalize` returns verbatim paths on Windows, and git — which is
/// MSYS-based there — cannot parse them: handing `\\?\C:\dev\wt` to
/// `git worktree add`/`remove` fails with exit 128. Since every path this tool
/// hands to git comes through here, leaving the prefix on would break removal
/// entirely on Windows, not merely display it oddly.
///
/// Only drive paths are simplified. `\\?\UNC\server\share` keeps its prefix,
/// because stripping it there changes which path is named. The one thing given
/// up is the verbatim form's exemption from the legacy 260-character limit,
/// which does not apply to the modern Windows versions this targets.
#[cfg(windows)]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    // Only paths that are valid UTF-8 are touched: a lossy conversion could
    // silently change the path we later hand to a delete.
    let Some(text) = path.to_str() else {
        return path;
    };
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return path;
    };
    let bytes = rest.as_bytes();
    let is_drive_path =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
    if is_drive_path {
        PathBuf::from(rest)
    } else {
        path
    }
}

#[cfg(not(windows))]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    path
}

/// Fold `.` and `..` textually, without touching the filesystem.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // `/..` is `/`; there is nothing above a root or a prefix.
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// True when any component of `path` is exactly `.git`.
///
/// Used to guarantee repository internals are never offered as purge targets,
/// including via the odd paths git reports from inside a submodule.
pub fn is_inside_git_dir(path: &Path) -> bool {
    has_component(path, ".git")
}

/// The 7-character short form of a sha, as [`crate::model::Worktree::head`]
/// documents.
pub fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

fn has_component(path: &Path, name: &str) -> bool {
    path.components().any(|c| c.as_os_str() == OsStr::new(name))
}

fn parent_is(path: &Path, name: &str) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|n| n == OsStr::new(name))
}

fn read_capped(path: &Path) -> Option<String> {
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(MAX_POINTER_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// Undo git's C-style quoting.
///
/// git wraps a value in double quotes and escapes it only when it contains
/// control characters — a multi-line `worktree lock --reason` is the case that
/// actually occurs. Anything not wrapped in quotes is returned untouched.
fn unquote_c_style(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return raw.to_string();
    }
    let inner = &bytes[1..bytes.len() - 1];

    let mut out: Vec<u8> = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let byte = inner[i];
        i += 1;
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        let Some(&escape) = inner.get(i) else {
            // Trailing backslash: keep it rather than losing a character.
            out.push(b'\\');
            break;
        };
        i += 1;
        match escape {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'v' => out.push(0x0b),
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            b'0'..=b'7' => {
                let mut value = u32::from(escape - b'0');
                let mut digits = 1;
                while digits < 3 && inner.get(i).is_some_and(|b| matches!(b, b'0'..=b'7')) {
                    value = value * 8 + u32::from(inner[i] - b'0');
                    i += 1;
                    digits += 1;
                }
                out.push(value as u8);
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquote_leaves_plain_reasons_alone() {
        assert_eq!(unquote_c_style("in use by CI"), "in use by CI");
        // A leading quote without a trailing one is not a quoted string.
        assert_eq!(unquote_c_style("\"unbalanced"), "\"unbalanced");
        assert_eq!(unquote_c_style(""), "");
    }

    #[test]
    fn unquote_decodes_git_escapes() {
        assert_eq!(
            unquote_c_style(r#""first line\nsecond line""#),
            "first line\nsecond line"
        );
        assert_eq!(unquote_c_style(r#""a\tb""#), "a\tb");
        assert_eq!(unquote_c_style(r#""say \"hi\"""#), "say \"hi\"");
        assert_eq!(unquote_c_style(r#""back\\slash""#), "back\\slash");
        // Octal escapes: \303\251 is UTF-8 for 'é'.
        assert_eq!(unquote_c_style(r#""caf\303\251""#), "café");
    }

    #[test]
    fn lexical_normalize_folds_dot_and_dotdot() {
        assert_eq!(
            lexical_normalize(Path::new("/a/b/c/../../d/./e")),
            PathBuf::from("/a/d/e")
        );
        // Two `..` really do climb two levels, not one.
        assert_eq!(
            lexical_normalize(Path::new("/a/b/../../c")),
            PathBuf::from("/c")
        );
        // The submodule pointer shape, resolved from the checkout directory.
        assert_eq!(
            lexical_normalize(Path::new("/s/vendor/lib/../../.git/modules/vendor/lib")),
            PathBuf::from("/s/.git/modules/vendor/lib")
        );
        // Nothing lives above the root.
        assert_eq!(lexical_normalize(Path::new("/../..")), PathBuf::from("/"));
        // Relative paths keep leading `..` because there is nothing to fold.
        assert_eq!(lexical_normalize(Path::new("../x")), PathBuf::from("../x"));
    }

    #[test]
    fn repo_root_strips_only_a_dot_git_suffix() {
        assert_eq!(
            repo_root_for_common_dir(Path::new("/dev/proj/.git")),
            PathBuf::from("/dev/proj")
        );
        // A bare repo is its own root, even when named like a git dir.
        assert_eq!(
            repo_root_for_common_dir(Path::new("/dev/proj.git")),
            PathBuf::from("/dev/proj.git")
        );
    }

    #[test]
    fn short_sha_is_seven_characters() {
        assert_eq!(
            short_sha("de0def2d4d28dc65e721dcf32a986c3ae8e610d1"),
            "de0def2"
        );
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn git_dir_components_are_detected_exactly() {
        assert!(is_inside_git_dir(Path::new("/a/.git/worktrees/x")));
        // A bare repo directory named `proj.git` is not inside a `.git` dir.
        assert!(!is_inside_git_dir(Path::new("/a/proj.git/worktrees")));
        assert!(!is_inside_git_dir(Path::new("/a/b/c")));
    }
}

#[cfg(test)]
mod verbatim_tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn a_verbatim_drive_path_loses_its_prefix() {
        // git cannot parse `\\?\C:\...`, so normalize must never emit one.
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\dev\wt")),
            PathBuf::from(r"C:\dev\wt")
        );
    }

    #[test]
    #[cfg(windows)]
    fn a_verbatim_unc_path_keeps_its_prefix() {
        // Stripping here would change which path is named, so it is left be
        // even though git cannot use it either.
        let unc = PathBuf::from(r"\\?\UNC\server\share\wt");
        assert_eq!(strip_verbatim(unc.clone()), unc);
    }

    #[test]
    #[cfg(windows)]
    fn an_ordinary_path_is_untouched() {
        let p = PathBuf::from(r"C:\dev\wt");
        assert_eq!(strip_verbatim(p.clone()), p);
    }

    #[test]
    fn normalize_never_emits_a_verbatim_prefix() {
        // The property that actually matters, asserted on a real canonicalised
        // path: whatever the platform hands back, what leaves `normalize` must
        // be something git can parse. Passes trivially on Unix; on Windows it
        // fails against a `normalize` that forwards `canonicalize` unchanged.
        let dir = tempfile::tempdir().expect("temp dir");
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).expect("create dirs");

        let normalized = normalize(&nested);
        let text = normalized.to_string_lossy();
        assert!(
            !text.starts_with(r"\\?\"),
            "normalize returned a verbatim path git cannot parse: {text}"
        );
        assert!(
            normalized.ends_with("b"),
            "normalize mangled the path: {text}"
        );
    }
}
