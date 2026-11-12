//! The external programs a file opens in: the system text editor, Notepad++, VS Code.
//!
//! "System" is the program plain text opens with (`.txt` on Windows, `text/plain` on Linux), never the file's own association:
//! opening `deploy.bat` or `run.py` with its association would run it on this PC.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::error::{Error, Result};

/// Settings values, in the order of the settings list.
pub const ALL: [&str; 3] = ["system", "npp", "code"];

/// The editors found on this PC; `system` is always there.
pub fn available() -> Vec<&'static str> {
    ALL.into_iter().filter(|e| *e == "system" || find(e).is_some()).collect()
}

/// Starts the editor on the file and lets it run on its own; the program's name for the log.
/// An editor that is not installed (any more) falls back to the system one.
pub fn open(editor: &str, file: &Path) -> Result<String> {
    if editor != "system"
        && let Some(exe) = find(editor)
        && let Ok(name) = spawn(&exe, file)
    {
        return Ok(name);
    }
    system(file)
}

fn spawn(exe: &Path, file: &Path) -> std::io::Result<String> {
    spawn_with(exe, &[], file)
}

fn spawn_with(exe: &Path, args: &[&str], file: &Path) -> std::io::Result<String> {
    let mut child = Command::new(exe).args(args).arg(file).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    // Waited for on the side: an editor that exits is reaped, not left a zombie.
    std::thread::spawn(move || child.wait());
    Ok(exe.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
}

fn exists(p: PathBuf) -> Option<PathBuf> {
    p.is_file().then_some(p)
}

/// A program on PATH.
fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|d| exists(d.join(name)))
}

#[cfg(windows)]
fn find(editor: &str) -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    match editor {
        "npp" => ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
            .iter()
            .filter_map(|k| env(k))
            .find_map(|d| exists(d.join("Notepad++").join("notepad++.exe")))
            .or_else(|| on_path("notepad++.exe")),
        "code" => env("LOCALAPPDATA")
            .map(|d| d.join("Programs").join("Microsoft VS Code"))
            .into_iter()
            .chain(env("ProgramFiles").map(|d| d.join("Microsoft VS Code")))
            // `code.cmd` on PATH sits in `<install>\bin`.
            .chain(on_path("code.cmd").and_then(|c| Some(c.parent()?.parent()?.to_path_buf())))
            .find_map(|d| exists(d.join("Code.exe"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn find(editor: &str) -> Option<PathBuf> {
    match editor {
        "code" => on_path("code"),
        _ => None,
    }
}

#[cfg(windows)]
fn system(file: &Path) -> Result<String> {
    let fail = |e: std::io::Error| Error::Einval { reason: e.to_string() };
    if let Some(exe) = txt_editor()
        && let Ok(name) = spawn(&exe, file)
    {
        return Ok(name);
    }
    // The packaged Notepad of Windows 11 cannot be started by its path; this stub starts it.
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    spawn(&root.join("System32").join("notepad.exe"), file).map_err(fail)
}

/// The program `.txt` files open with.
#[cfg(windows)]
fn txt_editor() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::UI::Shell::{ASSOCF_NONE, ASSOCSTR_EXECUTABLE, AssocQueryStringW};
    let ext: Vec<u16> = ".txt\0".encode_utf16().collect();
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: NUL-terminated inputs, an output buffer of `len` u16 that the call fills.
    let hr = unsafe { AssocQueryStringW(ASSOCF_NONE, ASSOCSTR_EXECUTABLE, ext.as_ptr(), verb.as_ptr(), buf.as_mut_ptr(), &mut len) };
    if hr != 0 || len == 0 {
        return None;
    }
    buf.truncate(len as usize - 1);
    let p = PathBuf::from(std::ffi::OsString::from_wide(&buf));
    // Packaged apps live in WindowsApps and cannot be run by path.
    if p.components().any(|c| c.as_os_str().eq_ignore_ascii_case("WindowsApps")) {
        return None;
    }
    exists(p)
}

/// The program plain text opens with, as `.txt` on Windows: `xdg-open` on the copy would pick the
/// file's own type and could run a script.
#[cfg(not(windows))]
fn system(file: &Path) -> Result<String> {
    let fail = || Error::Einval { reason: "no text editor found".into() };
    let desktop = Command::new("xdg-mime")
        .args(["query", "default", "text/plain"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_end_matches(".desktop").to_owned())
        .filter(|d| !d.is_empty());
    if let (Some(d), Some(launch)) = (desktop, on_path("gtk-launch"))
        && spawn_with(&launch, &[d.as_str()], file).is_ok()
    {
        return Ok(d);
    }
    ["gnome-text-editor", "gedit", "kate", "kwrite", "mousepad", "xed", "pluma", "featherpad"]
        .iter()
        .find_map(|n| on_path(n).and_then(|exe| spawn(&exe, file).ok()))
        .ok_or_else(fail)
}
