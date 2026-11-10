//! Server folders through the session's SFTP channel; chown through an exec channel.

use std::future::Future;

use futures::stream::{self, StreamExt};
use russh::ChannelMsg;
use russh_sftp::{
    client::{RawSftpSession, error::Error as SftpError},
    protocol::{File, FileAttributes, OpenFlags, StatusCode},
};

use super::{Entry, MAX_TEXT, TextFile, join, normalize, perm_text, sh_quote, valid_account};
use crate::{
    error::{Error, Result},
    ssh::{Session, sftp::fs_error},
};

/// Requests in flight for one operation over many files: deep enough to hide the round trip.
const IN_FLIGHT: usize = 16;
/// Folders read at once while walking a tree.
const DIRS_IN_FLIGHT: usize = 8;
const READ_CHUNK: u64 = 64 * 1024;

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

/// All names of a folder, without `.` and `..`. Attributes are the entries' own (lstat).
async fn read_dir(sftp: &RawSftpSession, path: &str) -> Result<Vec<File>> {
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
    Ok(files)
}

pub async fn list(s: &Session, path: &str) -> Result<Vec<Entry>> {
    let sftp = &s.sftp;
    let files = read_dir(sftp, path).await?;
    // Links: one stat each for the target type, pipelined on the channel.
    // Futures are collected first: a lazy `map` inside the stream trips the Send check of commands.
    let stats: Vec<_> = files
        .iter()
        .map(|f| {
            let link = f.attrs.is_symlink().then(|| join(path, &f.filename));
            async move {
                match link {
                    Some(p) => sftp.stat(p).await.ok().map(|a| a.attrs.is_dir()),
                    None => None,
                }
            }
        })
        .collect();
    let targets: Vec<Option<bool>> = stream::iter(stats)
    .buffered(IN_FLIGHT)
    .collect()
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
    if raw.trim().is_empty() {
        return Err(Error::BadPath);
    }
    let path = normalize(raw, &s.home, base);
    let attrs = s.sftp.stat(path.as_str()).await.map_err(|e| fs_error(e, &path))?.attrs;
    if !attrs.is_dir() {
        return Err(Error::Enotdir { path });
    }
    Ok(path)
}

/// SFTP v3 has no "exists" status: a generic failure on a name that is there means EEXIST.
async fn exists_or(sftp: &RawSftpSession, e: SftpError, at: &str, path: &str) -> Error {
    let generic = matches!(&e, SftpError::Status(st) if st.status_code == StatusCode::Failure);
    if generic && sftp.lstat(at).await.is_ok() { Error::Eexist { path: at.into() } } else { fs_error(e, path) }
}

pub async fn mkdir(s: &Session, path: &str) -> Result<()> {
    match s.sftp.mkdir(path, FileAttributes::empty()).await {
        Ok(_) => Ok(()),
        Err(e) => Err(exists_or(&s.sftp, e, path, path).await),
    }
}

/// Never replaces: plain SFTP rename fails on an existing target on OpenSSH, other servers may
/// overwrite, so the target is checked first.
pub async fn rename(s: &Session, from: &str, to: &str) -> Result<()> {
    let sftp = &s.sftp;
    if sftp.lstat(to).await.is_ok() {
        return Err(Error::Eexist { path: to.into() });
    }
    match sftp.rename(from, to).await {
        Ok(_) => Ok(()),
        Err(e) => Err(exists_or(sftp, e, to, from).await),
    }
}

/// Runs the futures `n` at a time, results in order; stops at the first error. The futures are
/// built before the stream: a lazy `map` inside it trips the Send check of Tauri commands.
async fn ordered<T, F: Future<Output = Result<T>>>(futs: Vec<F>, n: usize) -> Result<Vec<T>> {
    let mut s = stream::iter(futs).buffered(n);
    let mut out = Vec::new();
    while let Some(r) = s.next().await {
        out.push(r?);
    }
    Ok(out)
}

/// Like `ordered`, for results nobody needs, in any order.
async fn each<T, F: Future<Output = Result<T>>>(futs: Vec<F>) -> Result<()> {
    let mut s = stream::iter(futs).buffer_unordered(IN_FLIGHT);
    while let Some(r) = s.next().await {
        r?;
    }
    Ok(())
}

struct Node {
    path: String,
    attrs: FileAttributes,
    depth: usize,
}

