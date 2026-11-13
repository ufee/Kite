//! The transfer queue for the UI; state comes back as `transfer://state` and `transfer://batch`.

use std::sync::Arc;

use serde_json::Value;
use tauri::State;

use crate::{
    error::Result,
    transfer::{End, Pick, Policy, Transfers},
};

type Queue<'a> = State<'a, Arc<Transfers>>;

#[tauri::command]
pub async fn transfer_enqueue(q: Queue<'_>, from: End, picks: Vec<Pick>, to: End, mv: bool, policy: Policy) -> Result<u64> {
    q.enqueue(from, picks, to, mv, policy).await
}

#[tauri::command]
pub fn transfer_list(q: Queue<'_>) -> Vec<Value> {
    q.list()
}

/// The UI has put the list in place; the next `transfer://state` carries every item.
#[tauri::command]
pub fn transfer_resync(q: Queue<'_>) {
    q.resync();
}

#[tauri::command]
pub fn transfer_pause(q: Queue<'_>, id: u64) {
    q.pause(id);
}

#[tauri::command]
pub fn transfer_resume(q: Queue<'_>, id: u64) {
    q.resume(id);
}

#[tauri::command]
pub fn transfer_cancel(q: Queue<'_>, id: u64) {
    q.cancel(id);
}

#[tauri::command]
pub fn transfer_pause_all(q: Queue<'_>, pause: bool) {
    q.pause_all(pause);
}

#[tauri::command]
pub fn transfer_retry(q: Queue<'_>, id: u64, as_root: bool) {
    q.retry(id, as_root);
}

#[tauri::command]
pub fn transfer_clear_done(q: Queue<'_>) {
    q.clear_done();
}

#[tauri::command]
pub fn transfer_set_parallel(q: Queue<'_>, n: usize) {
    q.set_parallel(n);
}

#[tauri::command]
pub fn transfer_keep_time(q: Queue<'_>, keep: bool) {
    q.set_keep_time(keep);
}

/// "Copy directly between servers".
#[tauri::command]
pub fn transfer_direct(q: Queue<'_>, on: bool) {
    q.set_direct(on);
}

/// After a failed direct run: these items go through Kite.
#[tauri::command]
pub fn transfer_relay(q: Queue<'_>, ids: Vec<u64>) {
    q.relay(&ids);
}

/// Before a server is disconnected or deleted: its transfers pause or go.
#[tauri::command]
pub async fn transfer_detach(q: Queue<'_>, server: String, user: Option<String>, cancel: bool) -> Result<()> {
    q.detach(&server, user.as_deref(), cancel).await;
    Ok(())
}
