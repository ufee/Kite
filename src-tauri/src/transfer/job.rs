//! One queue item from start to end: a file or a whole folder, any route.
//!
//! Every file goes to `.name.kite-part` and is renamed over the target at the end, so a break never
//! spoils the file being replaced. A part whose source has the same size and date as when it was
//! started is continued from its length; on a server minus the write window, where a break can leave
//! holes. A folder keeps a mark: how many of its files in sorted order are done.

use std::sync::{Arc, atomic::Ordering};

use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::Semaphore;

use super::{
    Item, Prog, Resume, St, Transfers,
    io::{self, Finish, IN_FLIGHT, Kind, Meta, Side, Stop},
};
use crate::{
    error::{Error, Result},
    log::State as Row,
};

/// Small files of a folder in flight at once: hides the round trips on a far server.
const FILES_IN_FLIGHT: usize = 32;
/// Files above this go one at a time per item; each already keeps a deep pipeline.
const BIG: u64 = 4 * 1024 * 1024;
/// Folders created at once.
const DIRS_IN_FLIGHT: usize = 16;

pub struct Run<'a> {
    pub q: &'a Arc<Transfers>,
    pub id: u64,
    pub item: &'a Item,
    pub resume: &'a Resume,
    pub stop: &'a Stop,
    pub prog: &'a Prog,
    pub keep_time: bool,
}

/// The same files on both ends: a move there is a rename.
fn same_place(it: &Item) -> bool {
    it.from.src == it.to.src && (it.from.is_local() || it.from.user == it.to.user)
}

