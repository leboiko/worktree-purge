//! Thin wrapper around the `git` CLI.
//!
//! We shell out rather than link a git library: if worktrees exist then git is
//! installed, and `git worktree remove`/`prune` perform the metadata cleanup
//! that a raw `rm -rf` would leave dangling.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

/// Outcome of a git invocation that is allowed to fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl GitOutput {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }

    /// First non-empty line of stderr, for compact error reporting in the UI.
    pub fn error_summary(&self) -> String {
        self.stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("git command failed")
            .to_string()
    }
}

fn base_command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        // Deterministic, machine-readable output regardless of user locale.
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        // Never let a pager or an editor block a background worker.
        .env("GIT_PAGER", "cat")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("--no-pager")
        .stdin(Stdio::null());
    cmd
}

/// Run git in `dir`, capturing output. Never fails on a non-zero exit status —
/// inspect [`GitOutput::success`].
///
/// Returns `Err` only when git could not be executed at all.
pub fn run<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> std::io::Result<GitOutput> {
    let out = base_command(dir).args(args).output()?;
    Ok(GitOutput {
        status: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Run git and return stdout, or `None` if git failed or could not run.
///
/// Use for probes where "no answer" and "error" are handled the same way.
pub fn stdout<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Option<String> {
    match run(dir, args) {
        Ok(o) if o.success() => Some(o.stdout),
        _ => None,
    }
}

/// Trimmed single-line stdout of a git command.
pub fn stdout_line<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Option<String> {
    stdout(dir, args).map(|s| s.trim().to_string())
}

/// True when the `git` binary can be executed at all.
pub fn available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_is_available_in_the_test_environment() {
        assert!(available(), "these tests require the git CLI");
    }

    #[test]
    fn failure_is_reported_not_thrown() {
        let dir = std::env::temp_dir();
        let out = run(&dir, &["rev-parse", "--definitely-not-a-flag"]).expect("git ran");
        assert!(!out.success());
        assert!(
            !out.error_summary().is_empty(),
            "a failed command must yield a usable message"
        );
    }
}
