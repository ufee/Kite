//! "This PC": the local file system. Blocking calls; the IPC layer runs them on the blocking pool.
//! Windows paths are `C:\folder` with any drive the system has; Linux paths start at `/`.

use std::{fs, path::Path, time::UNIX_EPOCH};

use super::{Entry, MAX_TEXT, TextFile, io_error};
use crate::error::{Error, Result};

pub fn home() -> String {
    crate::ssh::keys::home().to_string_lossy().into_owned()
}

fn mtime_ms(m: &fs::Metadata) -> u64 {
    m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(windows)]
fn hidden_attr(m: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(windows))]
fn hidden_attr(_: &fs::Metadata) -> bool {
    false
}

pub fn list(path: &str) -> Result<Vec<Entry>> {
    let dir = fs::read_dir(path).map_err(|e| io_error(&e, path))?;
    let mut out = Vec::new();
    for item in dir {
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        // On Windows this comes from the directory listing itself, no extra call per file.
        let Ok(own) = item.metadata() else {
            out.push(Entry { hidden: name.starts_with('.'), name, dir: false, link: false, size: 0, mtime: 0, perm: String::new(), owner: String::new(), group: String::new() });
            continue;
        };
        out.push(entry(name, &item.path(), &own));
    }
    Ok(out)
}

fn entry(name: String, path: &Path, own: &fs::Metadata) -> Entry {
    let link = own.file_type().is_symlink();
    // A link (or a junction) takes the type of its target; a broken one is a file.
    let target = if link { fs::metadata(path).ok() } else { None };
    let meta = target.as_ref().unwrap_or(own);
    let dir = meta.is_dir() && (!link || target.is_some());
    Entry {
        hidden: name.starts_with('.') || hidden_attr(own),
        dir,
        link,
        size: if dir { 0 } else { meta.len() },
        mtime: mtime_ms(meta),
        perm: String::new(),
        owner: String::new(),
        group: String::new(),
        name,
    }
}

/// One path as its parent folder lists it: the folder and the row. None for a missing path or a drive root.
#[cfg_attr(not(windows), allow(dead_code, reason = "only the Explorer drop uses it"))]
pub fn stat(path: &Path) -> Option<(String, Entry)> {
    let own = fs::symlink_metadata(path).ok()?;
    let parent = path.parent()?.to_str()?.to_owned();
    let name = path.file_name()?.to_str()?.to_owned();
    Some((parent, entry(name, path, &own)))
}

