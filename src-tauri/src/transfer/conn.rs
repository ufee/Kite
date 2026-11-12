//! Transfer connections: one per server and user, apart from the one of the panels and terminals.
//! Opened by the first transfer of the pair, closed when it has been idle for a while.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use tokio::sync::{Semaphore, watch};

use crate::{
    error::{Error, Result},
    ssh::{Session, Ssh},
};

/// Bytes per SFTP read or write. The servers' 255 KiB limit is slower here: one reply spans
/// several SSH packets and stalls the session task (spike: 140 against 195 MB/s on 1 GB).
pub const CHUNK: u32 = 64 * 1024;
const IDLE: Duration = Duration::from_secs(30);
/// Data requests in flight on one connection, all files and items together. russh-sftp starts a
/// request's timeout when it is queued, not sent: with no cap, 32 files × 64 requests on a slow link
/// would time out before their turn on the wire. 128 × 64 KiB = 8 MiB, so the floor is ~70 KB/s.
pub const BUDGET: usize = 128;

pub struct Conn {
    pub session: Arc<Session>,
    /// Read and write size, the server's limit if it is lower than ours.
    pub chunk: u32,
    /// `posix-rename@openssh.com` works here: announced at init, turns false on a refusal.
    pub posix_rename: AtomicBool,
    /// One permit per data request in flight.
    pub budget: Arc<Semaphore>,
    /// Read and write pipelines running here: they split the budget evenly.
    pipes: AtomicUsize,
    /// Turns true when the connection is gone.
    dropped: watch::Receiver<bool>,
}

impl Conn {
    async fn open(ssh: Arc<Ssh>, server: String, user: String) -> Result<Arc<Conn>> {
        let (session, dropped) = ssh.open_xfer(&server, &user).await?;
        let mut chunk = CHUNK;
        if let Ok(l) = session.sftp.limits().await {
            for limit in [l.max_read_len, l.max_write_len] {
                if limit > 0 {
                    chunk = chunk.min(limit as u32);
                }
            }
        }
        let posix_rename = AtomicBool::new(session.posix_rename);
        Ok(Arc::new(Conn { session, chunk, posix_rename, budget: Arc::new(Semaphore::new(BUDGET)), pipes: AtomicUsize::new(0), dropped }))
    }

    /// Counts a pipeline in until the guard is dropped.
    pub fn pipe(&self) -> Pipe<'_> {
        self.pipes.fetch_add(1, Ordering::Relaxed);
        Pipe(self)
    }

    /// Requests one pipeline may keep in flight: an even part of the budget. Without it the first
    /// pipelines keep 64 each and a late one lives on the single slot it waited for.
    pub fn share(&self, most: usize) -> usize {
        (BUDGET / self.pipes.load(Ordering::Relaxed).max(1)).clamp(2, most)
    }

    /// Resolves once the connection is gone. A writer waiting for the channel window never hears
    /// about it, so a job races this to see a cut at once rather than at the next keepalive.
    pub async fn lost(&self) {
        let mut d = self.dropped.clone();
        let _ = d.wait_for(|gone| *gone).await;
    }

    pub fn is_closed(&self) -> bool {
        self.session.is_closed()
    }
}

pub struct Pipe<'a>(&'a Conn);

impl Drop for Pipe<'_> {
    fn drop(&mut self) {
        self.0.pipes.fetch_sub(1, Ordering::Relaxed);
    }
}

type Pair = (String, String);
type Pending = Shared<BoxFuture<'static, Result<Arc<Conn>>>>;

enum Slot {
    Pending(u64, Pending),
    Live(u64, Arc<Conn>),
}

#[derive(Default)]
struct Pairs {
    slots: HashMap<Pair, Slot>,
    /// Leases out per pair: the idle timer closes a connection only at zero.
    users: HashMap<Pair, usize>,
}

pub struct Pool {
    ssh: Arc<Ssh>,
    me: Weak<Pool>,
    pairs: Mutex<Pairs>,
    epoch: AtomicU64,
}

/// A connection in use by one transfer. Dropping the last lease of a pair starts the idle timer.
pub struct Lease {
    pool: Weak<Pool>,
    pair: Pair,
    pub conn: Arc<Conn>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade() {
            pool.release(&self.pair);
        }
    }
}

/// A use counted for a pair while `get` waits: given back if `get` is dropped (a pause meanwhile)
/// or fails, handed to the lease when it succeeds.
struct Claim<'a> {
    pool: &'a Pool,
    pair: Option<Pair>,
}

impl Claim<'_> {
    fn lease(mut self, conn: Arc<Conn>) -> Lease {
        let pair = self.pair.take().unwrap_or_default();
        Lease { pool: self.pool.me.clone(), pair, conn }
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if let Some(pair) = self.pair.take() {
            self.pool.release(&pair);
        }
    }
}

