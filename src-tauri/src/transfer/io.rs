//! Both ends of a transfer behind one face: this PC (blocking std::fs on the blocking pool) or a
//! server (the transfer connection's SFTP). Bytes flow from a reader task to a writer task through
//! a bounded channel, in file order, so every route (COPY, PUT, GET, RELAY) is the same code.

use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::stream::{FuturesOrdered, FuturesUnordered, StreamExt};
use russh_sftp::{
    client::error::Error as SftpError,
    protocol::{FileAttributes, OpenFlags, StatusCode},
};
use tokio::sync::{OwnedSemaphorePermit, mpsc, watch};

use super::conn::{Conn, Lease};
use crate::{
    error::{Error, Result},
    fs::io_error,
    ssh::sftp::fs_error,
};

/// Requests in flight per file; matches the OpenSSH `sftp` default. More gives nothing (spike).
pub const IN_FLIGHT: usize = 64;
/// Chunks queued between the reader and the writer.
const QUEUE: usize = 32;
/// Local chunk when neither end is a server.
const LOCAL_CHUNK: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Link,
    /// FIFO, socket, device: never transferred.
    Other,
}

#[derive(Clone, Debug)]
pub struct Meta {
    pub kind: Kind,
    pub size: u64,
    /// Seconds since the epoch: SFTP v3 has no finer dates.
    pub mtime: u64,
    pub mode: u32,
}

impl Meta {
    fn of_sftp(a: &FileAttributes) -> Meta {
        let kind = if a.is_symlink() {
            Kind::Link
        } else if a.is_dir() {
            Kind::Dir
        } else if a.permissions.is_none() || a.file_type().is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        Meta { kind, size: a.size.unwrap_or(0), mtime: a.mtime.unwrap_or(0) as u64, mode: a.permissions.unwrap_or(0) & 0o7777 }
    }

    fn of_local(m: &std::fs::Metadata) -> Meta {
        let ft = m.file_type();
        let kind = if ft.is_symlink() {
            Kind::Link
        } else if ft.is_dir() {
            Kind::Dir
        } else if ft.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        Meta { kind, size: if kind == Kind::File { m.len() } else { 0 }, mtime, mode: 0 }
    }
}

/// An entry under a folder being transferred; `rel` uses `/` on every platform.
pub struct Node {
    pub rel: String,
    pub meta: Meta,
}

pub enum Side {
    Local,
    Remote(Lease),
}

/// Stops a transfer between chunks: true means pause or cancel.
pub type Stop = watch::Receiver<bool>;

pub async fn stopped(stop: &mut Stop) {
    let _ = stop.wait_for(|s| *s).await;
}

fn is_eof(e: &SftpError) -> bool {
    matches!(e, SftpError::Status(s) if s.status_code == StatusCode::Eof)
}

fn no_file(e: &SftpError) -> bool {
    matches!(e, SftpError::Status(s) if s.status_code == StatusCode::NoSuchFile)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| Error::Einval { reason: e.to_string() })?
}