/// A typed path as this platform writes it, `.` and `..` folded. Not checked against the disk.
#[cfg(windows)]
pub fn normalize(raw: &str, home: &str, base: Option<&str>) -> Result<String> {
    let mut p = raw.trim().replace('/', "\\");
    if p.is_empty() {
        return Err(Error::BadPath);
    }
    if p == "~" || p.starts_with("~\\") {
        p = format!("{home}{}", &p[1..]);
    }
    let drive = |s: &str| {
        let b = s.as_bytes();
        (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\')).then(|| b[0].to_ascii_uppercase() as char)
    };
    // Network shares and drive-relative `C:name` are not supported: refuse instead of guessing.
    let b = p.as_bytes();
    if p.starts_with("\\\\") || (b.len() > 2 && b[1] == b':' && drive(&p).is_none()) {
        return Err(Error::BadPath);
    }
    if drive(&p).is_none() {
        match base {
            // `\folder` is the root of the base's drive
            Some(b) if p.starts_with('\\') && drive(b).is_some() => p = format!("{}{p}", &b[..2]),
            Some(b) => p = format!("{}\\{p}", b.trim_end_matches('\\')),
            None => return Err(Error::BadPath),
        }
    }
    let Some(letter) = drive(&p) else { return Err(Error::BadPath) };
    let mut parts: Vec<&str> = Vec::new();
    for part in p[2..].split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    Ok(format!("{letter}:\\{}", parts.join("\\")))
}

#[cfg(not(windows))]
pub fn normalize(raw: &str, home: &str, base: Option<&str>) -> Result<String> {
    if raw.trim().is_empty() {
        return Err(Error::BadPath);
    }
    Ok(super::normalize(raw, home, base))
}

/// A typed path that must be an existing folder.
pub fn resolve(raw: &str, base: Option<&str>) -> Result<String> {
    let path = normalize(raw, &home(), base)?;
    let meta = fs::metadata(&path).map_err(|e| io_error(&e, &path))?;
    if !meta.is_dir() {
        return Err(Error::Enotdir { path });
    }
    Ok(path)
}

pub fn mkdir(path: &str) -> Result<()> {
    check_name(path)?;
    fs::create_dir(path).map_err(|e| io_error(&e, path))
}

/// Names Win32 would quietly change or turn into a device: a trailing dot or space, `CON`, `NUL`,
/// `COM1`… A file made under such a name through `\\?\` cannot be reached by normal paths later.
#[cfg(windows)]
fn check_name(path: &str) -> Result<()> {
    let name = path.rsplit('\\').next().unwrap_or_default();
    let stem = name.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    let numbered = |p: &str| stem.len() == 4 && stem.starts_with(p) && matches!(stem.as_bytes()[3], b'1'..=b'9');
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered("COM") || numbered("LPT");
    if name.ends_with('.') || name.ends_with(' ') || device {
        return Err(Error::Einval { reason: format!("{name}: not a valid name on Windows") });
    }
    Ok(())
}

#[cfg(not(windows))]
fn check_name(_: &str) -> Result<()> {
    Ok(())
}

/// Never replaces: `rename` on Windows would overwrite a file silently. Changing only the case
/// of a name on Windows is the same file and is allowed.
pub fn rename(from: &str, to: &str) -> Result<()> {
    check_name(to)?;
    let same = cfg!(windows) && from.to_lowercase() == to.to_lowercase();
    if !same && fs::symlink_metadata(to).is_ok() {
        return Err(Error::Eexist { path: to.into() });
    }
    move_no_replace(from, to).map_err(|e| if e.kind() == std::io::ErrorKind::AlreadyExists { Error::Eexist { path: to.into() } } else { io_error(&e, from) })
}

/// The check above can race with a file that appears meanwhile; on Windows the move itself refuses
/// to replace. Paths are absolute and normalised, so the `\\?\` form lifts the 260-character limit.
#[cfg(windows)]
fn move_no_replace(from: &str, to: &str) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |s: &str| std::ffi::OsStr::new(&format!(r"\\?\{s}")).encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (f, t) = (wide(from), wide(to));
    // SAFETY: both are NUL-terminated wide strings that outlive the call.
    let ok = unsafe { windows_sys::Win32::Storage::FileSystem::MoveFileExW(f.as_ptr(), t.as_ptr(), 0) };
    if ok != 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

/// POSIX `rename` replaces; `renameat2(RENAME_NOREPLACE)` is Linux-only and not on every
/// filesystem, so the check above stays the guard here.
#[cfg(not(windows))]
fn move_no_replace(from: &str, to: &str) -> std::io::Result<()> {
    fs::rename(from, to)
}

/// To the system's recycle bin (Windows) or trash folder (Linux).
pub fn trash(paths: &[String]) -> Result<()> {
    for p in paths {
        fs::symlink_metadata(p).map_err(|e| io_error(&e, p))?;
    }
    trash::delete_all(paths).map_err(|e| Error::Einval { reason: e.to_string() })
}

pub fn read_text(path: &str) -> Result<TextFile> {
    use std::io::Read;
    let meta = fs::metadata(path).map_err(|e| io_error(&e, path))?;
    if meta.is_dir() {
        return Err(Error::Einval { reason: format!("{path}: is a folder") });
    }
    // Devices, FIFOs and /proc report size 0 and may never end.
    if !meta.is_file() {
        return Err(Error::Einval { reason: format!("{path}: not a regular file") });
    }
    if meta.len() > MAX_TEXT {
        return Ok(TextFile::too_big());
    }
    let mut data = Vec::new();
    fs::File::open(path).and_then(|f| f.take(MAX_TEXT + 1).read_to_end(&mut data)).map_err(|e| io_error(&e, path))?;
    if data.len() as u64 > MAX_TEXT {
        return Ok(TextFile::too_big());
    }
    Ok(TextFile::from_bytes(&data))
}

/// Drive roots that exist now: `C:\`, `D:\`, a mapped network drive; `/` on Linux.
#[cfg(windows)]
pub fn drives() -> Vec<String> {
    // SAFETY: no arguments, returns a bit mask.
    let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    (0..26u8).filter(|i| mask & (1 << i) != 0).map(|i| format!("{}:\\", (b'A' + i) as char)).collect()
}

#[cfg(not(windows))]
pub fn drives() -> Vec<String> {
    vec!["/".into()]
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn windows_paths() {
        let home = r"C:\Users\me";
        assert_eq!(normalize("c:", home, None).unwrap(), r"C:\");
        assert_eq!(normalize("d:/www/../logs/", home, None).unwrap(), r"D:\logs");
        assert_eq!(normalize("~", home, None).unwrap(), home);
        assert_eq!(normalize(r"~\Desktop", home, None).unwrap(), r"C:\Users\me\Desktop");
        assert_eq!(normalize("site", home, Some(r"E:\www")).unwrap(), r"E:\www\site");
        assert_eq!(normalize(r"\tmp", home, Some(r"E:\www")).unwrap(), r"E:\tmp");
        assert_eq!(normalize(r"C:\..\..", home, None).unwrap(), r"C:\");
        assert!(matches!(normalize("folder", home, None), Err(Error::BadPath)));
        assert!(matches!(normalize("  ", home, None), Err(Error::BadPath)));
        assert!(matches!(normalize("cc:", home, None), Err(Error::BadPath)));
        assert!(matches!(normalize(r"\\nas\share", home, Some(r"E:\www")), Err(Error::BadPath)));
        assert!(matches!(normalize("c:logs", home, Some(r"E:\www")), Err(Error::BadPath)));
        for bad in [r"C:\x\v1.", r"C:\x\a ", r"C:\x\nul", r"C:\x\Com1.txt", r"C:\x\LPT9"] {
            assert!(matches!(check_name(bad), Err(Error::Einval { .. })), "{bad}");
        }
        for good in [r"C:\x\v1.txt", r"C:\x\console", r"C:\x\com10", r"C:\x\.env"] {
            assert!(check_name(good).is_ok(), "{good}");
        }
    }

    #[test]
    fn lists_and_changes_a_folder() {
        let root = std::env::temp_dir().join(format!("kite-fs-{}", std::process::id()));
        let root_s = root.to_string_lossy().into_owned();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.txt"), b"hello").unwrap();
        mkdir(&format!(r"{root_s}\sub")).unwrap();
        assert!(matches!(mkdir(&format!(r"{root_s}\sub")), Err(Error::Eexist { .. })));
        assert!(matches!(rename(&format!(r"{root_s}\a.txt"), &format!(r"{root_s}\sub")), Err(Error::Eexist { .. })));
        rename(&format!(r"{root_s}\a.txt"), &format!(r"{root_s}\A.txt")).unwrap();
        let mut l = list(&root_s).unwrap();
        l.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(l.iter().map(|e| (e.name.as_str(), e.dir, e.size)).collect::<Vec<_>>(), [("A.txt", false, 5), ("sub", true, 0)]);
        assert_eq!(read_text(&format!(r"{root_s}\A.txt")).unwrap().text, "hello");
        assert!(matches!(resolve(&format!(r"{root_s}\A.txt"), None), Err(Error::Enotdir { .. })));
        assert!(matches!(resolve(&format!(r"{root_s}\none"), None), Err(Error::Enoent { .. })));
        fs::remove_dir_all(&root).unwrap();
        assert!(drives().iter().any(|d| d == r"C:\"));
    }
}
