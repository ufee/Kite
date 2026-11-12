//! The transfer queue. FIFO, 1–5 items at a time; a file or a folder is one item.
//!
//! The core owns the queue: the UI mirrors it from `transfer://state` (the items that changed, at
//! most ten times a second) and gets `transfer://batch` when every item of a batch has ended. The
//! queue is saved to `transfers.json`; after a restart unfinished items come back paused with their
//! progress.
//!
//! Servers are reached over a transfer connection of their own (see `conn`). A dropped connection
//! is retried with pauses of 1, 3, 10 and 30 s while the item keeps its place; then it fails. So
//! does an item that keeps losing the connection right after getting it back.

pub mod conn;
mod io;
mod job;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Notify, broadcast, watch};

pub use self::io::local_name_ok;
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
/// Drops in a row with no byte moved in between, then the item fails.
const DROPS: usize = 4;
/// Progress events: at most ten a second for the whole queue.
const TICK: Duration = Duration::from_millis(100);
/// The queue on disk at most this often.
const SAVE_EVERY: Duration = Duration::from_secs(2);
/// Deleting the parts of a cancelled item, connecting included.
const CLEAN_WAIT: Duration = Duration::from_secs(15);
/// A disconnect that cancels waits this long for the parts to go before closing the connections.
const DETACH_WAIT: Duration = Duration::from_secs(10);

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

    /// Names here as the file system compares them: Windows folders ignore case.
    fn key(&self, name: &str) -> String {
        if cfg!(windows) && self.is_local() { name.to_lowercase() } else { name.to_owned() }
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
    pub fn active(self) -> bool {
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
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
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
    /// An edit going back: the file it replaces keeps its mode and owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<Keep>,
    /// An edit going back: the server file's size and date the copy was checked against. Anything
    /// else there right before the rename means someone changed it meanwhile (`SERVER_CHANGED`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Stamp>,
}

/// Size in bytes and date in seconds of a file.
pub type Stamp = (u64, u64);

/// Mode and owner of a server file an edit replaces: rename-over makes a new file of the uploader.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Keep {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

impl Item {
    /// Writes this target: the same server, folder and name as the file system compares them.
    /// An editor save going back to its server file.
    fn is_edit(&self) -> bool {
        self.keep.is_some() || self.expect.is_some()
    }

    fn writes(&self, to: &End, tname: &str) -> bool {
        self.to.src == to.src && to.key(&self.to.path) == to.key(&to.path) && to.key(&self.tname) == to.key(tname)
    }

    /// The second of two picks that are one name on the target: a retry would land on the first.
    fn twin(&self) -> bool {
        self.err.as_ref().is_some_and(|e| e["code"] == "NAME_CLASH" && e["path"] == self.name.as_str())
    }
}

/// What a resume needs: parts being written with their source's size and date, and for a folder
/// how many of its files in sorted order are done.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Resume {
    #[serde(default)]
    pub files: usize,
    #[serde(default)]
    pub parts: BTreeMap<String, (u64, u64)>,
    /// Folders of the item this transfer made (`""` is its root): only they get the source's mode.
    #[serde(default)]
    pub made: BTreeSet<String>,
}

#[derive(Default)]
pub struct Prog {
    pub done: AtomicU64,
}

struct Live {
    stop: watch::Sender<bool>,
    prog: Arc<Prog>,
}

struct Rec {
    item: Item,
    resume: Resume,
    live: Option<Arc<Live>>,
    /// Log row of the item while it runs.
    row: Option<u64>,
    /// Paused by a disconnect of its server: goes on when the server is connected again.
    held: bool,
}

impl Rec {
    fn new(item: Item, resume: Resume) -> Self {
        Rec { item, resume, live: None, row: None, held: false }
    }

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
    /// Cancelled and cleared items until their parts are deleted, after their job if it still runs.
    /// Their target does not start again meanwhile.
    gone: Vec<Rec>,
    /// Something changed that the disk does not have yet.
    dirty: bool,
    /// Edit uploads the editor reports itself (`quiet`).
    quiet: HashSet<u64>,
}

