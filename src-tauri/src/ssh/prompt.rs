//! Questions the core asks the user mid-connection: trust a host key, unlock a key.
//! Each request is an event with an id; the answer comes back through a command with that id.
//! The UI queues the dialogs, so two servers asking at once never cancel each other.

use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyRequest {
    pub req_id: u64,
    /// `tofu` for a new host, `changed` when another key is on record.
    pub mode: &'static str,
    pub server: String,
    pub code: String,
    pub host: String,
    pub port: u16,
    /// `ED25519`, `ECDSA`, `RSA`: what to compare with on the server.
    pub algo: String,
    pub fp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    /// A new host whose address has a key of another type on record.
    pub others: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PassphraseRequest {
    pub req_id: u64,
    pub server: String,
    pub code: String,
    pub user: String,
    /// Key file as the user knows it (`%USERPROFILE%\.ssh\work`); the title shows its name.
    pub file: String,
    /// The agent is running: the "add to agent" box is available.
    pub agent: bool,
}

/// One answer from the passphrase dialog; `reply` tells the dialog whether to close.
pub struct PassphraseAnswer {
    pub phrase: Option<Zeroizing<String>>,
    pub add_to_agent: bool,
    pub reply: oneshot::Sender<Result<()>>,
}

/// Open requests by id, with the server they are about.
type Open<T> = Mutex<HashMap<u64, (String, T)>>;

#[derive(Default)]
pub struct Prompts {
    next: AtomicU64,
    host: Open<oneshot::Sender<bool>>,
    pass: Open<mpsc::Sender<PassphraseAnswer>>,
}

fn open<T>(m: &Open<T>) -> std::sync::MutexGuard<'_, HashMap<u64, (String, T)>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// An open request that is withdrawn, its dialog closed, when its connect goes away unanswered
/// (a transfer paused mid-login drops it).
pub struct Ask<'a, T> {
    m: &'a Open<T>,
    app: &'a AppHandle,
    id: u64,
}

impl<T> Ask<'_, T> {
    /// Answered: the dialog has closed itself.
    pub fn done(self) {
        open(self.m).remove(&self.id);
    }
}

impl<T> Drop for Ask<'_, T> {
    fn drop(&mut self) {
        if open(self.m).remove(&self.id).is_some() {
            let _ = self.app.emit("ssh://prompt-gone", [self.id]);
        }
    }
}

impl Prompts {
    fn id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Waits for the user. A dropped request (window reloaded, server disconnected) counts as "no".
    pub async fn host_key(&self, app: &AppHandle, mut req: HostKeyRequest) -> bool {
        req.req_id = self.id();
        let (tx, rx) = oneshot::channel();
        open(&self.host).insert(req.req_id, (req.server.clone(), tx));
        let _ask = Ask { m: &self.host, app, id: req.req_id };
        if app.emit("ssh://hostkey", &req).is_err() {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub fn host_key_respond(&self, req_id: u64, trust: bool) {
        if let Some((_, tx)) = open(&self.host).remove(&req_id) {
            let _ = tx.send(trust);
        }
    }

    /// Opens the dialog; answers arrive on the receiver until one is accepted or cancelled.
    #[allow(clippy::type_complexity)]
    pub fn passphrase<'a>(&'a self, app: &'a AppHandle, mut req: PassphraseRequest) -> Option<(Ask<'a, mpsc::Sender<PassphraseAnswer>>, mpsc::Receiver<PassphraseAnswer>)> {
        req.req_id = self.id();
        let (tx, rx) = mpsc::channel(1);
        open(&self.pass).insert(req.req_id, (req.server.clone(), tx));
        let ask = Ask { m: &self.pass, app, id: req.req_id };
        if app.emit("ssh://passphrase", &req).is_err() {
            ask.done();
            return None;
        }
        Some((ask, rx))
    }

    /// Drops the open requests (of one server, or all when the window was reloaded and its dialogs
    /// are gone): the waiting connections see "no" and finish. The UI closes what it still shows.
    pub fn cancel(&self, app: &AppHandle, server: Option<&str>) {
        fn take<T>(m: &Open<T>, server: Option<&str>, gone: &mut Vec<u64>) {
            let mut map = open(m);
            let ids: Vec<u64> = map.iter().filter(|(_, (s, _))| server.is_none_or(|x| x == s)).map(|(id, _)| *id).collect();
            for id in ids {
                map.remove(&id);
                gone.push(id);
            }
        }
        let mut gone = Vec::new();
        take(&self.host, server, &mut gone);
        take(&self.pass, server, &mut gone);
        if !gone.is_empty() {
            let _ = app.emit("ssh://prompt-gone", &gone);
        }
    }

    pub async fn passphrase_respond(&self, req_id: u64, phrase: Option<Zeroizing<String>>, add_to_agent: bool) -> Result<()> {
        let tx = open(&self.pass).get(&req_id).map(|(_, tx)| tx.clone());
        let Some(tx) = tx else { return Err(Error::Cancelled) };
        let (reply, rx) = oneshot::channel();
        tx.send(PassphraseAnswer { phrase, add_to_agent, reply }).await.map_err(|_| Error::Cancelled)?;
        rx.await.unwrap_or(Err(Error::Cancelled))
    }
}
