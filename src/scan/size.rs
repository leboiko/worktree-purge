//! Recursive disk usage — allocated size, i.e. what is actually reclaimed.

use std::path::Path;

use crate::model::SizeState;

/// Sum the on-disk (allocated) size of everything under `path`.
///
/// Uses allocated blocks rather than apparent length so sparse and
/// block-padded files are reported the way `du` reports them.
pub fn dir_size(_path: &Path) -> SizeState {
    todo!("scan::size::dir_size")
}
