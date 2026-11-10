//! "This PC": the local file system. Blocking calls; the IPC layer runs them on the blocking pool.
//! Windows paths are `C:\folder` with any drive the system has; Linux paths start at `/`.

use std::{fs, time::UNIX_EPOCH};

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
        let link = own.file_type().is_symlink();
        // A link (or a junction) takes the type of its target; a broken one is a file.
        let target = if link { fs::metadata(item.path()).ok() } else { None };
        let meta = target.as_ref().unwrap_or(&own);
        let dir = meta.is_dir() && (!link || target.is_some());
        out.push(Entry {
            hidden: name.starts_with('.') || hidden_attr(&own),
            dir,
            link,
            size: if dir { 0 } else { meta.len() },
            mtime: mtime_ms(meta),
            perm: String::new(),
            owner: String::new(),
            group: String::new(),
            name,
        });
    }
    Ok(out)
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
    fs::create_dir(path).map_err(|e| io_error(&e, path))
}

/// Never replaces: `rename` on Windows would overwrite a file silently. Changing only the case
/// of a name on Windows is the same file and is allowed.
pub fn rename(from: &str, to: &str) -> Result<()> {
    let same = cfg!(windows) && from.to_lowercase() == to.to_lowercase();
    if !same && fs::symlink_metadata(to).is_ok() {
        return Err(Error::Eexist { path: to.into() });
    }
    fs::rename(from, to).map_err(|e| io_error(&e, from))
}

/// To the system's recycle bin (Windows) or trash folder (Linux).
pub fn trash(paths: &[String]) -> Result<()> {
    for p in paths {
        fs::symlink_metadata(p).map_err(|e| io_error(&e, p))?;
    }
    trash::delete_all(paths).map_err(|e| Error::Einval { reason: e.to_string() })
}

pub fn read_text(path: &str) -> Result<TextFile> {
    let meta = fs::metadata(path).map_err(|e| io_error(&e, path))?;
    if meta.is_dir() {
        return Err(Error::Einval { reason: format!("{path}: is a folder") });
    }
    if meta.len() > MAX_TEXT {
        return Ok(TextFile::too_big());
    }
    let data = fs::read(path).map_err(|e| io_error(&e, path))?;
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
