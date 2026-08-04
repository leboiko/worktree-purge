//! Recursive disk usage — allocated size, i.e. what is actually reclaimed.

use std::fs::{Metadata, ReadDir};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use filesize::PathExt;

use crate::model::SizeState;

/// Directory entries processed between cancellation checks.
///
/// Small enough that a cancel lands promptly even inside one huge directory,
/// large enough that the atomic load is lost in the noise of a `stat` syscall.
const CANCEL_CHECK_INTERVAL: usize = 128;

/// Sum the on-disk (allocated) size of everything under `path`.
///
/// Uses allocated blocks rather than apparent length so sparse and
/// block-padded files are reported the way `du` reports them.
///
/// Returns [`SizeState::Unknown`] only when the *root* cannot be read; entries
/// below a readable root that cannot be stat'd are skipped, so one
/// permission-denied file never throws away the whole measurement.
pub fn dir_size(path: &Path) -> SizeState {
    static NEVER: AtomicBool = AtomicBool::new(false);
    // The walk can only bail out early when the flag is set, and this one never
    // is, so the `None` arm is unreachable here.
    dir_size_cancellable(path, &NEVER).unwrap_or(SizeState::Unknown)
}

/// [`dir_size`] that gives up as soon as `cancel` is set.
///
/// Returns `None` when it stopped early: a partial sum must never be reported
/// as a measurement, because the UI would show a wrong (too small) number of
/// bytes to reclaim.
pub(crate) fn dir_size_cancellable(path: &Path, cancel: &AtomicBool) -> Option<SizeState> {
    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    // The root is resolved *through* symlinks — the caller named this path
    // explicitly and means the thing it points at. Entries below it are not
    // (see `entry_size`), so no symlink cycle can trap the walk.
    let Ok(root_meta) = std::fs::metadata(path) else {
        return Some(SizeState::Unknown);
    };
    let mut total = entry_size(path, &root_meta);
    if !root_meta.is_dir() {
        return Some(SizeState::Ready(total));
    }

    let mut stack: Vec<PathBuf> = Vec::new();
    let mut budget = CANCEL_CHECK_INTERVAL;

    // A root we cannot even open is a *failed* measurement, not an empty one.
    let Ok(entries) = std::fs::read_dir(path) else {
        return Some(SizeState::Unknown);
    };
    total = total.saturating_add(scan_dir(entries, &mut stack, cancel, &mut budget)?);

    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        // An unreadable *sub*directory is skipped: sum what we can see.
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        total = total.saturating_add(scan_dir(entries, &mut stack, cancel, &mut budget)?);
    }

    Some(SizeState::Ready(total))
}

/// Sum one directory level, pushing subdirectories onto `stack`.
///
/// `None` means cancelled. The walk is iterative rather than recursive so a
/// pathological directory depth cannot blow the worker thread's stack.
fn scan_dir(
    entries: ReadDir,
    stack: &mut Vec<PathBuf>,
    cancel: &AtomicBool,
    budget: &mut usize,
) -> Option<u64> {
    let mut total = 0u64;
    for entry in entries.flatten() {
        *budget -= 1;
        if *budget == 0 {
            *budget = CANCEL_CHECK_INTERVAL;
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
        }
        // `DirEntry::metadata` does not traverse symlinks, so a link is
        // measured as itself and a directory symlink is never descended into.
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let child = entry.path();
        total = total.saturating_add(entry_size(&child, &meta));
        if meta.is_dir() {
            stack.push(child);
        }
    }
    Some(total)
}

/// Allocated size of a single entry, directories included (`du` counts them).
fn entry_size(path: &Path, meta: &Metadata) -> u64 {
    // On Windows `size_on_disk_fast` canonicalises the path, which resolves
    // reparse points: a symlink would be charged its target's allocation and
    // the target would then be counted a second time. Unix reads the link's own
    // blocks (normally zero — short targets live inline in the inode), which is
    // what `du` reports.
    if cfg!(windows) && meta.is_symlink() {
        return 0;
    }
    path.size_on_disk_fast(meta).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_root_is_unknown_not_zero() {
        let missing = std::env::temp_dir().join("worktree-purge-does-not-exist-9f3c1a");
        assert_eq!(dir_size(&missing), SizeState::Unknown);
    }

    #[test]
    fn a_single_file_root_reports_its_own_allocation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("blob.bin");
        std::fs::write(&file, vec![7u8; 16 * 1024]).expect("write");

        let SizeState::Ready(bytes) = dir_size(&file) else {
            panic!("expected a measurement for a readable file");
        };
        assert!(
            bytes >= 16 * 1024,
            "allocation must cover the 16 KiB written, got {bytes}"
        );
    }

    #[test]
    fn an_already_cancelled_walk_reports_no_measurement() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("a"), b"x").expect("write");
        let cancel = AtomicBool::new(true);
        assert_eq!(dir_size_cancellable(dir.path(), &cancel), None);
    }
}