/// A name this PC can hold as one path component. A server name is any bytes but `/`: on Windows
/// `\` and `:` would make it a path of its own (`..\x`, `C:\x`, an NTFS stream), so such a file is
/// skipped rather than joined.
pub fn local_name_ok(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return false;
    }
    if !cfg!(windows) {
        return true;
    }
    let stem = name.split('.').next().unwrap_or_default().trim_end_matches(' ').to_ascii_uppercase();
    !(name.ends_with(['.', ' ']) || name.chars().any(|c| c < ' ' || r#"\:<>"|?*"#.contains(c)) || is_device(&stem))
}

/// Names Windows keeps for devices, with any extension: CON, PRN, AUX, NUL, COM0–9 and LPT0–9,
/// the superscript digits ¹²³ included.
fn is_device(stem: &str) -> bool {
    if ["CON", "PRN", "AUX", "NUL"].contains(&stem) {
        return true;
    }
    let Some(rest) = stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT")) else { return false };
    let mut c = rest.chars();
    matches!((c.next(), c.next()), (Some(d), None) if d.is_ascii_digit() || "¹²³".contains(d))
}

/// Every component of a `/`-separated relative path is a name this PC can hold.
pub fn local_rel_ok(rel: &str) -> bool {
    rel.split('/').all(local_name_ok)
}

/// A local path from a folder and a `/`-separated relative path.
fn local_join(dir: &str, rel: &str) -> PathBuf {
    let mut p = PathBuf::from(dir);
    p.extend(rel.split('/').filter(|s| !s.is_empty()));
    p
}

fn remote_join(dir: &str, rel: &str) -> String {
    if rel.is_empty() {
        dir.to_owned()
    } else {
        crate::fs::join(dir, rel)
    }
}

/// `dir/.name.kite-part` next to the target.
pub fn part_name(name: &str) -> String {
    format!(".{name}.kite-part")
}

fn file_time(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

/// SSH string: length and bytes.
fn ssh_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

impl Side {
    pub fn conn(&self) -> Option<&Arc<Conn>> {
        match self {
            Side::Local => None,
            Side::Remote(l) => Some(&l.conn),
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Side::Local)
    }

    /// A path under `dir`; `rel` empty is `dir` itself.
    pub fn join(&self, dir: &str, rel: &str) -> String {
        match self {
            Side::Local => local_join(dir, rel).to_string_lossy().into_owned(),
            Side::Remote(_) => remote_join(dir, rel),
        }
    }

    /// The folder and the name of a path of this side.
    pub fn split(&self, path: &str) -> (String, String) {
        match self {
            Side::Local => {
                let p = Path::new(path);
                let dir = p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
                (dir, p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            }
            Side::Remote(_) => match path.rfind('/') {
                Some(0) => ("/".into(), path[1..].to_owned()),
                Some(i) => (path[..i].to_owned(), path[i + 1..].to_owned()),
                None => (String::new(), path.to_owned()),
            },
        }
    }

    pub fn part_of(&self, path: &str) -> String {
        let (dir, name) = self.split(path);
        self.join(&dir, &part_name(&name))
    }

    /// Own attributes of a path (a link is a link); None when it does not exist.
    pub async fn lstat(&self, path: &str) -> Result<Option<Meta>> {
        match self {
            Side::Local => {
                let p = path.to_owned();
                blocking(move || match std::fs::symlink_metadata(&p) {
                    Ok(m) => Ok(Some(Meta::of_local(&m))),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(io_error(&e, &p)),
                })
                .await
            }
            Side::Remote(l) => match l.conn.session.sftp.lstat(path).await {
                Ok(a) => Ok(Some(Meta::of_sftp(&a.attrs))),
                Err(e) if no_file(&e) => Ok(None),
                Err(e) => Err(fs_error(e, path)),
            },
        }
    }

    /// Attributes of what a path points to: the item the user picked may be a link.
    pub async fn stat(&self, path: &str) -> Result<Option<Meta>> {
        match self {
            Side::Local => {
                let p = path.to_owned();
                blocking(move || match std::fs::metadata(&p) {
                    Ok(m) => Ok(Some(Meta::of_local(&m))),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(io_error(&e, &p)),
                })
                .await
            }
            Side::Remote(l) => match l.conn.session.sftp.stat(path).await {
                Ok(a) => Ok(Some(Meta::of_sftp(&a.attrs))),
                Err(e) if no_file(&e) => Ok(None),
                Err(e) => Err(fs_error(e, path)),
            },
        }
    }

    /// Everything under `root`, parents before children; links are entries, never followed.
    pub async fn walk(&self, root: &str) -> Result<Vec<Node>> {
        match self {
            Side::Local => {
                let root = root.to_owned();
                blocking(move || local_walk(&root)).await
            }
            Side::Remote(l) => {
                let (nodes, _) = crate::fs::remote::walk(&l.conn.session.sftp, root, false, false).await?;
                let base = root.trim_end_matches('/').len() + 1;
                Ok(nodes.into_iter().map(|n| Node { rel: n.path[base.min(n.path.len())..].to_owned(), meta: Meta::of_sftp(&n.attrs) }).collect())
            }
        }
    }

    /// A folder; one that is already there is fine.
    /// A folder, if there is none yet; true when this call made it.
    pub async fn mkdir(&self, path: &str) -> Result<bool> {
        match self {
            Side::Local => {
                let p = path.to_owned();
                blocking(move || match std::fs::create_dir(&p) {
                    Ok(()) => Ok(true),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && Path::new(&p).is_dir() => Ok(false),
                    Err(e) => Err(io_error(&e, &p)),
                })
                .await
            }
            Side::Remote(l) => {
                let sftp = &l.conn.session.sftp;
                match sftp.mkdir(path, FileAttributes::empty()).await {
                    Ok(_) => Ok(true),
                    Err(e) => match sftp.stat(path).await {
                        Ok(a) if a.attrs.is_dir() => Ok(false),
                        Ok(_) => Err(Error::Eexist { path: path.into() }),
                        Err(_) => Err(fs_error(e, path)),
                    },
                }
            }
        }
    }

    pub async fn remove(&self, path: &str) -> Result<()> {
        match self {
            Side::Local => {
                let p = path.to_owned();
                blocking(move || std::fs::remove_file(&p).map_err(|e| io_error(&e, &p))).await
            }
            Side::Remote(l) => l.conn.session.sftp.remove(path).await.map(|_| ()).map_err(|e| fs_error(e, path)),
        }
    }

    /// A link itself, never what it points to; on Windows a link to a folder is a folder entry.
    pub async fn remove_link(&self, path: &str) -> Result<()> {
        match self {
            Side::Local => {
                let p = path.to_owned();
                blocking(move || std::fs::remove_file(&p).or_else(|_| std::fs::remove_dir(&p)).map_err(|e| io_error(&e, &p))).await
            }
            Side::Remote(_) => self.remove(path).await,
        }
    }

    /// An empty folder; one with something left in it stays.
    pub async fn rmdir(&self, path: &str) -> Result<()> {
        match self {
            Side::Local => {
                let p = path.to_owned();
                blocking(move || std::fs::remove_dir(&p).map_err(|e| io_error(&e, &p))).await
            }
            Side::Remote(l) => l.conn.session.sftp.rmdir(path).await.map(|_| ()).map_err(|e| fs_error(e, path)),
        }
    }

    /// Permission bits of a server path; set exactly, past the server's umask.
    pub async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        let Side::Remote(l) = self else { return Ok(()) };
        let mut a = FileAttributes::empty();
        a.permissions = Some(mode);
        l.conn.session.sftp.setstat(path, a).await.map(|_| ()).map_err(|e| fs_error(e, path))
    }

    /// Renames over an existing file: `posix-rename@openssh.com` when the server has it, otherwise
    /// remove and rename (not atomic, but the only way in plain SFTP v3).
    pub async fn rename_over(&self, from: &str, to: &str) -> Result<()> {
        match self {
            // On Windows std::fs::rename replaces an existing file (MoveFileExW with REPLACE_EXISTING).
            Side::Local => {
                let (f, t) = (from.to_owned(), to.to_owned());
                blocking(move || std::fs::rename(&f, &t).map_err(|e| io_error(&e, &t))).await
            }
            Side::Remote(l) => {
                let conn = &l.conn;
                let sftp = &conn.session.sftp;
                if conn.posix_rename.load(Ordering::Relaxed) {
                    let mut data = Vec::with_capacity(from.len() + to.len() + 8);
                    ssh_string(&mut data, from);
                    ssh_string(&mut data, to);
                    match sftp.extended("posix-rename@openssh.com", data).await {
                        Ok(russh_sftp::protocol::Packet::Status(s)) if s.status_code == StatusCode::Ok => return Ok(()),
                        Ok(russh_sftp::protocol::Packet::Status(s)) if s.status_code == StatusCode::OpUnsupported => {
                            conn.posix_rename.store(false, Ordering::Relaxed);
                        }
                        Ok(russh_sftp::protocol::Packet::Status(s)) => return Err(fs_error(SftpError::Status(s), to)),
                        Ok(_) => conn.posix_rename.store(false, Ordering::Relaxed),
                        Err(e) => return Err(fs_error(e, to)),
                    }
                }
                let old = self.old_of(to);
                let Err(first) = sftp.rename(from, to).await else {
                    // A break after the old file stepped aside last time left it there.
                    let _ = sftp.remove(&old).await;
                    return Ok(());
                };
                // Plain SFTP refuses to rename over a file. Only that case goes on: the part is there
                // and a file is in the way. The old file steps aside, the part takes its place, then
                // the old one goes; if the part cannot move, the old one comes back.
                let in_way = matches!(sftp.lstat(to).await, Ok(a) if !a.attrs.is_dir());
                if !in_way || sftp.lstat(from).await.is_err() {
                    return Err(fs_error(first, to));
                }
                let _ = sftp.remove(&old).await;
                sftp.rename(to, &old).await.map_err(|e| fs_error(e, to))?;
                match sftp.rename(from, to).await {
                    Ok(_) => {
                        let _ = sftp.remove(&old).await;
                        Ok(())
                    }
                    Err(e) => {
                        let _ = sftp.rename(&old, to).await;
                        Err(fs_error(e, to))
                    }
                }
            }
        }
    }

    /// Where `rename_over` puts the old file aside on a server without `posix-rename`.
    fn old_of(&self, path: &str) -> String {
        let (dir, name) = self.split(path);
        self.join(&dir, &format!(".{name}.kite-old"))
    }

    /// A cancel after a break between the two renames: the old file goes back to its name.
    pub async fn restore_old(&self, path: &str) {
        let Side::Remote(l) = self else { return };
        let sftp = &l.conn.session.sftp;
        let old = self.old_of(path);
        if sftp.lstat(path).await.is_err() && sftp.lstat(&old).await.is_ok() {
            let _ = sftp.rename(&old, path).await;
        }
    }

    /// A plain rename that never replaces: for moves inside one place.
    pub async fn rename_new(&self, from: &str, to: &str) -> Result<()> {
        match self {
            Side::Local => {
                let (f, t) = (from.to_owned(), to.to_owned());
                blocking(move || {
                    if std::fs::symlink_metadata(&t).is_ok() {
                        return Err(Error::Eexist { path: t });
                    }
                    std::fs::rename(&f, &t).map_err(|e| io_error(&e, &f))
                })
                .await
            }
            Side::Remote(l) => l.conn.session.sftp.rename(from, to).await.map(|_| ()).map_err(|e| fs_error(e, from)),
        }
    }

    pub async fn readlink(&self, path: &str) -> Result<String> {
        match self {
            Side::Local => {
                let p = path.to_owned();
                blocking(move || std::fs::read_link(&p).map(|t| t.to_string_lossy().into_owned()).map_err(|e| io_error(&e, &p))).await
            }
            Side::Remote(l) => {
                let name = l.conn.session.sftp.readlink(path).await.map_err(|e| fs_error(e, path))?;
                name.files.into_iter().next().map(|f| f.filename).ok_or_else(|| Error::Einval { reason: format!("{path}: empty link") })
            }
        }
    }

    /// A link on a server; an existing name in the way is replaced (links only, never folders).
    pub async fn symlink(&self, target: &str, link: &str) -> Result<()> {
        let Side::Remote(l) = self else {
            return Err(Error::Einval { reason: "links are not created on this PC".into() });
        };
        let sftp = &l.conn.session.sftp;
        if let Ok(a) = sftp.lstat(link).await {
            if a.attrs.is_dir() && !a.attrs.is_symlink() {
                return Err(Error::Eexist { path: link.into() });
            }
            sftp.remove(link).await.map_err(|e| fs_error(e, link))?;
        }
        // OpenSSH reads SYMLINK as (target, link), the reverse of the draft; russh-sftp sends its
        // first argument first, so the target goes there.
        sftp.symlink(target, link).await.map(|_| ()).map_err(|e| fs_error(e, link))
    }

    /// Deletes a file, a link or a whole tree (the source of a move).
    pub async fn delete_tree(&self, path: &str) -> Result<()> {
        match self {
            Side::Local => {
                let p = path.to_owned();
                blocking(move || {
                    let m = std::fs::symlink_metadata(&p).map_err(|e| io_error(&e, &p))?;
                    if m.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) }.map_err(|e| io_error(&e, &p))
                })
                .await
            }
            Side::Remote(l) => crate::fs::remote::delete(&l.conn.session, path).await.map(|_| ()),
        }
    }

    /// Length of a partial file, None when there is none.
    pub async fn part_len(&self, path: &str) -> Result<Option<u64>> {
        Ok(self.lstat(path).await?.filter(|m| m.kind == Kind::File).map(|m| m.size))
    }

    /// Chunk size this side reads or writes best.
    pub fn chunk(&self) -> Option<usize> {
        self.conn().map(|c| c.chunk as usize)
    }
}

