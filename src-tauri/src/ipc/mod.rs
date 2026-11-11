//! Commands the UI calls. Thin: they move the work off the async runtime and map errors to text.

pub mod fs;
pub mod ssh;

use std::sync::Arc;

use serde_json::Value;
use tauri::State;

use crate::store::{Doc, ServerConfig, Store};

type Res<T> = Result<T, String>;

/// File work runs on the blocking pool: a slow disk must not stall the IPC runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> std::io::Result<T> + Send + 'static) -> Res<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn state_load(store: State<'_, Arc<Store>>, doc: Doc) -> Res<Option<Value>> {
    let store = store.inner().clone();
    blocking(move || Ok(store.load_doc(doc))).await
}

#[tauri::command]
pub async fn state_save(store: State<'_, Arc<Store>>, doc: Doc, data: Value) -> Res<()> {
    let store = store.inner().clone();
    blocking(move || store.save_doc(doc, &data)).await
}

#[tauri::command]
pub async fn servers_load(store: State<'_, Arc<Store>>) -> Res<Option<ServerConfig>> {
    let store = store.inner().clone();
    blocking(move || Ok(store.load_servers())).await
}

#[tauri::command]
pub async fn servers_save(store: State<'_, Arc<Store>>, cfg: ServerConfig) -> Res<()> {
    let store = store.inner().clone();
    blocking(move || store.save_servers(&cfg)).await
}

#[tauri::command]
pub async fn term_load(store: State<'_, Arc<Store>>, id: u32) -> Res<Option<String>> {
    let store = store.inner().clone();
    blocking(move || Ok(store.load_term(id))).await
}

#[tauri::command]
pub async fn term_save(store: State<'_, Arc<Store>>, id: u32, data: String) -> Res<()> {
    let store = store.inner().clone();
    blocking(move || store.save_term(id, &data)).await
}

#[tauri::command]
pub async fn term_prune(store: State<'_, Arc<Store>>, keep: Vec<u32>) -> Res<()> {
    let store = store.inner().clone();
    blocking(move || store.prune_terms(&keep)).await
}