/// Everything under `root`, parents before children. Links are entries, never followed.
async fn walk(sftp: &RawSftpSession, root: &str) -> Result<Vec<Node>> {
    let mut out = Vec::new();
    let mut level = vec![root.to_owned()];
    let mut depth = 1;
    while !level.is_empty() {
        let reads: Vec<_> = level.iter().map(|d| async move { read_dir(sftp, d).await.map(|l| (d.clone(), l)) }).collect();
        let lists = ordered(reads, DIRS_IN_FLIGHT).await?;
        let mut next = Vec::new();
        for (dir, files) in lists {
            for f in files {
                let path = join(&dir, &f.filename);
                if f.attrs.is_dir() && !f.attrs.is_symlink() {
                    next.push(path.clone());
                }
                out.push(Node { path, attrs: f.attrs, depth });
            }
        }
        level = next;
        depth += 1;
    }
    Ok(out)
}

/// Deletes a file, a link or a whole folder; returns how many names went away.
pub async fn delete(s: &Session, path: &str) -> Result<usize> {
    let sftp = &s.sftp;
    let attrs = sftp.lstat(path).await.map_err(|e| fs_error(e, path))?.attrs;
    if !attrs.is_dir() || attrs.is_symlink() {
        sftp.remove(path).await.map_err(|e| fs_error(e, path))?;
        return Ok(1);
    }
    let tree = walk(sftp, path).await?;
    let (dirs, files): (Vec<&Node>, Vec<&Node>) = tree.iter().partition(|n| n.attrs.is_dir() && !n.attrs.is_symlink());
    each(files.iter().map(|n| async move { sftp.remove(n.path.as_str()).await.map_err(|e| fs_error(e, &n.path)) }).collect()).await?;
    // Deepest folders first; folders of one depth go together.
    let deepest = dirs.iter().map(|n| n.depth).max().unwrap_or(0);
    for depth in (1..=deepest).rev() {
        let level: Vec<_> = dirs
            .iter()
            .filter(|n| n.depth == depth)
            .map(|n| async move { sftp.rmdir(n.path.as_str()).await.map_err(|e| fs_error(e, &n.path)) })
            .collect();
        each(level).await?;
    }
    sftp.rmdir(path).await.map_err(|e| fs_error(e, path))?;
    Ok(tree.len() + 1)
}

