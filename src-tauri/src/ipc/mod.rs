//! Commands the UI calls. Thin: they move the work off the async runtime and map errors to text.

pub mod edit;
pub mod fs;
pub mod ssh;
pub mod term;
pub mod transfer;

use std::sync::Arc;

use serde_json::Value;
use tauri::State;

use crate::store::{Doc, ServerConfig, Store, import};

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

/// A server list from a file, a drop or the clipboard, checked; the UI diffs it with its own.
#[tauri::command]
pub fn import_read(text: String) -> Result<Vec<import::Parsed>, import::ImportError> {
    import::parse(&text)
}

/// The export goes where the user picks; None when the dialog was closed.
#[tauri::command]
pub async fn export_write(app: tauri::AppHandle, json: String) -> Res<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().set_file_name("servers.json").add_filter("JSON", &["json"]).save_file(move |f| {
        let _ = tx.send(f.and_then(|f| f.into_path().ok()));
    });
    let Some(path) = rx.await.ok().flatten() else { return Ok(None) };
    let shown = path.display().to_string();
    blocking(move || crate::store::write_atomic(&path, json.as_bytes())).await?;
    Ok(Some(shown))
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
