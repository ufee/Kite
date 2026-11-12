//! Editing in an external program (spec section 10).
//!
//! A server file is downloaded to a copy of its own, `<local data>/edit/<id>/<name>`, and opened in
//! the editor. The copies' folder is watched; a save settles for 300 ms (many editors save through a
//! temporary file and a rename) and goes back through the transfer queue, so a save that meets a
//! break waits there instead of being lost. Before it goes, the server file's size and date are
//! compared with the ones the copy came with: if someone changed it meanwhile, the user picks whose
//! version stays. Copies go when the app closes, except those an unfinished upload still reads.
//! Local files open in the editor as they are.
//!
//! A copy has one step in flight at a time: a download (`fetching`), a send being checked and queued
//! (`busy`), a queued upload (`upload`), another upload to the same file it waits for (`after`) or the
//! server's state learned after it (`settling`). A save that comes meanwhile sets `again` and goes
//! after it. The check against the server is made again by the queue right before the rename
//! (`Item.expect`), so a save that waited for a link does not overwrite someone else's change.

pub mod editor;

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use russh_sftp::protocol::FileAttributes;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast::error::RecvError;

use crate::{
    error::{Error, Result},
    fs::{io_error, remote},
    log::{Log, State as Row},
    msg,
    ssh::{Session, Ssh, sftp::fs_error},
    transfer::{End, Keep, Pick, St, Stamp, Transfers, local_name_ok},
};

/// Editors save in steps (temporary file, rename, attributes): the last event of a save comes within this.
const SETTLE: Duration = Duration::from_millis(300);

/// The file an event is about, as the UI knows it.
#[derive(Serialize, Clone)]
struct Event {
    server: String,
    user: String,
    path: String,
}

#[derive(Serialize, Clone)]
struct Changed {
    #[serde(flatten)]
    at: Event,
    /// False: autosave is off, the UI asks before `edit_upload`.
    uploaded: bool,
}

#[derive(Serialize, Clone)]
struct Failed {
    #[serde(flatten)]
    at: Event,
    err: Error,
}

/// A save the editor made that is on no way to the server: closing the app would lose it.
#[derive(Serialize)]
pub struct Unsent {
    server: String,
    name: String,
}

struct Copy {
    server: String,
    user: String,
    /// The path the pane opened: the UI knows the file by it.
    shown: String,
    /// The file itself: a link is followed, so the save replaces its target and the link stays.
    real: String,
    file: PathBuf,
    /// The server file when the copy was taken or last saved; None when unknown (no check then).
    remote: Option<Stamp>,
    keep: Option<Keep>,
    /// The copy as last looked at: an event without a change is the editor touching it, or us.
    seen: Option<(u64, SystemTime)>,
    /// Saved in the editor, not on the server yet.
    dirty: bool,
    /// The queue item taking the last save up.
    upload: Option<u64>,
    /// Another upload to the same file (an earlier run's, a transfer, a copy through a link): the
    /// save goes after it, as an upload of its own. Never taken for this copy's own: it reads another file.
    after: Option<u64>,
    /// The item of a save that failed: the next one, or the server coming back, sends it again.
    failed: Option<u64>,
    /// Saved again while a step was in flight: goes up after it.
    again: bool,
    /// The user chose this version over the server's while a step was in flight.
    force: bool,
    /// Being written by a download: its events are ours.
    fetching: bool,
    /// A send is checking the server and queueing the upload.
    busy: bool,
    /// An upload ended; the server file's new size and date are on their way.
    settling: bool,
    /// Counts events; a check runs only for the last one of a burst.
    tick: u64,
}

impl Copy {
    /// A step is in flight: a save waits for it.
    fn held(&self) -> bool {
        self.busy || self.settling || self.upload.is_some() || self.after.is_some()
    }
}

struct Inner {
    next: u64,
    copies: HashMap<u64, Copy>,
    /// Files being opened: a second F4 meanwhile would take a second copy.
    opening: HashSet<(String, String, String)>,
}

pub struct Edits {
    app: AppHandle,
    ssh: Arc<Ssh>,
    q: Arc<Transfers>,
    log: Arc<Log>,
    root: PathBuf,
    me: Weak<Edits>,
    inner: Mutex<Inner>,
    watcher: Mutex<Option<RecommendedWatcher>>,
    /// The "upload at once" setting: it holds for the copies already open too.
    autosave: AtomicBool,
}

fn einval(e: impl std::fmt::Display) -> Error {
    Error::Einval { reason: e.to_string() }
}

