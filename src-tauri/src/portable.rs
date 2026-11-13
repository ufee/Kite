//! Portable mode: a `data` folder next to the executable keeps everything the app writes.
//!
//! The portable archive ships that folder; the installer does not, so an installed copy keeps
//! the usual per-user folders. Secrets stay in the OS keychain and host keys in `~/.ssh/known_hosts`
//! in both modes: they belong to the user, not to a copy of the app.

use std::path::PathBuf;

/// Folder name next to the executable that turns portable mode on.
const DATA: &str = "data";

/// The portable data folder, when the executable has one beside it.
pub fn dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join(DATA);
    dir.is_dir().then_some(dir)
}