fn local_walk(root: &str) -> Result<Vec<Node>> {
    let mut out = Vec::new();
    let mut dirs = vec![(PathBuf::from(root), String::new())];
    while let Some((dir, rel)) = dirs.pop() {
        let read = std::fs::read_dir(&dir).map_err(|e| io_error(&e, &dir.to_string_lossy()))?;
        let mut level = Vec::new();
        for item in read {
            let item = item.map_err(|e| io_error(&e, &dir.to_string_lossy()))?;
            let name = item.file_name().to_string_lossy().into_owned();
            let path = item.path();
            let m = std::fs::symlink_metadata(&path).map_err(|e| io_error(&e, &path.to_string_lossy()))?;
            let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            level.push((path, r, Meta::of_local(&m)));
        }
        for (path, r, meta) in level {
            if meta.kind == Kind::Dir {
                dirs.push((path, r.clone()));
            }
            out.push(Node { rel: r, meta });
        }
    }
    // Parents before children: a folder's path is a prefix of everything under it.
    out.sort_by(|a, b| a.rel.split('/').cmp(b.rel.split('/')));
    Ok(out)
}

/* ---------- Bytes ---------- */

/// What the writer does when the last byte is in.
pub struct Finish {
    /// Date to set on the file.
    pub mtime: Option<u64>,
}