fn keep_of(a: &FileAttributes) -> Option<Keep> {
    Some(Keep { mode: a.permissions? & 0o7777, uid: a.uid?, gid: a.gid? })
}

fn split(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some((dir, name)) => (dir, name),
        None => (".", path),
    }
}

impl Edits {
    pub fn new(app: AppHandle, ssh: Arc<Ssh>, q: Arc<Transfers>, log: Arc<Log>, root: PathBuf) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&root);
        let me = Arc::new_cyclic(|me| Edits {
            app,
            ssh,
            q,
            log,
            root,
            me: me.clone(),
            inner: Mutex::new(Inner { next: 0, copies: HashMap::new(), opening: HashSet::new() }),
            watcher: Mutex::new(None),
            autosave: AtomicBool::new(true),
        });
        // Copies of secret files (`.env` 0600) are for this user alone.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&me.root, std::fs::Permissions::from_mode(0o700));
        }
        // Copies of the last run that no upload needs; the ids go on after the ones that stay.
        let kept = me.sweep();
        me.inner().next = kept.iter().filter_map(|n| n.parse::<u64>().ok()).max().unwrap_or(0);
        let w = Arc::downgrade(&me);
        let mut rx = me.q.ended();
        tauri::async_runtime::spawn(async move {
            loop {
                let ended = rx.recv().await;
                let Some(e) = w.upgrade() else { break };
                match ended {
                    // Each in a task of its own: the receiver keeps up with a queue of many small files.
                    Ok((item, st)) => {
                        tauri::async_runtime::spawn(async move { e.uploaded(item, st).await });
                    }
                    Err(RecvError::Lagged(_)) => e.reconcile(),
                    Err(RecvError::Closed) => break,
                }
            }
        });
        me
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Deletes the copies no unfinished upload reads from, and left downloads; the names of those that stay.
    fn sweep(&self) -> HashSet<String> {
        let need = self.q.local_sources();
        let mut kept = HashSet::new();
        for entry in std::fs::read_dir(&self.root).into_iter().flatten().flatten() {
            let path = entry.path();
            let gone = || if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            if need.contains(&path.display().to_string()) || gone().is_err() {
                kept.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
        kept
    }

    /// The app is closing: the watcher stops, the copies go.
    pub fn close(&self) {
        self.watcher.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.sweep();
    }

    /// Saves that closing would lose: neither on the server nor in the queue.
    /// A failed save counts as kept only while its item is in the queue: cleared or cancelled, the
    /// copy is all that is left of it. One waiting for another upload (`after`) is not in the queue yet.
    pub fn unsent(&self) -> Vec<Unsent> {
        let dirty: Vec<(Option<u64>, Unsent)> = {
            let g = self.inner();
            g.copies
                .values()
                .filter(|c| c.dirty && c.upload.is_none() && !c.settling)
                .map(|c| (c.failed, Unsent { server: c.server.clone(), name: split(&c.real).1.to_owned() }))
                .collect()
        };
        dirty.into_iter().filter(|(f, _)| !f.is_some_and(|f| self.q.state(f) == Some(St::Err))).map(|(_, u)| u).collect()
    }

    pub fn set_autosave(&self, on: bool) {
        self.autosave.store(on, Ordering::Relaxed);
    }

    fn autosave(&self) -> bool {
        self.autosave.load(Ordering::Relaxed)
    }

    fn find(&self, server: &str, user: &str, path: &str) -> Option<u64> {
        self.inner().copies.iter().find(|(_, c)| c.server == server && c.user == user && c.shown == path).map(|(id, _)| *id)
    }

    fn emit_at<T: Serialize + Clone>(&self, event: &str, id: u64, f: impl FnOnce(Event) -> T) {
        let at = {
            let g = self.inner();
            let Some(c) = g.copies.get(&id) else { return };
            Event { server: c.server.clone(), user: c.user.clone(), path: c.shown.clone() }
        };
        let _ = self.app.emit(event, f(at));
    }

    /* ---------- commands ---------- */

    pub async fn open_local(&self, path: String, editor: String) -> Result<()> {
        let file = path.clone();
        let app = tauri::async_runtime::spawn_blocking(move || {
            std::fs::metadata(&file).map_err(|e| io_error(&e, &file))?;
            editor::open(&editor, Path::new(&file))
        })
        .await
        .map_err(einval)??;
        self.log.add("local", "EDIT", format!("{path} · {app}"), "", Row::Ok);
        Ok(())
    }

    pub async fn open(&self, server: &str, user: &str, path: &str, editor: &str, autosave: bool) -> Result<()> {
        self.set_autosave(autosave);
        let key = (server.to_owned(), user.to_owned(), path.to_owned());
        // F4 again while the first is still downloading: the first one opens the editor.
        if !self.inner().opening.insert(key.clone()) {
            return Ok(());
        }
        let res = self.open_now(server, user, path, editor).await;
        self.inner().opening.remove(&key);
        res
    }

    async fn open_now(&self, server: &str, user: &str, path: &str, editor: &str) -> Result<()> {
        let s = self.ssh.session_live(server, user)?;
        let id = match self.find(server, user, path) {
            Some(id) => {
                // Opened again: a copy with nothing unsent takes the server's file afresh.
                let fresh = {
                    let g = self.inner();
                    let c = g.copies.get(&id).ok_or(Error::Cancelled)?;
                    !c.dirty && !c.held() && c.failed.is_none()
                };
                if fresh {
                    self.fetch(id, &s).await?;
                }
                id
            }
            None => self.take(&s, server, user, path).await?,
        };
        self.watch()?;
        let (file, name) = {
            let g = self.inner();
            let c = g.copies.get(&id).ok_or(Error::Cancelled)?;
            (c.file.clone(), split(&c.real).1.to_owned())
        };
        let ed = editor.to_owned();
        let app = tauri::async_runtime::spawn_blocking(move || editor::open(&ed, &file)).await.map_err(einval)??;
        self.log.add(&s.code, "EDIT", msg!("Слежу за {n} · {app}", n = name, app = app), "", Row::Ok);
        Ok(())
    }

    /// A first copy of a server file.
    async fn take(&self, s: &Session, server: &str, user: &str, path: &str) -> Result<u64> {
        let real = match s.sftp.realpath(path).await {
            Ok(n) => n.files.into_iter().next().map(|f| f.filename).unwrap_or_else(|| path.to_owned()),
            Err(_) => path.to_owned(),
        };
        let a = s.sftp.stat(real.as_str()).await.map_err(|e| fs_error(e, &real))?.attrs;
        if a.is_dir() {
            return Err(Error::Einval { reason: format!("{real}: is a folder") });
        }
        if a.permissions.is_some() && !a.file_type().is_file() {
            return Err(Error::NotRegular { path: real });
        }
        let name = split(&real).1.to_owned();
        if !local_name_ok(&name) {
            return Err(Error::BadName { path: name });
        }
        let id = {
            let mut g = self.inner();
            g.next += 1;
            g.next
        };
        let dir = self.root.join(id.to_string());
        std::fs::create_dir_all(&dir).map_err(|e| io_error(&e, &dir.display().to_string()))?;
        let copy = Copy {
            server: server.to_owned(),
            user: user.to_owned(),
            shown: path.to_owned(),
            real,
            file: dir.join(&name),
            remote: None,
            keep: None,
            seen: None,
            dirty: false,
            upload: None,
            after: None,
            failed: None,
            again: false,
            force: false,
            fetching: false,
            busy: false,
            settling: false,
            tick: 0,
        };
        self.inner().copies.insert(id, copy);
        if let Err(e) = self.fetch(id, s).await {
            self.inner().copies.remove(&id);
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        Ok(id)
    }

    /// The server file into the copy, over what it had. It is downloaded beside the copies' folders,
    /// where the watcher does not look, and replaces the copy only whole: a break leaves the copy as it was.
    async fn fetch(&self, id: u64, s: &Session) -> Result<()> {
        let (real, file) = {
            let mut g = self.inner();
            let c = g.copies.get_mut(&id).ok_or(Error::Cancelled)?;
            // A download of it is on its way already and brings the same file.
            if c.fetching {
                return Ok(());
            }
            c.fetching = true;
            (c.real.clone(), c.file.clone())
        };
        let part = self.root.join(format!("{id}.part"));
        let row = self.log.run(&s.code, "SFTP", format!("READ {real}"));
        let work = async {
            let a = s.sftp.stat(real.as_str()).await.map_err(|e| fs_error(e, &real))?.attrs;
            let size = remote::download(s, &real, &part).await?;
            let mtime = a.mtime.unwrap_or(0) as u64;
            // The copy carries the server's date: "keep dates" sends the save's own date back.
            let (p, f) = (part.clone(), file.clone());
            let seen = tauri::async_runtime::spawn_blocking(move || -> std::io::Result<(u64, SystemTime)> {
                std::fs::File::options().write(true).open(&p)?.set_modified(UNIX_EPOCH + Duration::from_secs(mtime))?;
                std::fs::rename(&p, &f)?;
                let m = std::fs::metadata(&f)?;
                Ok((m.len(), m.modified()?))
            })
            .await
            .map_err(einval)?
            .map_err(|e| io_error(&e, &file.display().to_string()))?;
            Ok::<_, Error>(((size, mtime), keep_of(&a), seen))
        };
        let res = work.await;
        if res.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        let mut g = self.inner();
        let Some(c) = g.copies.get_mut(&id) else { return Err(Error::Cancelled) };
        c.fetching = false;
        match res {
            Ok((stamp, keep, seen)) => {
                self.log.done(row, msg!("временная копия"), Row::Ok);
                c.remote = Some(stamp);
                c.keep = keep.or(c.keep);
                c.seen = Some(seen);
                c.dirty = false;
                Ok(())
            }
            Err(e) => {
                self.log.done(row, e.to_string(), Row::Err);
                Err(e)
            }
        }
    }

    /// The server's version wins: the copy takes it, the editor sees the file change.
    pub async fn reload(&self, server: &str, user: &str, path: &str) -> Result<()> {
        let id = self.find(server, user, path).ok_or(Error::Enoent { path: path.to_owned() })?;
        // The queue may be reading the copy: replacing it now would send the server's text back.
        if self.inner().copies.get(&id).is_some_and(Copy::held) {
            return Err(Error::Einval { reason: format!("{path}: the last save is still going to the server") });
        }
        let s = self.ssh.session_live(server, user)?;
        self.fetch(id, &s).await?;
        // The failed save is decided: its item leaves the queue, the copy is a fresh one again.
        let failed = self.inner().copies.get_mut(&id).and_then(|c| c.failed.take());
        if let Some(f) = failed {
            self.q.cancel(f);
        }
        Ok(())
    }

    /// The copy goes up; `force` skips the check against changes on the server.
    pub async fn upload(&self, server: &str, user: &str, path: &str, force: bool) -> Result<()> {
        let id = self.find(server, user, path).ok_or(Error::Enoent { path: path.to_owned() })?;
        self.send(id, force).await
    }

    /// One send at a time per copy; one asked for meanwhile goes after it.
    async fn send(&self, id: u64, mut force: bool) -> Result<()> {
        loop {
            {
                let mut g = self.inner();
                let c = g.copies.get_mut(&id).ok_or(Error::Cancelled)?;
                if c.held() {
                    c.again = true;
                    c.force |= force;
                    return Ok(());
                }
                c.busy = true;
            }
            let res = self.send_now(id, force).await;
            let retry = {
                let mut g = self.inner();
                let Some(c) = g.copies.get_mut(&id) else { return res.map(drop) };
                c.busy = false;
                // Handed to an item: its end takes `again`, even if it came before this.
                if matches!(res, Ok(true)) {
                    false
                } else {
                    // Nothing went up (a conflict, an error): a save meanwhile is in the copy the user
                    // decides about; only a choice of "mine" made meanwhile goes now.
                    c.again = false;
                    std::mem::take(&mut c.force)
                }
            };
            if !retry {
                return res.map(drop);
            }
            force = true;
        }
    }

    /// True when the save went to a queue item (its own, or one it waits for).
    async fn send_now(&self, id: u64, force: bool) -> Result<bool> {
        let (server, user, real, file, remote, mut keep, failed) = {
            let g = self.inner();
            let c = g.copies.get(&id).ok_or(Error::Cancelled)?;
            (c.server.clone(), c.user.clone(), c.real.clone(), c.file.clone(), c.remote, c.keep, c.failed)
        };
        // The server came back and the queue already sends the failed save: this one follows it.
        if let Some(f) = failed
            && self.q.state(f).is_some_and(St::active)
        {
            {
                let mut g = self.inner();
                if let Some(c) = g.copies.get_mut(&id) {
                    (c.upload, c.failed, c.again) = (Some(f), None, true);
                }
            }
            self.track(f);
            return Ok(true);
        }
        // Without a connection the save waits in the queue and goes unchecked when it is back.
        match self.ssh.session_live(&server, &user) {
            Ok(s) => match s.sftp.stat(real.as_str()).await.map_err(|e| fs_error(e, &real)) {
                Ok(a) => {
                    let now = (a.attrs.size.unwrap_or(0), a.attrs.mtime.unwrap_or(0) as u64);
                    if !force && remote.is_some_and(|r| r != now) {
                        self.emit_at("edit://conflict", id, |at| at);
                        return Ok(false);
                    }
                    keep = keep_of(&a.attrs).or(keep);
                }
                // Deleted on the server: the save makes it again.
                Err(Error::Enoent { .. } | Error::Disconnected) => {}
                Err(e) => return Err(e),
            },
            Err(Error::Disconnected) => {}
            Err(e) => return Err(e),
        }
        // The queue checks the server file again right before the rename; "mine" skips it.
        let expect = if force { None } else { remote };
        // A failed save is sent again rather than queued twice; the copy holds the latest text anyway.
        if let Some(f) = failed
            && self.q.resend(f, expect)
        {
            {
                let mut g = self.inner();
                if let Some(c) = g.copies.get_mut(&id) {
                    (c.upload, c.failed, c.again) = (Some(f), None, false);
                }
            }
            self.track(f);
            return Ok(true);
        }
        let size = std::fs::metadata(&file).map_err(|e| io_error(&e, &file.display().to_string()))?.len();
        let (dir, name) = split(&real);
        let from = End { src: "local".into(), user: String::new(), path: file.parent().map(|p| p.display().to_string()).unwrap_or_default() };
        let to = End { src: server, user, path: dir.to_owned() };
        let (item, new) = self.q.enqueue_edit(from, Pick { name: name.to_owned(), dir: false, size }, to, keep, expect).await?;
        if new {
            self.q.quiet(item);
        }
        {
            let mut g = self.inner();
            if let Some(c) = g.copies.get_mut(&id) {
                if new {
                    (c.upload, c.failed, c.again) = (Some(item), None, false);
                } else {
                    // Another upload to the file is on its way: this save waits for it, then goes as its own.
                    (c.after, c.again) = (Some(item), true);
                }
            }
        }
        self.track(item);
        // A paused one (left from the last run, or paused by hand) holds the save until it is resumed.
        if !new && self.q.state(item) == Some(St::Pause) {
            self.emit_at("edit://failed", id, |at| Failed { at, err: Error::EditWaits { path: real } });
        }
        Ok(true)
    }

    /// The copy waits for `item` now; if it ended before the copy noted it, its end is taken from the queue.
    fn track(&self, item: u64) {
        let st = match self.q.state(item) {
            Some(st) if st.active() => return,
            Some(st) => st,
            None => St::Err,
        };
        let me = self.me.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(e) = me.upgrade() {
                e.uploaded(item, st).await;
            }
        });
    }

    /* ---------- the watcher ---------- */

    fn watch(&self) -> Result<()> {
        let mut w = self.watcher.lock().unwrap_or_else(|e| e.into_inner());
        if w.is_some() {
            return Ok(());
        }
        let (me, root) = (self.me.clone(), self.root.clone());
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let (Ok(ev), Some(e)) = (res, me.upgrade()) else { return };
            // Only paths inside a copy's folder: a download's `<id>.part` beside them is not a number.
            let ids: HashSet<u64> = ev
                .paths
                .iter()
                .filter_map(|p| p.strip_prefix(&root).ok()?.components().next()?.as_os_str().to_str()?.parse().ok())
                .collect();
            for id in ids {
                e.touch(id);
            }
        })
        .map_err(einval)?;
        watcher.watch(&self.root, RecursiveMode::Recursive).map_err(einval)?;
        *w = Some(watcher);
        Ok(())
    }

    fn touch(&self, id: u64) {
        let tick = {
            let mut g = self.inner();
            let Some(c) = g.copies.get_mut(&id) else { return };
            c.tick += 1;
            c.tick
        };
        let me = self.me.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(SETTLE).await;
            if let Some(e) = me.upgrade() {
                e.check(id, tick).await;
            }
        });
    }

    /// After a burst of events: was the copy saved?
    async fn check(&self, id: u64, tick: u64) {
        let file = {
            let g = self.inner();
            match g.copies.get(&id) {
                Some(c) if c.tick == tick && !c.fetching => c.file.clone(),
                _ => return,
            }
        };
        // Mid-rename the file may be missing for a moment: the rename brings another event.
        let Ok(now) = std::fs::metadata(&file).and_then(|m| Ok((m.len(), m.modified()?))) else { return };
        let next = {
            let mut g = self.inner();
            let Some(c) = g.copies.get_mut(&id) else { return };
            if c.tick != tick || c.fetching || c.seen == Some(now) {
                return;
            }
            c.seen = Some(now);
            c.dirty = true;
            let name = split(&c.real).1.to_owned();
            let code = self.ssh.session_live(&c.server, &c.user).map(|s| s.code.clone()).unwrap_or_else(|_| c.server.clone());
            self.log.add(&code, "EDIT", msg!("{n} изменён в редакторе", n = name), "", Row::Ok);
            if c.held() {
                c.again = true;
                return;
            }
            self.autosave()
        };
        self.saved(id, next).await;
    }

    /// A save to take up: at once with autosave, otherwise the UI asks first.
    async fn saved(&self, id: u64, autosave: bool) {
        if !autosave {
            self.emit_at("edit://changed", id, |at| Changed { at, uploaded: false });
            return;
        }
        if let Err(err) = self.send(id, false).await {
            self.emit_at("edit://failed", id, |at| Failed { at, err });
        }
    }

    /// Ends the receiver missed: the copies learn them from the queue itself.
    fn reconcile(&self) {
        let items: Vec<(u64, bool)> = {
            let g = self.inner();
            g.copies
                .values()
                .flat_map(|c| [c.upload.map(|i| (i, false)), c.after.map(|i| (i, false)), c.failed.map(|i| (i, true))].into_iter().flatten())
                .collect()
        };
        for (item, failed) in items {
            let st = match self.q.state(item) {
                Some(st) if st.active() => continue,
                Some(St::Err) if failed => continue,
                Some(st) => st,
                // Cleared off the list: its end is unknown, the next save goes as a new upload.
                None if failed => continue,
                None => St::Err,
            };
            let me = self.me.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(e) = me.upgrade() {
                    e.uploaded(item, st).await;
                }
            });
        }
    }

    /// A queue item ended; if it carried a save, the copy learns the server file as it is now.
    async fn uploaded(&self, item: u64, st: St) {
        // `own`: the copy's save; otherwise another upload it waited for (`after`).
        let ids: Vec<(u64, bool)> = {
            let mut g = self.inner();
            let mut ids = Vec::new();
            for (id, c) in g.copies.iter_mut() {
                let own = if c.upload == Some(item) || c.failed == Some(item) {
                    c.upload = None;
                    c.failed = (st == St::Err).then_some(item);
                    true
                } else if c.after == Some(item) {
                    c.after = None;
                    false
                } else {
                    continue;
                };
                // Until the new size and date are in, a save meanwhile would be checked against the old ones.
                c.settling = true;
                ids.push((*id, own));
            }
            ids
        };
        for (id, own) in ids {
            self.settle(id, item, st, own).await;
        }
    }

    async fn settle(&self, id: u64, item: u64, st: St, own: bool) {
        let Some((server, user, real)) = ({
            let g = self.inner();
            g.copies.get(&id).map(|c| (c.server.clone(), c.user.clone(), c.real.clone()))
        }) else {
            return;
        };
        let done = own && st == St::Done;
        // The queue met another version on the server right before the rename: the same question as before the send.
        let changed = own && st == St::Err && self.q.err_code(item).as_deref() == Some("SERVER_CHANGED");
        let mut stamp = None;
        if done
            && let Ok(s) = self.ssh.session_live(&server, &user)
            && let Ok(a) = s.sftp.stat(real.as_str()).await
        {
            stamp = Some((a.attrs.size.unwrap_or(0), a.attrs.mtime.unwrap_or(0) as u64));
        }
        let (again, force) = {
            let mut g = self.inner();
            let Some(c) = g.copies.get_mut(&id) else { return };
            c.settling = false;
            if done {
                // Unknown: the next save goes without the check rather than with a false alarm.
                c.remote = stamp;
                c.dirty = c.again;
            }
            if changed {
                // The question covers the saves made meanwhile: the answer sends the copy as it is then.
                c.again = false;
            }
            (std::mem::take(&mut c.again), std::mem::take(&mut c.force))
        };
        if done {
            self.emit_at("edit://changed", id, |at| Changed { at, uploaded: true });
        }
        if changed && !force {
            self.emit_at("edit://conflict", id, |at| at);
        }
        if force {
            if let Err(err) = self.send(id, true).await {
                self.emit_at("edit://failed", id, |at| Failed { at, err });
            }
        } else if again {
            self.saved(id, self.autosave()).await;
        }
    }
}
