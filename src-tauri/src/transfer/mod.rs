//! The transfer queue. FIFO, 1–5 items at a time; a file or a folder is one item.
//!
//! The core owns the queue: the UI mirrors it from `transfer://state` (at most ten times a second)
//! and gets `transfer://batch` when every item of a batch has ended. The queue is saved to
//! `transfers.json`; after a restart unfinished items come back paused with their progress.
//!
//! Servers are reached over a transfer connection of their own (see `conn`). A dropped connection
//! is retried with pauses of 1, 3, 10 and 30 s while the item keeps its place; then it fails.

pub mod conn;
mod io;
mod job;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Notify, watch};

use self::{
    conn::Pool,
    io::{Side, Stop},
};
use crate::{
    error::{Error, Result},
    log::{Log, State as Row},
    msg,
    ssh::Ssh,
    store::Store,
};

const LOCAL: &str = "local";
const RECONNECT: [u64; 4] = [1, 3, 10, 30];
/// Progress events: at most ten a second for the whole queue.
const TICK: Duration = Duration::from_millis(100);
/// The queue on disk while something runs.
const SAVE_EVERY: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct End {
    /// `"local"` or a server id.
    pub src: String,
    pub user: String,
    pub path: String,
}

impl End {
    fn is_local(&self) -> bool {
        self.src == LOCAL
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum St {
    Wait,
    Count,
    Run,
    Pause,
    Done,
    Err,
}

impl St {
    fn active(self) -> bool {
        matches!(self, St::Wait | St::Count | St::Run | St::Pause)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    Replace,
    Both,
    Skip,
}

/// A picked entry as the panel has it.
#[derive(Clone, Debug, Deserialize)]
pub struct Pick {
    pub name: String,
    pub dir: bool,
    #[serde(default)]
    pub size: u64,
}

/// What the UI shows of an item.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: u64,
    pub batch: u64,
    pub name: String,
    /// Target name; differs from `name` with "keep both".
    pub tname: String,
    pub dir: bool,
    pub size: u64,
    pub done: u64,
    pub from: End,
    pub to: End,
    #[serde(rename = "move")]
    pub mv: bool,
    pub state: St,
    /// The core error as the UI words it; null unless the state is `err`.
    #[serde(default)]
    pub err: Option<Value>,
    /// No rights on the target: the queue offers "as root".
    #[serde(default)]
    pub denied: bool,
}

/// What a resume needs: parts being written with their source's size and date, and for a folder
/// how many of its files in sorted order are done.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Resume {
    #[serde(default)]
    pub files: usize,
    #[serde(default)]
    pub parts: BTreeMap<String, (u64, u64)>,
}

#[derive(Default)]
pub struct Prog {
    pub done: AtomicU64,
}

struct Live {
    stop: watch::Sender<bool>,
    prog: Arc<Prog>,
    /// Set before the stop when the item is cancelled: its parts are deleted.
    cancel: AtomicBool,
}

struct Rec {
    item: Item,
    resume: Resume,
    live: Option<Arc<Live>>,
    /// Log row of the item while it runs.
    row: Option<u64>,
}

impl Rec {
    /// The item with the progress of its running job.
    fn now(&self) -> Item {
        let mut item = self.item.clone();
        if let Some(l) = &self.live {
            item.done = l.prog.done.load(Ordering::Relaxed);
        }
        item
    }
}

#[derive(Serialize, Deserialize)]
struct Saved {
    #[serde(flatten)]
    item: Item,
    #[serde(default)]
    resume: Resume,
}

#[derive(Serialize, Deserialize, Default)]
struct Disk {
    #[serde(default)]
    items: Vec<Saved>,
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    next_batch: u64,
}

struct Inner {
    recs: Vec<Rec>,
    next_id: u64,
    next_batch: u64,
    parallel: usize,
    keep_time: bool,
    reported: HashSet<u64>,
    /// Cancelled items whose job has not stopped yet: their parts are deleted when it has.
    gone: Vec<Rec>,
    /// Something changed that the disk does not have yet.
    dirty: bool,
}

#[derive(Serialize)]
struct View<'a> {
    #[serde(flatten)]
    item: &'a Item,
    speed: f64,
}

