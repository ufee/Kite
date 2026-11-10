//! Folders on a server through the session's SFTP channel. Read-only for now: listing, resolving a
//! typed path, the home folder. Changing operations and the local file system arrive in stage 4.

use futures::future::join_all;
use russh_sftp::{
    client::error::Error as SftpError,
    protocol::{File, StatusCode},
};
use serde::Serialize;

use crate::{
    error::{Error, Result},
    ssh::{Session, sftp::fs_error},
};

#[derive(Debug, Serialize)]
pub struct Entry {
    pub name: String,
    /// Follows a link's target; a broken link is a file.
    pub dir: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub link: bool,
    pub size: u64,
    /// Milliseconds, like the UI's Date.
    pub mtime: u64,
    /// `rwxr-xr-x`, without the type letter.
    pub perm: String,
    pub owner: String,
    pub group: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// POSIX path from what the user typed: `~` is the home folder, a relative path hangs off `base`
/// (or the home folder), `.` and `..` are folded. Links are not resolved: the path stays as typed.
pub fn normalize(raw: &str, home: &str, base: Option<&str>) -> String {
    let raw = raw.trim();
    let full = if raw == "~" {
        home.to_owned()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else if raw.starts_with('/') {
        raw.to_owned()
    } else {
        format!("{}/{raw}", base.unwrap_or(home))
    };
    let mut parts: Vec<&str> = Vec::new();
    for p in full.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

fn perm_text(mode: u32) -> String {
    const BITS: [(u32, char); 9] =
        [(0o400, 'r'), (0o200, 'w'), (0o100, 'x'), (0o040, 'r'), (0o020, 'w'), (0o010, 'x'), (0o004, 'r'), (0o002, 'w'), (0o001, 'x')];
    let mut s: Vec<char> = BITS.iter().map(|&(b, c)| if mode & b != 0 { c } else { '-' }).collect();
    // setuid, setgid, sticky as ls shows them
    for (bit, at, on, off) in [(0o4000, 2, 's', 'S'), (0o2000, 5, 's', 'S'), (0o1000, 8, 't', 'T')] {
        if mode & bit != 0 {
            s[at] = if s[at] == 'x' { on } else { off };
        }
    }
    s.into_iter().collect()
}

/// `longname` is `ls -l` output: names of the owner and group are fields 3 and 4.
fn owner_group(f: &File) -> (String, String) {
    let mut it = f.longname.split_whitespace().skip(2);
    match (it.next(), it.next()) {
        (Some(o), Some(g)) if !f.longname.is_empty() => (o.to_owned(), g.to_owned()),
        _ => (
            f.attrs.user.clone().or(f.attrs.uid.map(|u| u.to_string())).unwrap_or_default(),
            f.attrs.group.clone().or(f.attrs.gid.map(|g| g.to_string())).unwrap_or_default(),
        ),
    }
}

pub async fn list(s: &Session, path: &str) -> Result<Vec<Entry>> {
    let sftp = &s.sftp;
    let handle = sftp.opendir(path).await.map_err(|e| fs_error(e, path))?.handle;
    let mut files = Vec::new();
    loop {
        match sftp.readdir(handle.as_str()).await {
            Ok(name) => files.extend(name.files.into_iter().filter(|f| f.filename != "." && f.filename != "..")),
            Err(SftpError::Status(st)) if st.status_code == StatusCode::Eof => break,
            Err(e) => {
                let _ = sftp.close(handle).await;
                return Err(fs_error(e, path));
            }
        }
    }
    let _ = sftp.close(handle).await;
    // Links: one stat each for the target type, pipelined on the channel.
    let dir = path.trim_end_matches('/');
    let targets = join_all(files.iter().map(|f| async move {
        if !f.attrs.is_symlink() {
            return None;
        }
        sftp.stat(format!("{dir}/{}", f.filename)).await.ok().map(|a| a.attrs.is_dir())
    }))
    .await;
    Ok(files
        .iter()
        .zip(targets)
        .map(|(f, target)| {
            let link = f.attrs.is_symlink();
            let (owner, group) = owner_group(f);
            Entry {
                hidden: f.filename.starts_with('.'),
                dir: if link { target.unwrap_or(false) } else { f.attrs.is_dir() },
                link,
                size: f.attrs.size.unwrap_or(0),
                mtime: u64::from(f.attrs.mtime.unwrap_or(0)) * 1000,
                perm: perm_text(f.attrs.permissions.unwrap_or(0)),
                owner,
                group,
                name: f.filename.clone(),
            }
        })
        .collect())
}

/// A typed path that must be an existing folder.
pub async fn resolve(s: &Session, raw: &str, base: Option<&str>) -> Result<String> {
    let path = normalize(raw, &s.home, base);
    let attrs = s.sftp.stat(path.as_str()).await.map_err(|e| fs_error(e, &path))?.attrs;
    if !attrs.is_dir() {
        return Err(Error::Enotdir { path });
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize("~", "/home/u", None), "/home/u");
        assert_eq!(normalize("~/www/../logs/", "/home/u", None), "/home/u/logs");
        assert_eq!(normalize("etc", "/root", Some("/")), "/etc");
        assert_eq!(normalize("conf", "/root", None), "/root/conf");
        assert_eq!(normalize("/var//www/./site", "/root", None), "/var/www/site");
        assert_eq!(normalize("/../..", "/root", None), "/");
    }

    #[test]
    fn permission_text() {
        assert_eq!(perm_text(0o755), "rwxr-xr-x");
        assert_eq!(perm_text(0o644), "rw-r--r--");
        assert_eq!(perm_text(0o4755), "rwsr-xr-x");
        assert_eq!(perm_text(0o1777), "rwxrwxrwt");
        assert_eq!(perm_text(0o2640), "rw-r-S---");
    }
}
