//! Kite: SSH and SFTP in one window.
//!
//! The shell (one window, plugins, WebView hardening), the file store, SSH sessions with their
//! host keys, keys and secrets, and read-only server folders. The rest still runs in the UI's
//! `MockBackend`; the core modules arrive stage by stage.

mod error;
mod fs;
mod ipc;
mod log;
mod secrets;
mod ssh;
mod store;
mod webview;

use std::sync::Arc;

use tauri::Manager;
use tauri_plugin_window_state::StateFlags;

pub fn run() {
    // Kept alive for the whole `run`; Tauri commands and SSH tasks spawn on this handle.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(ssh::ASYNC_WORKERS)
        .enable_all()
        .build()
        .expect("failed to build the async runtime");
    tauri::async_runtime::set(runtime.handle().clone());

    tauri::Builder::default()
        // Must be the first plugin: a second launch raises the first window,
        // otherwise two processes would overwrite the session file.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        // Size, position and maximized state; the UI shows the window itself once it is painted.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::all() & !StateFlags::VISIBLE)
                .build(),
        )
        // Used from Rust only (the key file picker); the UI has no dialog permissions.
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            app.manage(Arc::new(store::Store::new(dir)?));
            let log = Arc::new(log::Log::new(Some(app.handle().clone())));
            app.manage(log.clone());
            app.manage(ssh::Ssh::new(app.handle().clone(), log));
            if let Some(w) = app.get_webview_window("main") {
                webview::harden(&w);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::state_load,
            ipc::state_save,
            ipc::servers_load,
            ipc::servers_save,
            ipc::term_load,
            ipc::term_save,
            ipc::term_prune,
            ipc::ssh::ssh_connect,
            ipc::ssh::ssh_disconnect,
            ipc::ssh::ssh_test,
            ipc::ssh::ssh_first_login,
            ipc::ssh::hostkey_respond,
            ipc::ssh::passphrase_respond,
            ipc::ssh::hostkey_known,
            ipc::ssh::hostkey_forget,
            ipc::ssh::set_strict_host,
            ipc::ssh::ssh_sessions,
            ipc::ssh::log_tail,
            ipc::ssh::secret_set,
            ipc::ssh::secret_delete,
            ipc::ssh::secrets_available,
            ipc::ssh::agent_status,
            ipc::ssh::keys_list,
            ipc::ssh::pick_key_file,
            ipc::ssh::fs_home,
            ipc::ssh::fs_resolve,
            ipc::ssh::fs_list,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kite");
}