/// Copies `size` bytes of `src` from `offset` into `dst` at the same offset. `dst` is created or,
/// with an offset, continued; bytes are counted into `done` as the writer takes them.
#[allow(clippy::too_many_arguments)]
pub async fn copy(
    from: &Side,
    src: &str,
    to: &Side,
    dst: &str,
    offset: u64,
    size: u64,
    finish: Finish,
    done: &AtomicU64,
    stop: &Stop,
) -> Result<()> {
    let chunk = to.chunk().or(from.chunk()).unwrap_or(LOCAL_CHUNK);
    let (tx, rx) = mpsc::channel::<Vec<u8>>(QUEUE);
    let reader = read(from, src, offset, size, chunk, tx, stop.clone());
    let writer = write(to, dst, offset, size, rx, finish, done, stop.clone());
    let (r, w) = tokio::join!(reader, writer);
    // A writer error is the root cause when the channel closed under the reader, and the other way round.
    match (r, w) {
        (Err(Error::Stopped), _) | (_, Err(Error::Stopped)) => Err(Error::Stopped),
        (_, Err(e)) => Err(e),
        (Err(e), _) => Err(e),
        (Ok(got), Ok(())) if got < size - offset => Err(Error::SourceChanged { path: src.into() }),
        _ => Ok(()),
    }
}