/// Runs one command; Ok when it printed our marker, so a server that swallows exec
/// (`ForceCommand internal-sftp`) does not pass for success. Returns stderr otherwise,
/// NoExec when the command left no trace at all.
async fn exec(s: &Session, cmd: &str) -> Result<std::result::Result<(), String>> {
    const MARK: &str = "kite-ok";
    let mut ch = s.handle.channel_open_session().await?;
    ch.exec(true, format!("{cmd} && echo {MARK}")).await?;
    ch.eof().await?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    while let Some(m) = ch.wait().await {
        match m {
            ChannelMsg::Data { data } if out.len() < 4096 => out.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, .. } if err.len() < 4096 => err.extend_from_slice(&data),
            ChannelMsg::Failure => return Err(Error::NoExec),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    if String::from_utf8_lossy(&out).lines().any(|l| l.trim() == MARK) {
        Ok(Ok(()))
    } else {
        let err = String::from_utf8_lossy(&err).trim().to_owned();
        if err.is_empty() { Err(Error::NoExec) } else { Ok(Err(err)) }
    }
}

async fn chown(s: &Session, paths: &[String], owner: &str, group: &str, recursive: bool) -> Result<()> {
    for name in [owner, group].into_iter().filter(|n| !n.is_empty()) {
        if !valid_account(name) {
            return Err(Error::Einval { reason: format!("chown: {name}") });
        }
    }
    let spec = if group.is_empty() { owner.to_owned() } else { format!("{owner}:{group}") };
    let mut cmd = String::from("chown ");
    if recursive {
        cmd.push_str("-R ");
    }
    cmd.push_str("-- ");
    cmd.push_str(&sh_quote(&spec));
    for p in paths {
        cmd.push(' ');
        cmd.push_str(&sh_quote(p));
    }
    match exec(s, &cmd).await? {
        Ok(()) => Ok(()),
        Err(text) if text.contains("Operation not permitted") || text.contains("Permission denied") => {
            Err(Error::Eacces { path: paths.first().cloned().unwrap_or_default() })
        }
        Err(text) => Err(Error::Einval { reason: text.lines().next().unwrap_or_default().to_owned() }),
    }
}

/// Permission bits 0o777 for every path, and with `recursive` for everything under the folders.
/// setuid, setgid and sticky stay as they were. Owner and group change through `chown` when given.
pub async fn chmod(s: &Session, paths: &[String], mode: u32, owner: &str, group: &str, recursive: bool) -> Result<usize> {
    let sftp = &s.sftp;
    // Modes before chown: a chown by a non-root user clears setuid and setgid.
    let mut targets: Vec<(String, u32)> = Vec::new();
    for p in paths {
        let own = sftp.lstat(p.as_str()).await.map_err(|e| fs_error(e, p))?.attrs;
        // A link on the list changes its target, as chmod does; its tree is not walked.
        let attrs = if own.is_symlink() { sftp.stat(p.as_str()).await.map_err(|e| fs_error(e, p))?.attrs } else { own.clone() };
        targets.push((p.clone(), attrs.permissions.unwrap_or(0)));
        if recursive && own.is_dir() && !own.is_symlink() {
            for n in walk(sftp, p).await? {
                // A link's own mode means nothing, and setstat would follow it out of the tree.
                if !n.attrs.is_symlink() {
                    targets.push((n.path, n.attrs.permissions.unwrap_or(0)));
                }
            }
        }
    }
    if !owner.is_empty() || !group.is_empty() {
        chown(s, paths, owner, group, recursive).await?;
    }
    let sets: Vec<_> = targets
        .iter()
        .map(|(p, old)| async move {
            let attrs = FileAttributes { permissions: Some((old & 0o7000) | (mode & 0o777)), ..FileAttributes::empty() };
            sftp.setstat(p.as_str(), attrs).await.map_err(|e| fs_error(e, p))
        })
        .collect();
    each(sets).await?;
    Ok(targets.len())
}

/// A file for the viewer: up to 10 MB, read with pipelined chunks.
pub async fn read_text(s: &Session, path: &str) -> Result<TextFile> {
    let sftp = &s.sftp;
    let attrs = sftp.stat(path).await.map_err(|e| fs_error(e, path))?.attrs;
    if attrs.is_dir() {
        return Err(Error::Einval { reason: format!("{path}: is a folder") });
    }
    let size = attrs.size.unwrap_or(0);
    if size > MAX_TEXT {
        return Ok(TextFile::too_big());
    }
    let handle = sftp.open(path, OpenFlags::READ, FileAttributes::empty()).await.map_err(|e| fs_error(e, path))?.handle;
    let res = read_all(sftp, &handle, path, size).await;
    let _ = sftp.close(handle).await;
    Ok(match res? {
        Some(data) => TextFile::from_bytes(&data),
        None => TextFile::too_big(),
    })
}

async fn read_at(sftp: &RawSftpSession, handle: &str, path: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
    match sftp.read(handle, offset, len as u32).await {
        Ok(d) => Ok(d.data),
        Err(SftpError::Status(st)) if st.status_code == StatusCode::Eof => Ok(Vec::new()),
        Err(e) => Err(fs_error(e, path)),
    }
}

/// None when the file grew past the limit while reading.
async fn read_all(sftp: &RawSftpSession, handle: &str, path: &str, size: u64) -> Result<Option<Vec<u8>>> {
    let offsets: Vec<u64> = (0..size).step_by(READ_CHUNK as usize).collect();
    let reads: Vec<_> = offsets.iter().map(|&o| read_at(sftp, handle, path, o, READ_CHUNK.min(size - o))).collect();
    let chunks = ordered(reads, IN_FLIGHT).await?;
    let mut data = Vec::with_capacity(size as usize);
    for (k, c) in chunks.into_iter().enumerate() {
        // A short reply mid-file: the server cut the chunk, carry on from where it stopped.
        if (data.len() as u64) < offsets[k] {
            break;
        }
        data.extend(c);
    }
    loop {
        let more = read_at(sftp, handle, path, data.len() as u64, READ_CHUNK).await?;
        if more.is_empty() {
            return Ok(Some(data));
        }
        data.extend(more);
        if data.len() as u64 > MAX_TEXT {
            return Ok(None);
        }
    }
}