#[derive(Serialize, Clone)]
struct Batch {
    batch: u64,
    #[serde(rename = "move")]
    mv: bool,
    to: String,
    ok: usize,
    bad: usize,
    total: usize,
}

#[derive(Serialize, Clone)]
struct Changed<'a> {
    src: &'a str,
    path: &'a str,
}

pub struct Transfers {
    app: AppHandle,
    ssh: Arc<Ssh>,
    log: Arc<Log>,
    store: Arc<Store>,
    pool: Arc<Pool>,
    me: Weak<Transfers>,
    inner: Mutex<Inner>,
    kick: Notify,
}

impl Transfers {
    pub fn new(app: AppHandle, ssh: Arc<Ssh>, log: Arc<Log>, store: Arc<Store>) -> Arc<Self> {
        let disk: Disk = store.load_transfers().unwrap_or_default();
        let recs = disk
            .items
            .into_iter()
            .filter(|s| s.item.state != St::Done)
            .map(|Saved { mut item, resume }| {
                // Nothing heavy starts by itself after a restart.
                if item.state != St::Err {
                    item.state = St::Pause;
                }
                Rec { item, resume, live: None, row: None }
            })
            .collect::<Vec<_>>();
        let next_id = recs.iter().map(|r| r.item.id).max().unwrap_or(0).max(disk.next_id);
        let next_batch = recs.iter().map(|r| r.item.batch).max().unwrap_or(0).max(disk.next_batch);
        // A batch with only errors left was reported before the restart.
        let reported = recs.iter().map(|r| r.item.batch).filter(|b| recs.iter().all(|r| r.item.batch != *b || r.item.state == St::Err)).collect();
        let me = Arc::new_cyclic(|me| Transfers {
            app,
            pool: Pool::new(ssh.clone()),
            ssh,
            log,
            store,
            me: me.clone(),
            inner: Mutex::new(Inner { recs, next_id, next_batch, parallel: 3, keep_time: true, reported, gone: Vec::new(), dirty: false }),
            kick: Notify::new(),
        });
        let t = me.clone();
        tauri::async_runtime::spawn(async move { t.publisher().await });
        me
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn changed(&self) {
        self.inner().dirty = true;
        self.kick.notify_one();
    }

    fn with<R>(&self, id: u64, f: impl FnOnce(&mut Rec) -> R) -> Option<R> {
        let mut g = self.inner();
        let Inner { recs, gone, .. } = &mut *g;
        recs.iter_mut().chain(gone.iter_mut()).find(|r| r.item.id == id).map(f)
    }

    /* ---------- commands ---------- */

    pub fn list(&self) -> Vec<Value> {
        let g = self.inner();
        g.recs.iter().map(|r| serde_json::to_value(View { item: &r.now(), speed: 0.0 }).unwrap_or_default()).collect()
    }

    pub async fn enqueue(&self, from: End, picks: Vec<Pick>, to: End, mv: bool, policy: Policy) -> Result<u64> {
        if from.src == "ext" {
            return Err(Error::Einval { reason: "drag and drop from Explorer comes in stage 7".into() });
        }
        if from.src == to.src && from.path == to.path {
            return Err(Error::Einval { reason: "same folder".into() });
        }
        for p in picks.iter().filter(|p| p.dir && from.src == to.src) {
            let inner = Side::Local.join(&from.path, &p.name);
            let (a, b) = if from.is_local() { (inner.to_lowercase(), to.path.to_lowercase()) } else { (crate::fs::join(&from.path, &p.name), to.path.clone()) };
            let sep = if from.is_local() { std::path::MAIN_SEPARATOR } else { '/' };
            if b == a || b.starts_with(&format!("{}{sep}", a.trim_end_matches(sep))) {
                return Err(Error::Einval { reason: format!("{}: into itself", p.name) });
            }
        }
        let names: HashSet<String> = if policy == Policy::Replace { HashSet::new() } else { self.names(&to).await? };
        let mut taken = names.clone();
        let mut g = self.inner();
        let batch = g.next_batch + 1;
        let mut added = 0;
        for p in picks {
            let mut tname = p.name.clone();
            if names.contains(&p.name) {
                match policy {
                    Policy::Skip => continue,
                    Policy::Both => tname = free_name(&taken, &p.name, p.dir),
                    Policy::Replace => {}
                }
            }
            // The same target twice at once would write one part from two sides.
            if g.recs.iter().any(|r| r.item.state.active() && r.item.to.src == to.src && r.item.to.path == to.path && r.item.tname == tname) {
                continue;
            }
            taken.insert(tname.clone());
            g.next_id += 1;
            let item = Item {
                id: g.next_id,
                batch,
                tname,
                dir: p.dir,
                size: if p.dir { 0 } else { p.size },
                done: 0,
                from: from.clone(),
                to: to.clone(),
                mv,
                state: St::Wait,
                err: None,
                denied: false,
                name: p.name,
            };
            g.recs.push(Rec { item, resume: Resume::default(), live: None, row: None });
            added += 1;
        }
        if added == 0 {
            return Ok(0);
        }
        g.next_batch = batch;
        drop(g);
        self.changed();
        self.schedule();
        Ok(batch)
    }

    /// Names in the target folder, for "skip" and "keep both".
    async fn names(&self, to: &End) -> Result<HashSet<String>> {
        let list = if to.is_local() {
            let p = to.path.clone();
            tauri::async_runtime::spawn_blocking(move || crate::fs::local::list(&p)).await.map_err(|e| Error::Einval { reason: e.to_string() })??
        } else {
            let s = self.ssh.session_live(&to.src, &to.user)?;
            crate::fs::remote::list(&s, &to.path).await?
        };
        Ok(list.into_iter().map(|e| e.name).collect())
    }

    pub fn pause(&self, id: u64) {
        let live = self.with(id, |r| {
            if matches!(r.item.state, St::Wait | St::Count | St::Run) {
                r.item.state = St::Pause;
                r.live.clone()
            } else {
                None
            }
        });
        if let Some(Some(l)) = live {
            let _ = l.stop.send(true);
        }
        self.changed();
        self.schedule();
    }

    pub fn resume(&self, id: u64) {
        self.with(id, |r| {
            if r.item.state == St::Pause {
                r.item.state = St::Wait;
            }
        });
        self.reopen(id);
        self.changed();
        self.schedule();
    }

    pub fn pause_all(&self, pause: bool) {
        let ids: Vec<u64> = self.inner().recs.iter().map(|r| r.item.id).collect();
        for id in ids {
            if pause { self.pause(id) } else { self.resume(id) }
        }
    }

    /// Cancels an item, or takes a finished one off the list. Parts it leaves are deleted.
    pub fn cancel(&self, id: u64) {
        let (gone, live) = {
            let mut g = self.inner();
            let Some(k) = g.recs.iter().position(|r| r.item.id == id) else { return };
            let mut gone = g.recs.remove(k);
            let live = gone.live.clone();
            if let Some(l) = &live {
                l.cancel.store(true, Ordering::Relaxed);
                // The job still runs: it keeps reporting its parts here until it stops.
                g.gone.push(Rec { item: gone.item.clone(), resume: gone.resume.clone(), live: gone.live.take(), row: None });
            }
            (gone, live)
        };
        match &live {
            Some(l) => {
                let _ = l.stop.send(true);
            }
            None if gone.item.state != St::Done && !gone.resume.parts.is_empty() => {
                if let Some(t) = self.me.upgrade() {
                    let (to, tname, parts) = (gone.item.to.clone(), gone.item.tname.clone(), gone.resume.parts.keys().cloned().collect());
                    tauri::async_runtime::spawn(async move { t.clean(&to, &tname, parts).await });
                }
            }
            None => {}
        }
        if let Some(row) = gone.row
            && gone.item.state.active()
        {
            self.log.done(row, "отменено", Row::Ok);
        }
        self.changed();
        self.schedule();
    }

    pub fn retry(&self, id: u64, as_root: bool) {
        self.with(id, |r| {
            if r.item.state == St::Err {
                if as_root {
                    r.item.to.user = "root".into();
                }
                r.item.state = St::Wait;
                r.item.err = None;
                r.item.denied = false;
                r.row = None;
            }
        });
        self.reopen(id);
        self.changed();
        self.schedule();
    }

    pub fn clear_done(&self) {
        self.inner().recs.retain(|r| r.item.state.active());
        self.changed();
    }

    pub fn set_parallel(&self, n: usize) {
        self.inner().parallel = n.clamp(1, 5);
        self.schedule();
    }

    pub fn set_keep_time(&self, on: bool) {
        self.inner().keep_time = on;
    }

    /// Disconnecting or deleting a server: its transfers pause or go, then its connections close.
    pub async fn detach(&self, server: &str, user: Option<&str>, cancel: bool) {
        let touches = |e: &End| e.src == server && user.is_none_or(|u| u == e.user);
        let ids: Vec<u64> = self.inner().recs.iter().filter(|r| r.item.state.active() && (touches(&r.item.from) || touches(&r.item.to))).map(|r| r.item.id).collect();
        for id in ids {
            if cancel { self.cancel(id) } else { self.pause(id) }
        }
        self.pool.close(server, user).await;
    }

    /// A batch that ended can end again after a resume or a retry.
    fn reopen(&self, id: u64) {
        let mut g = self.inner();
        if let Some(b) = g.recs.iter().find(|r| r.item.id == id).map(|r| r.item.batch) {
            g.reported.remove(&b);
        }
    }

    /* ---------- running ---------- */

    fn schedule(&self) {
        let Some(me) = self.me.upgrade() else { return };
        let start: Vec<(u64, Arc<Live>, watch::Receiver<bool>)> = {
            let mut g = self.inner();
            let mut running = g.recs.iter().filter(|r| r.live.is_some()).count();
            let parallel = g.parallel;
            let mut out = Vec::new();
            for r in g.recs.iter_mut() {
                if running >= parallel {
                    break;
                }
                if r.item.state != St::Wait || r.live.is_some() {
                    continue;
                }
                let (tx, rx) = watch::channel(false);
                let live = Arc::new(Live { stop: tx, prog: Arc::new(Prog::default()), cancel: AtomicBool::new(false) });
                live.prog.done.store(r.item.done, Ordering::Relaxed);
                r.live = Some(live.clone());
                r.item.state = if r.item.dir { St::Count } else { St::Run };
                running += 1;
                out.push((r.item.id, live, rx));
            }
            out
        };
        for (id, live, stop) in start {
            let me = me.clone();
            tauri::async_runtime::spawn(async move { me.drive(id, live, stop).await });
        }
        self.kick.notify_one();
    }

    /// Runs one item to its end, waiting out dropped connections.
    async fn drive(self: Arc<Self>, id: u64, live: Arc<Live>, stop: Stop) {
        let res = loop {
            let Some((item, resume, keep_time)) = self.with(id, |r| (r.item.clone(), r.resume.clone())).map(|(i, r)| (i, r, self.inner().keep_time)) else {
                break Err(Error::Stopped);
            };
            let res = self.attempt(id, &item, &resume, &live.prog, &stop, keep_time).await;
            if !matches!(res, Err(Error::Disconnected)) {
                break res;
            }
            self.log_note(id, "соединение потеряно · жду");
            let mut back = false;
            for pause in RECONNECT {
                let mut s = stop.clone();
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(pause)) => {}
                    _ = io::stopped(&mut s) => break,
                }
                if self.reachable(&item).await {
                    back = true;
                    break;
                }
            }
            if *stop.borrow() {
                break Err(Error::Stopped);
            }
            if !back {
                break Err(Error::Disconnected);
            }
        };
        self.finish(id, &live, res).await;
    }

    async fn side(&self, e: &End) -> Result<Side> {
        if e.is_local() { Ok(Side::Local) } else { Ok(Side::Remote(self.pool.get(&e.src, &e.user).await?)) }
    }

    async fn reachable(&self, it: &Item) -> bool {
        self.side(&it.from).await.is_ok() && self.side(&it.to).await.is_ok()
    }

    async fn attempt(self: &Arc<Self>, id: u64, item: &Item, resume: &Resume, prog: &Prog, stop: &Stop, keep_time: bool) -> Result<()> {
        let from = self.side(&item.from).await?;
        let to = self.side(&item.to).await?;
        self.open_row(id, item, &from, &to);
        let run = job::Run { q: self, id, item, resume, stop, prog, keep_time };
        // Copies stop by themselves and close their handles; a long walk or a hung request gets a
        // moment to do so, then is dropped.
        let mut s = stop.clone();
        let go = run.go(&from, &to);
        tokio::pin!(go);
        let lost = async {
            let conns: Vec<_> = [&from, &to].into_iter().filter_map(|s| s.conn()).collect();
            if conns.is_empty() {
                std::future::pending::<()>().await;
            }
            futures::future::select_all(conns.iter().map(|c| Box::pin(c.lost()))).await;
        };
        let res = tokio::select! {
            r = &mut go => r,
            _ = io::stopped(&mut s) => tokio::time::timeout(Duration::from_secs(2), &mut go).await.unwrap_or(Err(Error::Stopped)),
            // Requests in flight on a dead connection may never return.
            _ = lost => Err(Error::Disconnected),
        };
        if matches!(res, Err(Error::Disconnected)) {
            let conns: Vec<_> = [&from, &to].into_iter().filter_map(|s| s.conn().cloned()).collect();
            let closed: Vec<_> = conns.iter().filter(|c| c.is_closed()).collect();
            // A dead channel under a live connection: take down what we cannot tell apart.
            for c in if closed.is_empty() { conns.iter().collect() } else { closed } {
                self.pool.lost(c).await;
            }
        }
        res
    }

    async fn finish(&self, id: u64, live: &Live, res: Result<()>) {
        let cancelled = live.cancel.load(Ordering::Relaxed);
        let done = live.prog.done.load(Ordering::Relaxed);
        let mut changed: Vec<End> = Vec::new();
        let mut clean = None;
        {
            let mut g = self.inner();
            let rec = g.recs.iter_mut().find(|r| r.item.id == id && r.live.as_ref().is_some_and(|l| std::ptr::eq(Arc::as_ptr(l), live)));
            match rec {
                Some(r) => {
                    r.live = None;
                    r.item.done = done;
                    match res {
                        Ok(()) => {
                            r.item.state = St::Done;
                            r.item.done = r.item.size;
                            r.resume = Resume::default();
                            if let Some(row) = r.row.take() {
                                self.log.done(row, msg!("{size}", size = r.item.size), Row::Ok);
                            }
                            changed.push(r.item.to.clone());
                            if r.item.mv {
                                changed.push(r.item.from.clone());
                            }
                        }
                        Err(Error::Stopped) => {
                            // Paused: the part stays for the resume.
                            if r.item.state.active() && r.item.state != St::Pause {
                                r.item.state = St::Pause;
                            }
                        }
                        Err(e) => {
                            r.item.denied = matches!(&e, Error::Eacces { path } if !r.item.to.is_local() && r.item.to.user != "root" && path.starts_with(r.item.to.path.trim_end_matches('/')));
                            if let Some(row) = r.row.take() {
                                self.log.done(row, e.to_string(), Row::Err);
                            }
                            r.item.err = serde_json::to_value(&e).ok();
                            r.item.state = St::Err;
                            changed.push(r.item.to.clone());
                        }
                    }
                }
                None if cancelled => {
                    if let Some(k) = g.gone.iter().position(|r| r.item.id == id) {
                        let r = g.gone.remove(k);
                        clean = Some((r.item.to, r.item.tname, r.resume.parts.into_keys().collect::<Vec<_>>()));
                    }
                }
                None => {}
            }
        }
        // The publisher may have gone to sleep with nothing running: the end must reach the UI and the disk.
        self.changed();
        if let Some((to, tname, parts)) = clean {
            self.clean(&to, &tname, parts).await;
        }
        for e in changed {
            let _ = self.app.emit("fs://changed", Changed { src: &e.src, path: &e.path });
        }
        self.report();
        self.schedule();
    }

    /* ---------- bookkeeping from the job ---------- */

    fn open_row(&self, id: u64, item: &Item, from: &Side, to: &Side) {
        let code = |s: &Side| s.conn().map(|c| c.session.code.clone());
        let (op, src) = match (from.is_local(), to.is_local()) {
            (true, true) => ("COPY", LOCAL.to_owned()),
            (true, false) => ("PUT", code(to).unwrap_or_default()),
            (false, true) => ("GET", code(from).unwrap_or_default()),
            (false, false) => ("RELAY", code(to).unwrap_or_default()),
        };
        let text = format!("{} · {} → {}", item.tname, item.from.path, item.to.path);
        let log = &self.log;
        self.with(id, |r| {
            if r.row.is_none() {
                r.row = Some(log.run(&src, op, text));
            }
        });
    }

    fn log_note(&self, id: u64, note: &'static str) {
        if let Some(Some(row)) = self.with(id, |r| r.row) {
            self.log.done(row, note, Row::Run);
        }
    }

    /// A row about one path of a folder: a skipped link, a file that failed.
    fn log_path(&self, side: &Side, path: &str, note: impl Into<crate::log::Msg>, state: Row) {
        let src = side.conn().map(|c| c.session.code.clone()).unwrap_or_else(|| LOCAL.to_owned());
        let tag = if side.is_local() { "FS" } else { "SFTP" };
        self.log.add(&src, tag, path.to_owned(), note, state);
    }

    fn set_size(&self, id: u64, size: u64) {
        self.with(id, |r| r.item.size = size);
        self.kick.notify_one();
    }

    fn set_state(&self, id: u64, st: St) {
        self.with(id, |r| {
            if r.item.state.active() && r.item.state != St::Pause {
                r.item.state = st;
            }
        });
        self.kick.notify_one();
    }

    fn part_started(&self, id: u64, rel: &str, size: u64, mtime: u64) {
        self.with(id, |r| {
            r.resume.parts.insert(rel.to_owned(), (size, mtime));
        });
    }

    fn part_done(&self, id: u64, rel: &str) {
        self.with(id, |r| {
            r.resume.parts.remove(rel);
        });
    }

    fn files_done(&self, id: u64, n: usize) {
        self.with(id, |r| r.resume.files = n);
    }

    /// Deletes the parts of a cancelled item.
    async fn clean(&self, to: &End, tname: &str, parts: Vec<String>) {
        let Ok(side) = self.side(to).await else { return };
        let root = side.join(&to.path, tname);
        for rel in parts {
            let _ = side.remove(&side.part_of(&side.join(&root, &rel))).await;
        }
    }

    /// One toast per batch, when its last item has ended.
    fn report(&self) {
        let out: Vec<Batch> = {
            let mut g = self.inner();
            let batches: Vec<u64> = g.recs.iter().map(|r| r.item.batch).collect::<HashSet<_>>().into_iter().collect();
            let mut out = Vec::new();
            for b in batches {
                if g.reported.contains(&b) {
                    continue;
                }
                let items: Vec<&Item> = g.recs.iter().filter(|r| r.item.batch == b).map(|r| &r.item).collect();
                if items.iter().any(|i| i.state.active()) {
                    continue;
                }
                out.push(Batch {
                    batch: b,
                    mv: items[0].mv,
                    to: items[0].to.src.clone(),
                    ok: items.iter().filter(|i| i.state == St::Done).count(),
                    bad: items.iter().filter(|i| i.state == St::Err).count(),
                    total: items.len(),
                });
            }
            for b in &out {
                g.reported.insert(b.batch);
            }
            out
        };
        for b in out {
            let _ = self.app.emit("transfer://batch", b);
        }
    }

    /* ---------- events and the disk ---------- */

    /// Sends the queue at most every `TICK` while anything changes or runs, and saves it.
    async fn publisher(self: Arc<Self>) {
        let mut speeds: HashMap<u64, (u64, Instant, f64)> = HashMap::new();
        let mut saved_at = Instant::now();
        loop {
            let running = self.inner().recs.iter().any(|r| r.live.is_some());
            if running {
                tokio::select! {
                    _ = self.kick.notified() => {}
                    _ = tokio::time::sleep(TICK) => {}
                }
            } else {
                self.kick.notified().await;
            }
            let now = Instant::now();
            let (views, save) = {
                let mut g = self.inner();
                let mut views = Vec::with_capacity(g.recs.len());
                for r in &g.recs {
                    let item = r.now();
                    let mut speed = 0.0;
                    if r.live.is_some() {
                        let e = speeds.entry(item.id).or_insert((item.done, now, 0.0));
                        let dt = now.duration_since(e.1).as_secs_f64();
                        if dt >= 0.09 {
                            let rate = item.done.saturating_sub(e.0) as f64 / dt;
                            e.2 = if e.2 == 0.0 { rate } else { e.2 * 0.7 + rate * 0.3 };
                            *e = (item.done, now, e.2);
                        }
                        speed = e.2;
                    }
                    views.push(serde_json::to_value(View { item: &item, speed }).unwrap_or_default());
                }
                speeds.retain(|id, _| g.recs.iter().any(|r| r.item.id == *id && r.live.is_some()));
                let due = g.dirty || (g.recs.iter().any(|r| r.live.is_some()) && now.duration_since(saved_at) >= SAVE_EVERY);
                let save = due.then(|| {
                    g.dirty = false;
                    Disk {
                        items: g
                            .recs
                            .iter()
                            .map(|r| Saved { item: r.now(), resume: r.resume.clone() })
                            .collect(),
                        next_id: g.next_id,
                        next_batch: g.next_batch,
                    }
                });
                (views, save)
            };
            let _ = self.app.emit("transfer://state", views);
            if let Some(disk) = save {
                saved_at = now;
                let store = self.store.clone();
                let _ = tauri::async_runtime::spawn_blocking(move || store.save_transfers(&disk)).await;
            }
            tokio::time::sleep(TICK).await;
        }
    }
}