/// A server file that fits one read, fetched to this PC: READ and CLOSE go out together, and the
/// part is written, dated and renamed over `dst` in one blocking call. Saves a round trip and three
/// pool hops per file (small files). False when it does not apply or the read came back short:
/// the caller then takes the general path, which also reports the errors.
#[allow(clippy::too_many_arguments)]
pub async fn fetch_small(from: &Side, src: &str, to: &Side, part: &str, dst: &str, size: u64, mtime: Option<u64>, done: &AtomicU64) -> Result<bool> {
    let (Side::Remote(l), Side::Local) = (from, to) else { return Ok(false) };
    if size > l.conn.chunk as u64 {
        return Ok(false);
    }
    let sftp = &l.conn.session.sftp;
    let handle = sftp.open(src, OpenFlags::READ, FileAttributes::empty()).await.map_err(|e| fs_error(e, src))?.handle;
    let permit = l.conn.budget.acquire().await.map_err(|_| Error::Disconnected)?;
    let read = async {
        if size == 0 { Ok(Vec::new()) } else { sftp.read(handle.as_str(), 0, size as u32).await.map(|d| d.data) }
    };
    // `biased`: the read is queued before the close, and the server answers in order.
    let (read, _) = tokio::join!(biased; read, sftp.close(handle.as_str()));
    drop(permit);
    let Some(data) = read.ok().filter(|d| d.len() as u64 == size) else { return Ok(false) };
    let (p, d) = (part.to_owned(), dst.to_owned());
    blocking(move || {
        let mut f = std::fs::File::create(&p).map_err(|e| io_error(&e, &p))?;
        f.write_all(&data).map_err(|e| io_error(&e, &p))?;
        if let Some(t) = mtime {
            f.set_modified(file_time(t)).map_err(|e| io_error(&e, &p))?;
        }
        drop(f);
        std::fs::rename(&p, &d).map_err(|e| io_error(&e, &d))
    })
    .await?;
    done.fetch_add(size, Ordering::Relaxed);
    Ok(true)
}

