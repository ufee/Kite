//! Portable mode: a `data` folder next to the executable keeps everything the app writes.
//!
//! The portable archive ships that folder with a marker file in it; the installer does not, so an
//! installed copy keeps the usual per-user folders. The marker, not the folder alone, turns the mode
//! on: an unpacker that drops empty folders cannot turn it off, and a `data` folder of some other
//! program next to the exe cannot turn it on. Secrets stay in the OS keychain and host keys in
//! `~/.ssh/known_hosts` in both modes: they belong to the user, not to a copy of the app.

use std::path::{Path, PathBuf};

/// Folder next to the executable that keeps the data.
const DATA: &str = "data";
/// File in that folder that turns portable mode on.
const MARKER: &str = ".kite-portable";

/// The portable data folder, when the executable has one with the marker beside it.
pub fn dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join(DATA);
    dir.join(MARKER).is_file().then_some(dir)
}

/// The folder takes a write: an archive unpacked into Program Files or onto a read-only drive
/// would otherwise stop the app in its setup with nothing on the screen.
pub fn writable(dir: &Path) -> std::io::Result<()> {
    let probe = dir.join(".write-test");
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)
}

/// Says why the portable copy cannot start, then ends the process.
pub fn fail(dir: &Path, e: &std::io::Error) -> ! {
    let text = format!(
        "Kite не может писать в папку данных:\n{d}\n{e}\n\nПортативной копии нужна папка с правами на запись — не Program Files и не носитель только для чтения.\n\n\
         Kite cannot write to its data folder:\n{d}\n{e}\n\nA portable copy needs a writable folder, not Program Files or a read-only drive.",
        d = dir.display()
    );
    show(&text);
    std::process::exit(1)
}

#[cfg(windows)]
fn show(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (text, title) = (wide(text), wide("Kite"));
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call; no owner window.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

#[cfg(not(windows))]
fn show(text: &str) {
    eprintln!("{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_writable_folder_passes_and_keeps_nothing() {
        let dir = std::env::temp_dir().join(format!("kite-portable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        writable(&dir).unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir(&dir).unwrap();
        assert!(writable(&dir).is_err());
    }
}