/// `name (2).ext` for a file, `name (2)` for a folder; the first number that is free.
fn free_name(taken: &HashSet<String>, name: &str, dir: bool) -> String {
    let dot = if dir { None } else { name.rfind('.').filter(|&d| d > 0) };
    let (stem, ext) = match dot {
        Some(d) => (&name[..d], &name[d..]),
        None => (name, ""),
    };
    (2..).map(|n| format!("{stem} ({n}){ext}")).find(|c| !taken.contains(c)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_names() {
        let taken: HashSet<String> = ["a.txt", "a (2).txt", "dir.v1"].iter().map(|s| s.to_string()).collect();
        assert_eq!(free_name(&taken, "a.txt", false), "a (3).txt");
        assert_eq!(free_name(&taken, "dir.v1", true), "dir.v1 (2)");
        assert_eq!(free_name(&taken, ".bashrc", false), ".bashrc (2)");
    }

    #[test]
    fn saved_queue_round_trip() {
        let item = Item {
            id: 1,
            batch: 1,
            name: "a".into(),
            tname: "a".into(),
            dir: true,
            size: 10,
            done: 4,
            from: End { src: "local".into(), user: String::new(), path: "C:\\x".into() },
            to: End { src: "s1".into(), user: "root".into(), path: "/srv".into() },
            mv: false,
            state: St::Run,
            err: None,
            denied: false,
        };
        let mut resume = Resume { files: 3, ..Default::default() };
        resume.parts.insert("b/c".into(), (100, 1_700_000_000));
        let text = serde_json::to_string(&Disk { items: vec![Saved { item, resume: resume.clone() }], next_id: 1, next_batch: 1 }).unwrap();
        assert!(text.contains("\"move\":false"));
        let back: Disk = serde_json::from_str(&text).unwrap();
        assert_eq!(back.items[0].resume, resume);
        assert_eq!(back.items[0].item.state, St::Run);
    }
}