impl Pool {
    pub fn new(ssh: Arc<Ssh>) -> Arc<Self> {
        Arc::new_cyclic(|me| Pool { ssh, me: me.clone(), pairs: Mutex::new(Pairs::default()), epoch: AtomicU64::new(0) })
    }

    fn pairs(&self) -> std::sync::MutexGuard<'_, Pairs> {
        self.pairs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The transfer connection of the pair; callers that come while it connects share the result.
    /// Safe to drop while it waits: the connect goes on for the others, the use is given back.
    pub async fn get(&self, server: &str, user: &str) -> Result<Lease> {
        let pair: Pair = (server.to_owned(), user.to_owned());
        let (epoch, fut) = {
            let mut p = self.pairs();
            *p.users.entry(pair.clone()).or_default() += 1;
            match p.slots.get(&pair) {
                Some(Slot::Live(_, c)) if !c.is_closed() => {
                    return Ok(Lease { pool: self.me.clone(), pair, conn: c.clone() });
                }
                Some(Slot::Pending(e, f)) => (*e, f.clone()),
                _ => {
                    let epoch = self.epoch.fetch_add(1, Ordering::Relaxed);
                    let fut = Conn::open(self.ssh.clone(), pair.0.clone(), pair.1.clone()).boxed().shared();
                    p.slots.insert(pair.clone(), Slot::Pending(epoch, fut.clone()));
                    (epoch, fut)
                }
            }
        };
        let claim = Claim { pool: self, pair: Some(pair.clone()) };
        let res = fut.await;
        let mut stale = None;
        {
            let mut p = self.pairs();
            let ours = matches!(p.slots.get(&pair), Some(Slot::Pending(e, _)) if *e == epoch);
            match &res {
                Ok(c) if ours => {
                    p.slots.insert(pair.clone(), Slot::Live(epoch, c.clone()));
                }
                Ok(c) => {
                    // Closed for the server meanwhile, or another caller already put it in place.
                    if !matches!(p.slots.get(&pair), Some(Slot::Live(e, _)) if *e == epoch) {
                        stale = Some(c.clone());
                    }
                }
                Err(_) if ours => {
                    p.slots.remove(&pair);
                }
                Err(_) => {}
            }
        }
        match (res, stale) {
            // Closed for the server while connecting (a disconnect): the job's retry decides.
            (Ok(_), Some(c)) => {
                drop(claim);
                c.session.close().await;
                Err(Error::Disconnected)
            }
            (Ok(conn), None) => Ok(claim.lease(conn)),
            (Err(e), _) => Err(e),
        }
    }

    fn release(&self, pair: &Pair) {
        let idle = {
            let mut p = self.pairs();
            let n = p.users.get_mut(pair).map(|n| {
                *n = n.saturating_sub(1);
                *n
            });
            if n == Some(0) {
                p.users.remove(pair);
            }
            match p.slots.get(pair) {
                Some(Slot::Live(e, _)) if n == Some(0) => Some(*e),
                // Nobody waits for this connect any more (a pause): dropping it frees its handshake slot.
                Some(Slot::Pending(..)) if n == Some(0) => {
                    p.slots.remove(pair);
                    None
                }
                _ => None,
            }
        };
        let Some(epoch) = idle else { return };
        let me = self.me.clone();
        let pair = pair.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(IDLE).await;
            let Some(pool) = me.upgrade() else { return };
            let gone = {
                let mut p = pool.pairs();
                let idle = !p.users.contains_key(&pair) && matches!(p.slots.get(&pair), Some(Slot::Live(e, _)) if *e == epoch);
                if idle { p.slots.remove(&pair) } else { None }
            };
            if let Some(Slot::Live(_, c)) = gone {
                c.session.close().await;
            }
        });
    }

    /// A transfer saw the connection die: the next `get` opens a new one.
    pub async fn lost(&self, conn: &Arc<Conn>) {
        let gone = {
            let mut p = self.pairs();
            let key = p.slots.iter().find(|(_, s)| matches!(s, Slot::Live(_, c) if Arc::ptr_eq(c, conn))).map(|(k, _)| k.clone());
            key.and_then(|k| p.slots.remove(&k))
        };
        if gone.is_some() || !conn.is_closed() {
            conn.session.sftp_lost().await;
        }
    }

    /// Closes the transfer connections of a server (one user or all).
    pub async fn close(&self, server: &str, user: Option<&str>) {
        let gone: Vec<Slot> = {
            let mut p = self.pairs();
            let keys: Vec<Pair> = p.slots.keys().filter(|(s, u)| s == server && user.is_none_or(|x| x == u)).cloned().collect();
            keys.into_iter().filter_map(|k| p.slots.remove(&k)).collect()
        };
        for slot in gone {
            if let Slot::Live(_, c) = slot {
                c.session.close().await;
            }
        }
    }
}