/// Sends the file from `offset` in order; returns the bytes read.
async fn read(side: &Side, path: &str, offset: u64, size: u64, chunk: usize, tx: mpsc::Sender<Vec<u8>>, mut stop: Stop) -> Result<u64> {
    match side {
        Side::Local => {
            let p = path.to_owned();
            blocking(move || {
                let mut f = std::fs::File::open(&p).map_err(|e| io_error(&e, &p))?;
                if offset > 0 {
                    f.seek(SeekFrom::Start(offset)).map_err(|e| io_error(&e, &p))?;
                }
                let mut left = size - offset;
                let mut got = 0;
                while left > 0 {
                    if *stop.borrow() {
                        return Err(Error::Stopped);
                    }
                    let want = (chunk as u64).min(left) as usize;
                    let mut buf = vec![0; want];
                    let mut filled = 0;
                    while filled < want {
                        match f.read(&mut buf[filled..]).map_err(|e| io_error(&e, &p))? {
                            0 => break,
                            n => filled += n,
                        }
                    }
                    if filled == 0 {
                        break;
                    }
                    buf.truncate(filled);
                    got += filled as u64;
                    left -= filled as u64;
                    if tx.blocking_send(buf).is_err() {
                        break; // the writer failed and reports it
                    }
                }
                Ok(got)
            })
            .await
        }
        Side::Remote(l) => {
            let sftp = &l.conn.session.sftp;
            let handle = sftp.open(path, OpenFlags::READ, FileAttributes::empty()).await.map_err(|e| fs_error(e, path))?.handle;
            let res = tokio::select! {
                r = read_pipeline(&l.conn, &handle, path, offset, size, chunk as u64, &tx) => r,
                _ = stopped(&mut stop) => Err(Error::Stopped),
            };
            drop(tx);
            let _ = sftp.close(handle).await;
            res
        }
    }
}

/// A slot of the connection's request budget. A pipeline with requests of its own in flight only
/// takes a free one: waiting here would stop it collecting its replies, and those free slots. With
/// none in flight it waits.
async fn permit(conn: &Conn, idle: bool) -> Result<Option<OwnedSemaphorePermit>> {
    match conn.budget.clone().try_acquire_owned() {
        Ok(p) => Ok(Some(p)),
        Err(_) if idle => conn.budget.clone().acquire_owned().await.map(Some).map_err(|_| Error::Disconnected),
        Err(_) => Ok(None),
    }
}

