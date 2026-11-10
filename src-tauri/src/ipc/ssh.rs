//! SSH, secrets, keys, the log and server folders. Errors go to the UI as `{ code, ...facts }`.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use zeroize::Zeroizing;

use crate::{
    error::{Error, Result},
    fs::{self, Entry},
    log::{Log, Row},
    secrets,
    ssh::{Draft, SessionState, Ssh, TestReport, keys},
    store::Server,
};

type SshState<'a> = State<'a, Arc<Ssh>>;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(Error::ssh)?
}

#[tauri::command]
pub async fn ssh_connect(ssh: SshState<'_>, server: Server, user: String) -> Result<()> {
    ssh.connect(&server, &user).await.map(|_| ())
}

#[tauri::command]
pub async fn ssh_disconnect(ssh: SshState<'_>, server: String, user: Option<String>) -> Result<()> {
    ssh.disconnect(&server, user.as_deref()).await;
    Ok(())
}

#[tauri::command]
pub async fn ssh_test(ssh: SshState<'_>, draft: Draft) -> Result<TestReport> {
    ssh.inner().clone().test(&draft).await
}

#[tauri::command]
pub async fn ssh_first_login(ssh: SshState<'_>, draft: Draft, install_key: bool) -> Result<()> {
    ssh.inner().clone().first_login(&draft, install_key).await
}

#[tauri::command]
pub fn hostkey_respond(ssh: SshState<'_>, req_id: u64, trust: bool) {
    ssh.prompts.host_key_respond(req_id, trust);
}

#[tauri::command]
pub async fn passphrase_respond(ssh: SshState<'_>, req_id: u64, phrase: Option<Zeroizing<String>>, add_to_agent: bool) -> Result<()> {
    ssh.prompts.passphrase_respond(req_id, phrase, add_to_agent).await
}

/// The window was (re)loaded: dialogs of the previous page are gone, nobody will answer them.
#[tauri::command]
pub fn prompts_reset(app: AppHandle, ssh: SshState<'_>) {
    ssh.prompts.cancel(&app, None);
}

#[tauri::command]
pub async fn hostkey_known(ssh: SshState<'_>, host: String, port: u16) -> Result<Option<String>> {
    Ok(ssh.hostkey_known(&host, port).await)
}

#[tauri::command]
pub async fn hostkey_forget(ssh: SshState<'_>, host: String, port: u16) -> Result<()> {
    ssh.hostkey_forget(&host, port).await
}

#[tauri::command]
pub fn set_strict_host(ssh: SshState<'_>, on: bool) {
    ssh.set_strict(on);
}

#[derive(Serialize)]
pub struct SessionRow {
    server: String,
    user: String,
    state: SessionState,
}

#[tauri::command]
pub fn ssh_sessions(ssh: SshState<'_>) -> Vec<SessionRow> {
    ssh.sessions().into_iter().map(|(server, user, state)| SessionRow { server, user, state }).collect()
}

#[tauri::command]
pub fn log_tail(log: State<'_, Arc<Log>>) -> Vec<Row> {
    log.tail()
}

/* ---------- Secrets and keys ---------- */

#[tauri::command]
pub async fn secret_set(server: String, user: String, password: Zeroizing<String>) -> Result<()> {
    blocking(move || secrets::set(&server, &user, &password)).await
}

#[tauri::command]
pub async fn secret_delete(server: String, user: String) -> Result<()> {
    blocking(move || secrets::delete(&server, &user)).await
}

#[tauri::command]
pub async fn secrets_available() -> bool {
    // On Linux the first call connects to the Secret Service over D-Bus.
    blocking(|| Ok(secrets::available())).await.unwrap_or(false)
}

#[derive(Serialize)]
pub struct AgentStatus {
    running: bool,
    keys: Vec<String>,
}

#[tauri::command]
pub async fn agent_status() -> AgentStatus {
    match keys::agent().await {
        Some(mut agent) => AgentStatus { running: true, keys: keys::agent_keys(&mut agent).await.unwrap_or_default().iter().map(keys::label).collect() },
        None => AgentStatus { running: false, keys: Vec::new() },
    }
}

#[tauri::command]
pub async fn keys_list() -> Result<Vec<String>> {
    blocking(|| Ok(keys::list())).await
}

#[tauri::command]
pub async fn pick_key_file(app: AppHandle) -> Result<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().set_directory(keys::ssh_dir()).pick_file(move |f| {
        let _ = tx.send(f.and_then(|f| f.into_path().ok()));
    });
    Ok(rx.await.ok().flatten().map(|p| keys::contract(&p)))
}

/* ---------- Server folders ---------- */

#[tauri::command]
pub async fn fs_home(ssh: SshState<'_>, server: String, user: String) -> Result<String> {
    Ok(ssh.session_live(&server, &user)?.home.clone())
}

#[tauri::command]
pub async fn fs_resolve(ssh: SshState<'_>, server: String, user: String, raw: String, base: Option<String>) -> Result<String> {
    let s = ssh.session_live(&server, &user)?;
    fs::resolve(&s, &raw, base.as_deref()).await
}

#[tauri::command]
pub async fn fs_list(ssh: SshState<'_>, server: String, user: String, path: String) -> Result<Vec<Entry>> {
    let s = ssh.session_live(&server, &user)?;
    fs::list(&s, &path).await
}