impl Run<'_> {
    fn done(&self) -> &std::sync::atomic::AtomicU64 {
        &self.prog.done
    }

    pub async fn go(&self, from: &Side, to: &Side) -> Result<()> {
        let it = self.item;
        let src = from.join(&it.from.path, &it.name);
        if to.is_local() && !io::local_name_ok(&it.tname) {
            return Err(Error::BadName { path: it.tname.clone() });
        }
        let dst = to.join(&it.to.path, &it.tname);
        // The picked item may be a link: its target is what the user sees in the panel.
        let meta = from.stat(&src).await?.ok_or_else(|| Error::Enoent { path: src.clone() })?;
        if meta.kind == Kind::Other {
            return Err(Error::NotRegular { path: src });
        }
        if it.mv && same_place(it) && self.rename(from, &src, to, &dst, &meta).await? {
            self.done().store(meta.size, Ordering::Relaxed);
            return Ok(());
        }
        // Moving a link sends what it points to and then deletes the link, never its target's files.
        let link = it.mv && from.lstat(&src).await?.is_some_and(|m| m.kind == Kind::Link);
        if meta.kind == Kind::Dir {
            self.folder(from, &src, to, &dst, &meta, it.mv && !link).await?;
        } else {
            self.q.set_size(self.id, meta.size);
            self.done().store(0, Ordering::Relaxed);
            self.file(from, &src, &meta, to, &dst, "").await?;
            if it.mv && !link {
                unchanged(from, &src, &meta, false).await?;
            }
        }
        if link {
            from.remove_link(&src).await?;
        } else if it.mv && meta.kind != Kind::Dir {
            from.delete_tree(&src).await?;
        }
        Ok(())
    }

    /// A move inside one place: true when the rename did it all.
    async fn rename(&self, from: &Side, src: &str, to: &Side, dst: &str, meta: &Meta) -> Result<bool> {
        match to.lstat(dst).await? {
            None => match from.rename_new(src, dst).await {
                Ok(()) => Ok(true),
                // Another drive of this PC: copy and delete.
                Err(Error::Einval { .. }) if from.is_local() => Ok(false),
                Err(e) => Err(e),
            },
            Some(m) if meta.kind != Kind::Dir && m.kind != Kind::Dir => match to.rename_over(src, dst).await {
                Ok(()) => Ok(true),
                Err(Error::Einval { .. }) if from.is_local() => Ok(false),
                Err(e) => Err(e),
            },
            // A folder into an existing folder merges: file by file, as a copy.
            _ => Ok(false),
        }
    }

    /// One file through its part. `rel` is its place in the item's folder, empty for a single file.
    async fn file(&self, from: &Side, src: &str, meta: &Meta, to: &Side, dst: &str, rel: &str) -> Result<()> {
        let part = to.part_of(dst);
        let mut offset = 0;
        if self.resume.parts.get(rel) == Some(&(meta.size, meta.mtime))
            && let Some(len) = to.part_len(&part).await?
            && len <= meta.size
        {
            offset = match to.conn() {
                // Writes may have landed out of order within the window: redo that much, plus one
                // chunk for the last write let in at the window's edge with uneven chunks.
                Some(c) => len.saturating_sub((IN_FLIGHT as u64 + 1) * c.chunk as u64),
                None => len,
            };
        }
        self.q.part_started(self.id, rel, meta.size, meta.mtime);
        self.done().fetch_add(offset, Ordering::Relaxed);
        let relay = !from.is_local() && !to.is_local();
        let finish = Finish { mtime: self.keep_time.then_some(meta.mtime), private: relay };
        if offset == 0 && io::fetch_small(from, src, to, &part, dst, meta.size, finish.mtime, self.done()).await? {
            self.q.part_done(self.id, rel);
            return Ok(());
        }
        io::copy(from, src, to, &part, offset, meta.size, finish, self.done(), self.stop).await?;
        to.rename_over(&part, dst).await?;
        self.q.part_done(self.id, rel);
        // Server to server the mode goes along, exact past the umask. A refusal costs only the
        // mode: the file stays 0600, narrower than its source.
        if relay {
            match to.chmod(dst, meta.mode & 0o777).await {
                Ok(()) => {}
                Err(e @ Error::Disconnected) => return Err(e),
                Err(e) => self.q.log_path(to, dst, e.to_string(), Row::Err),
            }
        }
        Ok(())
    }

    /// A file under the mark that is already at the target: same size, and same date when dates are kept.
    async fn same(&self, to: &Side, dst: String, meta: &Meta, i: usize) -> (usize, bool) {
        let same = match to.lstat(&dst).await {
            Ok(Some(m)) => m.kind == Kind::File && m.size == meta.size && (!self.keep_time || m.mtime == meta.mtime),
            _ => false,
        };
        (i, same)
    }

    /// One file of a folder; a move deletes its source right after, if it is still what was sent.
    #[allow(clippy::too_many_arguments)]
    async fn one(&self, from: &Side, src: String, to: &Side, dst: String, n: &io::Node, big: &Semaphore, i: usize, mv: bool) -> (usize, String, Result<()>) {
        let res = async {
            let _slot = if n.meta.size > BIG { big.acquire().await.ok() } else { None };
            self.file(from, &src, &n.meta, to, &dst, &n.rel).await?;
            if mv {
                unchanged(from, &src, &n.meta, true).await?;
                from.remove(&src).await?;
            }
            Ok(())
        }
        .await;
        (i, src, res)
    }

    /// Entries this PC cannot take as they are: names Windows reads as paths or devices, and names
    /// that differ only in case (one file there). Each one is logged and left out with what is under it.
    fn unfit(&self, from: &Side, src: &str, nodes: Vec<io::Node>) -> (Vec<io::Node>, Option<Error>) {
        let fold = cfg!(windows);
        let mut seen: std::collections::HashMap<String, Kind> = std::collections::HashMap::new();
        let mut cut: Vec<String> = Vec::new();
        let mut first = None;
        let mut keep = Vec::with_capacity(nodes.len());
        for n in nodes {
            if cut.iter().any(|c| n.rel.starts_with(c.as_str())) {
                continue;
            }
            let key = if fold { n.rel.to_lowercase() } else { n.rel.clone() };
            let err = if !io::local_rel_ok(&n.rel) {
                Some(Error::BadName { path: n.rel.clone() })
            } else {
                match seen.get(&key) {
                    // Two folders that fold to one simply merge.
                    Some(Kind::Dir) if n.meta.kind == Kind::Dir => None,
                    Some(_) => Some(Error::NameClash { path: n.rel.clone() }),
                    None => None,
                }
            };
            match err {
                Some(e) => {
                    self.q.log_path(from, &from.join(src, &n.rel), e.to_string(), Row::Err);
                    if n.meta.kind == Kind::Dir {
                        cut.push(format!("{}/", n.rel));
                    }
                    first.get_or_insert(e);
                }
                None => {
                    seen.entry(key).or_insert(n.meta.kind);
                    keep.push(n);
                }
            }
        }
        (keep, first)
    }

    async fn folder(&self, from: &Side, src: &str, to: &Side, dst: &str, meta: &Meta, mv: bool) -> Result<()> {
        self.q.set_state(self.id, St::Count);
        let mut nodes = from.walk(src).await?;
        nodes.sort_by(|a, b| a.rel.split('/').cmp(b.rel.split('/')));
        let mut first: Option<Error> = None;
        if to.is_local() {
            let (keep, bad) = self.unfit(from, src, nodes);
            nodes = keep;
            first = bad;
        }
        let total = nodes.iter().filter(|n| n.meta.kind == Kind::File).map(|n| n.meta.size).sum();
        self.q.set_size(self.id, total);
        self.done().store(0, Ordering::Relaxed);
        self.q.set_state(self.id, St::Run);

        let relay = !from.is_local() && !to.is_local();
        // Server to server, folders are made 0700 and get their own modes after their files (a
        // 0555 folder would refuse them), so one cut short is never wider than its source. Only
        // folders this item made get a mode: a merge leaves the target's. Only a relay keeps the list.
        let mut made: std::collections::HashSet<String> = self.resume.made.iter().cloned().collect();
        if to.mkdir(dst, relay).await? && relay {
            self.q.dir_made(self.id, "");
            made.insert(String::new());
        }
        // Parents before children: one depth at a time, the folders of a depth together.
        let dirs: Vec<&io::Node> = nodes.iter().filter(|n| n.meta.kind == Kind::Dir).collect();
        let deepest = dirs.iter().map(|n| n.rel.matches('/').count()).max().unwrap_or(0);
        for depth in 0..=deepest {
            let level: Vec<&&io::Node> = dirs.iter().filter(|n| n.rel.matches('/').count() == depth).collect();
            let (mut next, mut fl) = (0, FuturesUnordered::new());
            loop {
                while fl.len() < DIRS_IN_FLIGHT && next < level.len() {
                    let n = level[next];
                    fl.push(mkdir(to, to.join(dst, &n.rel), &n.rel, relay));
                    next += 1;
                }
                match fl.next().await {
                    Some((rel, r)) => {
                        if r? && relay {
                            self.q.dir_made(self.id, rel);
                            made.insert(rel.to_owned());
                        }
                    }
                    None => break,
                }
            }
        }
        if *self.stop.borrow() {
            return Err(Error::Stopped);
        }

        for n in nodes.iter().filter(|n| n.meta.kind == Kind::Link || n.meta.kind == Kind::Other) {
            let s = from.join(src, &n.rel);
            if n.meta.kind == Kind::Link && relay {
                let made = async { to.symlink(&from.readlink(&s).await?, &to.join(dst, &n.rel)).await }.await;
                match made {
                    Ok(()) if mv => {
                        let _ = from.remove(&s).await;
                    }
                    Ok(()) => {}
                    Err(e @ Error::Disconnected) => return Err(e),
                    Err(e) => {
                        self.q.log_path(from, &s, e.to_string(), Row::Err);
                        first.get_or_insert(e);
                    }
                }
            } else {
                // Links of this PC mean nothing on a server, and Windows gets none.
                let what = if n.meta.kind == Kind::Link { "ссылка, пропущена" } else { "не обычный файл, пропущен" };
                self.q.log_path(from, &s, what, Row::Ok);
            }
        }

        let files: Vec<&io::Node> = nodes.iter().filter(|n| n.meta.kind == Kind::File).collect();
        // A move deletes what it has sent, so the source list shrinks and the mark means nothing.
        let mark = if mv { 0 } else { self.resume.files.min(files.len()) };
        let mut flags = vec![false; files.len()];
        // Files under the mark are checked, not trusted: the folder may have changed since.
        let (mut next, mut fl) = (0, FuturesUnordered::new());
        loop {
            while fl.len() < FILES_IN_FLIGHT && next < mark {
                fl.push(self.same(to, to.join(dst, &files[next].rel), &files[next].meta, next));
                next += 1;
            }
            match fl.next().await {
                Some((i, true)) => {
                    flags[i] = true;
                    self.done().fetch_add(files[i].meta.size, Ordering::Relaxed);
                }
                Some(_) => {}
                None => break,
            }
        }
        let mut watermark = flags.iter().take_while(|f| **f).count();
        self.q.files_done(self.id, watermark);

        let big = Semaphore::new(1);
        let big = &big;
        let todo: Vec<usize> = (0..files.len()).filter(|i| !flags[*i]).collect();
        let (mut next, mut sent) = (0, FuturesUnordered::new());
        loop {
            while sent.len() < FILES_IN_FLIGHT && next < todo.len() {
                let i = todo[next];
                sent.push(self.one(from, from.join(src, &files[i].rel), to, to.join(dst, &files[i].rel), files[i], big, i, mv));
                next += 1;
            }
            let Some((i, s, res)) = sent.next().await else { break };
            match res {
                Ok(()) => {
                    flags[i] = true;
                    let w = watermark + flags[watermark..].iter().take_while(|f| **f).count();
                    if w != watermark {
                        watermark = w;
                        self.q.files_done(self.id, watermark);
                    }
                }
                // The whole item stops: pause, cancel, or the connection is gone.
                Err(e @ (Error::Stopped | Error::Disconnected)) => return Err(e),
                // One file failed: the rest go on, the item ends with the first error.
                Err(e) => {
                    self.q.log_path(from, &s, e.to_string(), Row::Err);
                    first.get_or_insert(e);
                }
            }
        }
        drop(sent);

        if relay {
            // Modes of the folders, deepest first, so none closes before its children are done.
            // A refusal costs only the mode: the files are there, it goes to the log.
            let modes = dirs.iter().rev().map(|n| (to.join(dst, &n.rel), n.rel.as_str(), n.meta.mode)).chain([(dst.to_owned(), "", meta.mode)]);
            for (path, rel, mode) in modes {
                if !made.contains(rel) {
                    continue;
                }
                match to.chmod(&path, mode & 0o7777).await {
                    Ok(()) => {}
                    Err(e @ Error::Disconnected) => return Err(e),
                    Err(e) => self.q.log_path(to, &path, e.to_string(), Row::Err),
                }
            }
        }
        if mv {
            // Emptied folders go, deepest first; one with something left (a failed file) stays.
            for n in dirs.iter().rev() {
                let _ = from.rmdir(&from.join(src, &n.rel)).await;
            }
            let _ = from.rmdir(src).await;
        }
        first.map_or(Ok(()), Err)
    }
}

async fn mkdir<'a>(to: &Side, path: String, rel: &'a str, private: bool) -> (&'a str, Result<bool>) {
    (rel, to.mkdir(&path, private).await)
}

/// A move deletes its source only while it is still what was sent: a file written to meanwhile
/// stays, and the item ends with an error.
async fn unchanged(from: &Side, src: &str, meta: &Meta, own: bool) -> Result<()> {
    let now = if own { from.lstat(src).await? } else { from.stat(src).await? };
    match now {
        Some(m) if m.size == meta.size && m.mtime == meta.mtime => Ok(()),
        _ => Err(Error::SourceChanged { path: src.into() }),
    }
}

#[cfg(test)]
mod tests {
    /// The mark counts files in this order on every run, so it must not depend on the listing order.
    #[test]
    fn sort_order_is_stable_across_platforms() {
        let mut v = vec!["b.txt", "b/c", "a", "b", "a/z"];
        v.sort_by(|a, b| a.split('/').cmp(b.split('/')));
        assert_eq!(v, ["a", "a/z", "b", "b/c", "b.txt"]);
    }
}