#[derive(Serialize)]
struct View<'a> {
    #[serde(flatten)]
    item: &'a Item,
    speed: f64,
}

/// One `transfer://state`: items new or changed since the last one, and ids no longer listed.
/// With `reset`, `up` is the whole list and replaces what the UI has.
#[derive(Serialize, Clone)]
struct Diff {
    up: Vec<Value>,
    del: Vec<u64>,
    reset: bool,
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
    /// An item left `gone`.
    settled: Notify,
    /// The UI took a fresh list: the next event carries every item.
    resend: AtomicBool,
    /// Items that ended, with how: edits wait for their upload here.
    ended: broadcast::Sender<(u64, St)>,
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
                Rec::new(item, resume)
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
            inner: Mutex::new(Inner { recs, next_id, next_batch, parallel: 3, keep_time: true, reported, gone: Vec::new(), dirty: false, quiet: HashSet::new() }),
            kick: Notify::new(),
            settled: Notify::new(),
            resend: AtomicBool::new(false),
            ended: broadcast::channel(64).0,
        });
        let w = Arc::downgrade(&me);
        me.ssh.on_live(move |server| {
            if let Some(t) = w.upgrade() {
                t.attach(server);
            }
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

    /// The UI has put its list in place: an event that crossed the reply may have been lost under
    /// it, so everything goes again.
    pub fn resync(&self) {
        self.resend.store(true, Ordering::Relaxed);
        self.kick.notify_one();
    }

    /// The queue to disk now: the app is closing and the publisher's next save would not come.
    pub fn save_now(&self) {
        let disk = self.disk(&mut self.inner());
        let _ = self.store.save_transfers(&disk);
    }

    fn disk(&self, g: &mut Inner) -> Disk {
        g.dirty = false;
        Disk { items: g.recs.iter().map(|r| Saved { item: r.now(), resume: r.resume.clone() }).collect(), next_id: g.next_id, next_batch: g.next_batch }
    }

    pub async fn enqueue(&self, from: End, picks: Vec<Pick>, to: End, mv: bool, policy: Policy) -> Result<u64> {
        Ok(self.enqueue_with(from, picks, to, mv, policy, (None, None)).await?.0)
    }

    /// An edited copy back to its server file, replacing it with its mode and owner kept; `expect` is
    /// the server file the copy was checked against, compared again right before the rename.
    /// The item id and true; or, when an upload to that file is still in the queue, its id and false.
    pub async fn enqueue_edit(&self, from: End, pick: Pick, to: End, keep: Option<Keep>, expect: Option<Stamp>) -> Result<(u64, bool)> {
        let name = pick.name.clone();
        if let Some(id) = self.enqueue_with(from, vec![pick], to.clone(), false, Policy::Replace, (keep, expect)).await?.1 {
            return Ok((id, true));
        }
        let g = self.inner();
        g.recs
            .iter()
            .find(|r| r.item.state.active() && r.item.writes(&to, &name))
            .map(|r| (r.item.id, false))
            .ok_or_else(|| Error::Einval { reason: format!("{name}: not queued") })
    }

    /// The state of an item; None when it is off the list.
    pub fn state(&self, id: u64) -> Option<St> {
        self.inner().recs.iter().find(|r| r.item.id == id).map(|r| r.item.state)
    }

    /// The code of the error an item ended with.
    pub fn err_code(&self, id: u64) -> Option<String> {
        let g = self.inner();
        let r = g.recs.iter().find(|r| r.item.id == id)?;
        r.item.err.as_ref()?["code"].as_str().map(str::to_owned)
    }

    /// An edit upload whose end the editor reports itself: its batch gets no "copied" toast. After a
    /// restart nobody marks it, and the batch toast says it went.
    pub fn quiet(&self, id: u64) {
        self.inner().quiet.insert(id);
    }

    /// Items that ended from now on, with their state.
    pub fn ended(&self) -> broadcast::Receiver<(u64, St)> {
        self.ended.subscribe()
    }

    /// Local folders that unfinished items read from: their files must stay.
    pub fn local_sources(&self) -> HashSet<String> {
        let g = self.inner();
        g.recs.iter().filter(|r| r.item.from.is_local() && r.item.state != St::Done).map(|r| r.item.from.path.clone()).collect()
    }

    /// The batch, and the id of the last item added.
    async fn enqueue_with(&self, from: End, picks: Vec<Pick>, to: End, mv: bool, policy: Policy, edit: (Option<Keep>, Option<Stamp>)) -> Result<(u64, Option<u64>)> {
        let (keep, expect) = edit;
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
            let key = to.key(&p.name);
            let mut clash = None;
            if names.contains(&key) {
                match policy {
                    Policy::Skip => continue,
                    Policy::Both => tname = free_name(&taken, &p.name, p.dir, |n| to.key(n)),
                    Policy::Replace => {}
                }
            } else if taken.contains(&key) {
                // Two picks that are one name here (Windows ignores case): "keep both" numbers the
                // second, otherwise it fails rather than land on the first.
                match policy {
                    Policy::Both => tname = free_name(&taken, &p.name, p.dir, |n| to.key(n)),
                    _ => clash = serde_json::to_value(Error::NameClash { path: p.name.clone() }).ok(),
                }
            }
            // The same target twice at once would write one part from two sides.
            if clash.is_none() && g.recs.iter().any(|r| r.item.state.active() && r.item.writes(&to, &tname)) {
                continue;
            }
            taken.insert(to.key(&tname));
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
                state: if clash.is_some() { St::Err } else { St::Wait },
                err: clash,
                denied: false,
                keep,
                expect,
                name: p.name,
            };
            g.recs.push(Rec::new(item, Resume::default()));
            added += 1;
        }
        if added == 0 {
            return Ok((0, None));
        }
        g.next_batch = batch;
        let last = g.next_id;
        drop(g);
        self.changed();
        self.schedule();
        // A batch of clashes only has ended already.
        self.report();
        Ok((batch, Some(last)))
    }

    /// Names in the target folder as its file system compares them, for "skip" and "keep both".
    async fn names(&self, to: &End) -> Result<HashSet<String>> {
        let list = if to.is_local() {
            let p = to.path.clone();
            tauri::async_runtime::spawn_blocking(move || crate::fs::local::list(&p)).await.map_err(|e| Error::Einval { reason: e.to_string() })??
        } else {
            let s = self.ssh.session_live(&to.src, &to.user)?;
            crate::fs::remote::list(&s, &to.path).await?
        };
        Ok(list.into_iter().map(|e| to.key(&e.name)).collect())
    }

    pub fn pause(&self, id: u64) {
        self.hold(id, false);
    }

    /// Pauses an item; `held` marks a pause by a disconnect, which a new connect undoes.
    fn hold(&self, id: u64, held: bool) {
        let live = self.with(id, |r| {
            if matches!(r.item.state, St::Wait | St::Count | St::Run) {
                r.item.state = St::Pause;
                r.held = held;
                r.live.clone()
            } else {
                // A pause by hand outlasts a reconnect.
                r.held &= held;
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
            r.held = false;
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
        let (row, live, sweep) = {
            let mut g = self.inner();
            let Some(k) = g.recs.iter().position(|r| r.item.id == id) else { return };
            let mut rec = g.recs.remove(k);
            let row = rec.row.take().filter(|_| rec.item.state.active());
            let live = rec.live.clone();
            // A running job keeps reporting its parts here until it stops; `finish` sweeps them then.
            let sweep = live.is_none() && rec.item.state != St::Done && !rec.resume.parts.is_empty();
            if live.is_some() || sweep {
                g.gone.push(rec);
            }
            (row, live, sweep)
        };
        if let Some(l) = &live {
            let _ = l.stop.send(true);
        }
        if sweep {
            self.spawn_sweep(id);
        }
        if let Some(row) = row {
            self.log.done(row, "отменено", Row::Ok);
        }
        let _ = self.ended.send((id, St::Err));
        self.changed();
        self.schedule();
    }

    pub fn retry(&self, id: u64, as_root: bool) {
        self.with(id, |r| {
            if r.item.state == St::Err && !r.item.twin() {
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

    /// A failed edit upload goes again, checked against `expect` now; false when it is not failed.
    pub fn resend(&self, id: u64, expect: Option<Stamp>) -> bool {
        let failed = self.with(id, |r| {
            let failed = r.item.state == St::Err;
            if failed {
                r.item.expect = expect;
            }
            failed
        });
        if failed != Some(true) {
            return false;
        }
        self.retry(id, false);
        true
    }

    /// Takes the ended items off the list; the parts failed ones leave are deleted.
    pub fn clear_done(&self) {
        let swept: Vec<u64> = {
            let mut g = self.inner();
            let (keep, out): (Vec<Rec>, Vec<Rec>) = std::mem::take(&mut g.recs).into_iter().partition(|r| r.item.state.active());
            g.recs = keep;
            let dirty: Vec<Rec> = out.into_iter().filter(|r| r.item.state == St::Err && !r.resume.parts.is_empty()).collect();
            let ids = dirty.iter().map(|r| r.item.id).collect();
            g.gone.extend(dirty);
            ids
        };
        for id in swept {
            self.spawn_sweep(id);
        }
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
    /// A cancel waits a while for the parts to be deleted over the connections it is about to close.
    pub async fn detach(&self, server: &str, user: Option<&str>, cancel: bool) {
        let touches = |e: &End| e.src == server && user.is_none_or(|u| u == e.user);
        let ids: Vec<u64> = self.inner().recs.iter().filter(|r| r.item.state.active() && (touches(&r.item.from) || touches(&r.item.to))).map(|r| r.item.id).collect();
        for &id in &ids {
            if cancel { self.cancel(id) } else { self.hold(id, true) }
        }
        if cancel {
            let swept = async {
                loop {
                    let next = self.settled.notified();
                    tokio::pin!(next);
                    next.as_mut().enable();
                    if !self.inner().gone.iter().any(|r| ids.contains(&r.item.id)) {
                        break;
                    }
                    next.await;
                }
            };
            let _ = tokio::time::timeout(DETACH_WAIT, swept).await;
        }
        self.pool.close(server, user).await;
    }

    /// A server is connected again: what its disconnect paused goes on, once every server the
    /// item needs is connected.
    pub fn attach(&self, server: &str) {
        let held: Vec<(u64, Vec<String>)> = self
            .inner()
            .recs
            .iter()
            .filter(|r| r.held && r.item.state == St::Pause && (r.item.from.src == server || r.item.to.src == server))
            .map(|r| (r.item.id, [&r.item.from, &r.item.to].into_iter().filter(|e| !e.is_local()).map(|e| e.src.clone()).collect()))
            .collect();
        for (id, ends) in held {
            if ends.iter().all(|s| self.ssh.has_session(s)) {
                self.resume(id);
            }
        }
        // An editor save that failed for the lack of a link goes up once the server is back.
        const LINK: [&str; 4] = ["NET", "TIMEOUT", "DROPPED", "DISCONNECTED"];
        let edits: Vec<u64> = self
            .inner()
            .recs
            .iter()
            .filter(|r| r.item.is_edit() && r.item.state == St::Err && r.item.to.src == server)
            .filter(|r| r.item.err.as_ref().and_then(|e| e.get("code")).and_then(|c| c.as_str()).is_some_and(|c| LINK.contains(&c)))
            .map(|r| r.item.id)
            .collect();
        for id in edits {
            self.retry(id, false);
        }
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
            let Inner { recs, gone, parallel, .. } = &mut *g;
            let mut running = recs.iter().filter(|r| r.live.is_some()).count();
            // Targets being written: a retried clash or a resumed twin waits for its turn.
            let mut busy: Vec<(End, String)> = recs.iter().filter(|r| r.live.is_some()).map(|r| (r.item.to.clone(), r.item.tname.clone())).collect();
            let mut out = Vec::new();
            for r in recs.iter_mut() {
                if running >= *parallel {
                    break;
                }
                if r.item.state != St::Wait || r.live.is_some() {
                    continue;
                }
                // A cancelled item still writes this target or its parts are being deleted.
                if gone.iter().any(|x| x.item.writes(&r.item.to, &r.item.tname)) || busy.iter().any(|(to, t)| r.item.writes(to, t)) {
                    continue;
                }
                busy.push((r.item.to.clone(), r.item.tname.clone()));
                let (tx, rx) = watch::channel(false);
                let live = Arc::new(Live { stop: tx, prog: Arc::new(Prog::default()) });
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
        let mut drops = 0;
        let mut mark = live.prog.done.load(Ordering::Relaxed);
        let res = loop {
            let Some((item, resume, keep_time)) = self.with(id, |r| (r.item.clone(), r.resume.clone())).map(|(i, r)| (i, r, self.inner().keep_time)) else {
                break Err(Error::Stopped);
            };
            let res = self.attempt(id, &item, &resume, &live.prog, &stop, keep_time).await;
            if !matches!(res, Err(Error::Disconnected)) {
                break res;
            }
            // A server that lets us in and drops us at the same step again would loop forever.
            let done = live.prog.done.load(Ordering::Relaxed);
            drops = if done > mark { 1 } else { drops + 1 };
            mark = done;
            if drops >= DROPS {
                break res;
            }
            self.log_note(id, "соединение потеряно · жду");
            let mut back = false;
            for pause in RECONNECT {
                let mut s = stop.clone();
                tokio::select! {
                    biased;
                    _ = io::stopped(&mut s) => break,
                    _ = tokio::time::sleep(Duration::from_secs(pause)) => {}
                }
                let mut s = stop.clone();
                let up = tokio::select! {
                    biased;
                    _ = io::stopped(&mut s) => break,
                    up = self.reachable(&item) => up,
                };
                if up {
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
        // Connecting to a silent host takes a while: a pause or a cancel must not wait for it.
        let mut s = stop.clone();
        let (from, to) = tokio::select! {
            biased;
            _ = io::stopped(&mut s) => return Err(Error::Stopped),
            sides = async { Ok::<_, Error>((self.side(&item.from).await?, self.side(&item.to).await?)) } => sides?,
        };
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
        // Work that ended counts even if the connection went at the same moment.
        let res = tokio::select! {
            biased;
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
        let done = live.prog.done.load(Ordering::Relaxed);
        let mut changed: Vec<End> = Vec::new();
        let mut sweep = false;
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
                            // Paused: the part stays for the resume. Resumed while stopping: `Wait`
                            // stays and the item starts again below.
                            if matches!(r.item.state, St::Count | St::Run) {
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
                // Cancelled while running: its parts go now.
                None => {
                    if let Some(r) = g.gone.iter_mut().find(|r| r.item.id == id) {
                        r.live = None;
                        sweep = true;
                    }
                }
            }
        }
        // The publisher may have gone to sleep with nothing running: the end must reach the UI and the disk.
        self.changed();
        if let Some(st) = self.with(id, |r| r.item.state).filter(|s| matches!(s, St::Done | St::Err)) {
            let _ = self.ended.send((id, st));
        }
        if sweep {
            self.sweep(id).await;
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

    /// Count → Run from the job. A pause or a resume meanwhile (Pause, Wait) is the user's and stays.
    fn set_state(&self, id: u64, st: St) {
        self.with(id, |r| {
            if matches!(r.item.state, St::Count | St::Run) {
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

    fn dir_made(&self, id: u64, rel: &str) {
        self.with(id, |r| {
            r.resume.made.insert(rel.to_owned());
        });
    }

    fn files_done(&self, id: u64, n: usize) {
        self.with(id, |r| r.resume.files = n);
    }

    /// Deletes the parts of an item in `gone`, then lets go of it: its target may start again.
    async fn sweep(&self, id: u64) {
        let parts = self.inner().gone.iter().find(|r| r.item.id == id).map(|r| (r.item.to.clone(), r.item.tname.clone(), r.resume.parts.keys().cloned().collect::<Vec<_>>()));
        if let Some((to, tname, parts)) = parts
            && !parts.is_empty()
        {
            // A server that is gone keeps its parts; the queue must not wait on it forever.
            let _ = tokio::time::timeout(CLEAN_WAIT, self.clean(&to, &tname, parts)).await;
        }
        self.inner().gone.retain(|r| r.item.id != id);
        self.settled.notify_waiters();
        self.schedule();
    }

    fn spawn_sweep(&self, id: u64) {
        if let Some(t) = self.me.upgrade() {
            tauri::async_runtime::spawn(async move { t.sweep(id).await });
        }
    }

    async fn clean(&self, to: &End, tname: &str, parts: Vec<String>) {
        let Ok(side) = self.side(to).await else { return };
        let root = side.join(&to.path, tname);
        for rel in parts {
            let target = side.join(&root, &rel);
            let _ = side.remove(&side.part_of(&target)).await;
            side.restore_old(&target).await;
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
            // A finished edit upload has its own toast ("uploaded to") while its copy is open, and one
            // the server changed under has the copy's question; another failure, or an upload that went
            // after a restart, keeps the batch toast.
            let edit = {
                let mut g = self.inner();
                let ids: Vec<u64> = g.recs.iter().filter(|r| r.item.batch == b.batch).map(|r| r.item.id).collect();
                let quiet = ids.iter().any(|id| g.quiet.contains(id));
                // A failed one may be sent again, and stays quiet for that.
                if b.bad == 0 {
                    g.quiet.retain(|id| !ids.contains(id));
                }
                // A file changed on the server: the copy asks whose version wins, which says it all.
                let changed = g
                    .recs
                    .iter()
                    .filter(|r| r.item.batch == b.batch && r.item.state == St::Err)
                    .all(|r| r.item.err.as_ref().and_then(|e| e["code"].as_str()) == Some("SERVER_CHANGED"));
                quiet && changed
            };
            if edit {
                continue;
            }
            let _ = self.app.emit("transfer://batch", b);
        }
    }

    /* ---------- events and the disk ---------- */

    /// Sends what changed in the queue at most every `TICK`, and saves it at most every `SAVE_EVERY`.
    /// Running items go every tick for their speed; the rest only when they change.
    async fn publisher(self: Arc<Self>) {
        let mut speeds: HashMap<u64, (u64, Instant, f64)> = HashMap::new();
        // What the UI has of each item.
        let mut sent: HashMap<u64, Item> = HashMap::new();
        let mut saved_at = Instant::now();
        loop {
            let (running, dirty) = {
                let g = self.inner();
                (g.recs.iter().any(|r| r.live.is_some()), g.dirty)
            };
            if running {
                tokio::select! {
                    _ = self.kick.notified() => {}
                    _ = tokio::time::sleep(TICK) => {}
                }
            } else if dirty {
                tokio::select! {
                    _ = self.kick.notified() => {}
                    _ = tokio::time::sleep_until((saved_at + SAVE_EVERY).into()) => {}
                }
            } else {
                self.kick.notified().await;
            }
            let now = Instant::now();
            let reset = self.resend.swap(false, Ordering::Relaxed);
            if reset {
                sent.clear();
            }
            let (diff, save) = {
                let mut g = self.inner();
                let mut up = Vec::new();
                for r in &g.recs {
                    let live = r.live.is_some();
                    if !live && sent.get(&r.item.id) == Some(&r.item) {
                        continue;
                    }
                    let item = r.now();
                    let mut speed = 0.0;
                    if live {
                        let e = speeds.entry(item.id).or_insert((item.done, now, 0.0));
                        let dt = now.duration_since(e.1).as_secs_f64();
                        if dt >= 0.09 {
                            let rate = item.done.saturating_sub(e.0) as f64 / dt;
                            e.2 = if e.2 == 0.0 { rate } else { e.2 * 0.7 + rate * 0.3 };
                            *e = (item.done, now, e.2);
                        }
                        speed = e.2;
                    }
                    up.push(serde_json::to_value(View { item: &item, speed }).unwrap_or_default());
                    sent.insert(item.id, item);
                }
                let ids: HashSet<u64> = g.recs.iter().map(|r| r.item.id).collect();
                let del: Vec<u64> = sent.keys().filter(|id| !ids.contains(id)).copied().collect();
                for id in &del {
                    sent.remove(id);
                }
                speeds.retain(|id, _| g.recs.iter().any(|r| r.item.id == *id && r.live.is_some()));
                let running = g.recs.iter().any(|r| r.live.is_some());
                let due = (g.dirty || running) && now.duration_since(saved_at) >= SAVE_EVERY;
                let save = due.then(|| self.disk(&mut g));
                ((reset || !up.is_empty() || !del.is_empty()).then_some(Diff { up, del, reset }), save)
            };
            if let Some(diff) = diff {
                let _ = self.app.emit("transfer://state", diff);
            }
            if let Some(disk) = save {
                saved_at = now;
                let store = self.store.clone();
                let _ = tauri::async_runtime::spawn_blocking(move || store.save_transfers(&disk)).await;
            }
            tokio::time::sleep(TICK).await;
        }
    }
}

/// `name (2).ext` for a file, `name (2)` for a folder; the first number that is free. `taken`
/// holds names as `key` makes them.
fn free_name(taken: &HashSet<String>, name: &str, dir: bool, key: impl Fn(&str) -> String) -> String {
    let dot = if dir { None } else { name.rfind('.').filter(|&d| d > 0) };
    let (stem, ext) = match dot {
        Some(d) => (&name[..d], &name[d..]),
        None => (name, ""),
    };
    (2..).map(|n| format!("{stem} ({n}){ext}")).find(|c| !taken.contains(&key(c))).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_names() {
        let same = |n: &str| n.to_owned();
        let taken: HashSet<String> = ["a.txt", "a (2).txt", "dir.v1"].iter().map(|s| s.to_string()).collect();
        assert_eq!(free_name(&taken, "a.txt", false, same), "a (3).txt");
        assert_eq!(free_name(&taken, "dir.v1", true, same), "dir.v1 (2)");
        assert_eq!(free_name(&taken, ".bashrc", false, same), ".bashrc (2)");
        // A Windows folder: "A (3).TXT" is taken by "a (3).txt".
        let fold = |n: &str| n.to_lowercase();
        let taken: HashSet<String> = ["a.txt", "a (2).txt", "a (3).txt"].iter().map(|s| s.to_string()).collect();
        assert_eq!(free_name(&taken, "A.TXT", false, fold), "A (4).TXT");
    }

    #[test]
    fn windows_targets_ignore_case() {
        let end = |src: &str, path: &str| End { src: src.into(), user: String::new(), path: path.into() };
        let item = Item {
            id: 1,
            batch: 1,
            name: "Readme.md".into(),
            tname: "Readme.md".into(),
            dir: false,
            size: 1,
            done: 0,
            from: end("s1", "/srv"),
            to: end("local", "C:\\X"),
            mv: false,
            state: St::Wait,
            err: None,
            denied: false,
            keep: None,
            expect: None,
        };
        assert_eq!(item.writes(&end("local", "c:\\x"), "README.md"), cfg!(windows));
        assert!(!item.writes(&end("s1", "C:\\X"), "Readme.md"));
        let remote = Item { to: end("s1", "/srv"), ..item };
        assert!(!remote.writes(&end("s1", "/srv"), "README.md"));
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
            keep: Some(Keep { mode: 0o600, uid: 0, gid: 0 }),
            expect: Some((12, 1_700_000_000)),
        };
        let mut resume = Resume { files: 3, ..Default::default() };
        resume.parts.insert("b/c".into(), (100, 1_700_000_000));
        let text = serde_json::to_string(&Disk { items: vec![Saved { item, resume: resume.clone() }], next_id: 1, next_batch: 1 }).unwrap();
        assert!(text.contains("\"move\":false"));
        let back: Disk = serde_json::from_str(&text).unwrap();
        assert_eq!(back.items[0].resume, resume);
        assert_eq!(back.items[0].item.state, St::Run);
        assert_eq!(back.items[0].item.expect, Some((12, 1_700_000_000)));
    }
}
