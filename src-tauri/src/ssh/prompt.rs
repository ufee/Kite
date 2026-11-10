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
    pub fp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
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

#[derive(Default)]
pub struct Prompts {
    next: AtomicU64,
    host: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    pass: Mutex<HashMap<u64, mpsc::Sender<PassphraseAnswer>>>,
}

impl Prompts {
    fn id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Requests issued so far: a change across a login means the user was asked something.
    pub fn issued(&self) -> u64 {
        self.next.load(Ordering::Relaxed)
    }

    /// Waits for the user. A dropped request (window reloaded, app closing) counts as "no".
    pub async fn host_key(&self, app: &AppHandle, mut req: HostKeyRequest) -> bool {
        req.req_id = self.id();
        let (tx, rx) = oneshot::channel();
        self.host.lock().unwrap_or_else(|e| e.into_inner()).insert(req.req_id, tx);
        if app.emit("ssh://hostkey", &req).is_err() {
            self.host.lock().unwrap_or_else(|e| e.into_inner()).remove(&req.req_id);
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub fn host_key_respond(&self, req_id: u64, trust: bool) {
        if let Some(tx) = self.host.lock().unwrap_or_else(|e| e.into_inner()).remove(&req_id) {
            let _ = tx.send(trust);
        }
    }

    /// Opens the dialog; answers arrive on the receiver until one is accepted or cancelled.
    pub fn passphrase(&self, app: &AppHandle, mut req: PassphraseRequest) -> Option<(u64, mpsc::Receiver<PassphraseAnswer>)> {
        req.req_id = self.id();
        let (tx, rx) = mpsc::channel(1);
        self.pass.lock().unwrap_or_else(|e| e.into_inner()).insert(req.req_id, tx);
        if app.emit("ssh://passphrase", &req).is_err() {
            self.passphrase_done(req.req_id);
            return None;
        }
        Some((req.req_id, rx))
    }

    pub fn passphrase_done(&self, req_id: u64) {
        self.pass.lock().unwrap_or_else(|e| e.into_inner()).remove(&req_id);
    }

    pub async fn passphrase_respond(&self, req_id: u64, phrase: Option<Zeroizing<String>>, add_to_agent: bool) -> Result<()> {
        let tx = self.pass.lock().unwrap_or_else(|e| e.into_inner()).get(&req_id).cloned();
        let Some(tx) = tx else { return Err(Error::Cancelled) };
        let (reply, rx) = oneshot::channel();
        tx.send(PassphraseAnswer { phrase, add_to_agent, reply }).await.map_err(|_| Error::Cancelled)?;
        rx.await.unwrap_or(Err(Error::Cancelled))
    }
}