/// Keeps `IN_FLIGHT` reads outstanding and passes them on in file order.
async fn read_pipeline(conn: &Conn, handle: &str, path: &str, start: u64, size: u64, chunk: u64, tx: &mpsc::Sender<Vec<u8>>) -> Result<u64> {
    let sftp = &conn.session.sftp;
    let _pipe = conn.pipe();
    let mut in_flight = FuturesOrdered::new();
    let (mut offset, mut got) = (start, 0u64);
    loop {
        while offset < size && in_flight.len() < conn.share(IN_FLIGHT) {
            let Some(permit) = permit(conn, in_flight.is_empty()).await? else { break };
            let len = chunk.min(size - offset);
            let at = offset;
            in_flight.push_back(async move {
                let r = sftp.read(handle, at, len as u32).await;
                drop(permit);
                (at, len, r)
            });
            offset += len;
        }
        let Some((at, len, res)) = in_flight.next().await else { return Ok(got) };
        let data = match res {
            Ok(d) => d.data,
            Err(e) if is_eof(&e) => return Ok(got),
            Err(e) => return Err(fs_error(e, path)),
        };
        let mut have = data.len() as u64;
        got += have;
        if data.is_empty() {
            return Ok(got);
        }
        if tx.send(data).await.is_err() {
            return Ok(got);
        }
        // Servers may return less than asked: fill the gap before the next queued reply.
        while have < len {
            match sftp.read(handle, at + have, (len - have) as u32).await {
                Ok(d) if !d.data.is_empty() => {
                    have += d.data.len() as u64;
                    got += d.data.len() as u64;
                    if tx.send(d.data).await.is_err() {
                        return Ok(got);
                    }
                }
                Ok(_) => return Ok(got),
                Err(e) if is_eof(&e) => return Ok(got),
                Err(e) => return Err(fs_error(e, path)),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn write(side: &Side, path: &str, offset: u64, size: u64, mut rx: mpsc::Receiver<Vec<u8>>, finish: Finish, done: &AtomicU64, mut stop: Stop) -> Result<()> {
    match side {
        Side::Local => {
            let p = path.to_owned();
            // The blocking writer counts into its own counter; this task mirrors it into `done`.
            let counter = Arc::new(AtomicU64::new(0));
            let c = counter.clone();
            let job = blocking(move || {
                let mut f = if offset > 0 {
                    let mut f = std::fs::OpenOptions::new().write(true).open(&p).map_err(|e| io_error(&e, &p))?;
                    f.set_len(offset).map_err(|e| io_error(&e, &p))?;
                    f.seek(SeekFrom::Start(offset)).map_err(|e| io_error(&e, &p))?;
                    f
                } else {
                    std::fs::File::create(&p).map_err(|e| io_error(&e, &p))?
                };
                while let Some(chunk) = rx.blocking_recv() {
                    f.write_all(&chunk).map_err(|e| io_error(&e, &p))?;
                    c.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                }
                if let Some(t) = finish.mtime {
                    f.set_modified(file_time(t)).map_err(|e| io_error(&e, &p))?;
                }
                Ok(())
            });
            tokio::pin!(job);
            let mut seen = 0;
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            loop {
                tokio::select! {
                    r = &mut job => {
                        done.fetch_add(counter.load(Ordering::Relaxed) - seen, Ordering::Relaxed);
                        return if *stop.borrow() { r.and(Err(Error::Stopped)) } else { r };
                    }
                    _ = tick.tick() => {
                        let now = counter.load(Ordering::Relaxed);
                        done.fetch_add(now - seen, Ordering::Relaxed);
                        seen = now;
                    }
                    _ = stopped(&mut stop) => {
                        // The reader stops and closes the channel; the job then ends on its own.
                        let _ = (&mut job).await;
                        done.fetch_add(counter.load(Ordering::Relaxed) - seen, Ordering::Relaxed);
                        return Err(Error::Stopped);
                    }
                }
            }
        }
        Side::Remote(l) => {
            let sftp = &l.conn.session.sftp;
            let flags = if offset > 0 { OpenFlags::WRITE } else { OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE };
            // The part stays writable for a resume: its mode comes at the end, with the date.
            let handle = sftp.open(path, flags, FileAttributes::empty()).await.map_err(|e| fs_error(e, path))?.handle;
            let res = tokio::select! {
                r = write_pipeline(&l.conn, &handle, path, offset, &mut rx, done) => r,
                _ = stopped(&mut stop) => Err(Error::Stopped),
            };
            drop(rx);
            let res = match res {
                Ok(end) if end < size => {
                    // The source ended early; do not leave stale bytes of an older part after it.
                    let mut a = FileAttributes::empty();
                    a.size = Some(end);
                    sftp.fsetstat(handle.as_str(), a).await.map(|_| ()).map_err(|e| fs_error(e, path))
                }
                r => r.map(|_| ()),
            };
            // Date and close go together: the server applies them in order, nothing is written after.
            // The mode comes after the rename, so a part always stays writable for a resume.
            let set = async {
                let Some(t) = finish.mtime.filter(|_| res.is_ok()) else { return Ok(()) };
                let mut a = FileAttributes::empty();
                a.atime = Some(t as u32);
                a.mtime = Some(t as u32);
                sftp.fsetstat(handle.as_str(), a).await.map(|_| ()).map_err(|e| fs_error(e, path))
            };
            let (set, closed) = tokio::join!(biased; set, sftp.close(handle.as_str()));
            res?;
            set?;
            closed.map(|_| ()).map_err(|e| fs_error(e, path))
        }
    }
}

/// Writes chunks as they come with up to `IN_FLIGHT` outstanding. Nothing goes out further than
/// `IN_FLIGHT` chunks past the first unconfirmed byte, so after a break the file can only have holes
/// within that window of its end: resume steps back by it. Returns the end offset.
async fn write_pipeline(conn: &Conn, handle: &str, path: &str, start: u64, rx: &mut mpsc::Receiver<Vec<u8>>, done: &AtomicU64) -> Result<u64> {
    let sftp = &conn.session.sftp;
    let _pipe = conn.pipe();
    let window = IN_FLIGHT as u64 * conn.chunk as u64;
    let mut in_flight = FuturesUnordered::new();
    // Outstanding writes by offset: the first key is the lowest unconfirmed byte.
    let mut open: BTreeMap<u64, u64> = BTreeMap::new();
    let (mut offset, mut reading) = (start, true);
    let mut held: Option<Vec<u8>> = None;
    loop {
        let floor = open.keys().next().copied().unwrap_or(offset);
        let room = in_flight.len() < conn.share(IN_FLIGHT) && offset - floor < window;
        if room && let Some(data) = held.take() {
            match permit(conn, in_flight.is_empty()).await? {
                Some(permit) => {
                    let (at, len) = (offset, data.len() as u64);
                    offset += len;
                    open.insert(at, len);
                    in_flight.push(async move {
                        let r = sftp.write(handle, at, data).await;
                        drop(permit);
                        (at, len, r)
                    });
                    continue;
                }
                // The budget is spent: a reply of ours below frees a slot.
                None => held = Some(data),
            }
        }
        tokio::select! {
            biased;
            Some((at, len, res)) = in_flight.next(), if !in_flight.is_empty() => {
                res.map_err(|e| fs_error(e, path))?;
                open.remove(&at);
                done.fetch_add(len, Ordering::Relaxed);
            }
            chunk = rx.recv(), if reading && held.is_none() => match chunk {
                Some(data) => held = Some(data),
                None => reading = false,
            },
            else => return Ok(offset),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_walk_is_parents_first_and_sorted() {
        let dir = std::env::temp_dir().join(format!("kite-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("b/c")).unwrap();
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::write(dir.join("b/c/f.txt"), b"x").unwrap();
        std::fs::write(dir.join("a/z"), b"yy").unwrap();
        std::fs::write(dir.join("b.txt"), b"").unwrap();
        let rels: Vec<String> = local_walk(&dir.to_string_lossy()).unwrap().into_iter().map(|n| n.rel).collect();
        assert_eq!(rels, ["a", "a/z", "b", "b/c", "b/c/f.txt", "b.txt"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn server_names_that_escape_are_refused() {
        for bad in ["", ".", "..", "a/b"] {
            assert!(!local_name_ok(bad), "{bad:?}");
        }
        assert!(local_name_ok("report.final.txt"));
        assert!(local_name_ok(".bashrc"));
        assert!(local_rel_ok("a/b/c.txt"));
        assert!(!local_rel_ok("a/../b"));
        if cfg!(windows) {
            for bad in [r"..\..\x.bat", r"C:\Users\Public\x.bat", "a.txt:stream", "CON", "nul.txt", "com1", "dot.", "space ", "q?", "tab\t", "COM0", "lpt0.log", "COM¹", "LPT³.txt"] {
                assert!(!local_name_ok(bad), "{bad:?}");
            }
            assert!(local_name_ok("CONSOLE.txt"));
            assert!(local_name_ok("COM10"));
            assert!(local_name_ok("LPT"));
            assert!(!local_rel_ok(r"ok/..\..\evil"));
        }
    }

    #[test]
    fn remote_split_and_part() {
        assert_eq!(part_name("a.tar"), ".a.tar.kite-part");
        assert_eq!(remote_join("/srv", "a/b"), "/srv/a/b");
        assert_eq!(remote_join("/", "a"), "/a");
        assert_eq!(remote_join("/srv", ""), "/srv");
    }
}
