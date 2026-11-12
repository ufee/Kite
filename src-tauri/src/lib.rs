//! Kite: SSH and SFTP in one window.
//!
//! The shell (one window, plugins, WebView hardening), the file store, SSH sessions with their
//! host keys, keys and secrets, local and server files. The rest still runs in the UI's
//! `MockBackend`; the core modules arrive stage by stage.

mod edit;
mod error;
mod fs;
mod ipc;
mod log;
mod secrets;
mod ssh;
mod store;
mod term;
mod transfer;
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
            let ssh = ssh::Ssh::new(app.handle().clone(), log.clone());
            app.manage(ssh.clone());
            app.manage(Arc::new(term::Terms::default()));
            let store = app.state::<Arc<store::Store>>().inner().clone();
            let q = transfer::Transfers::new(app.handle().clone(), ssh.clone(), log.clone(), store);
            app.manage(q.clone());
            let copies = app.path().app_local_data_dir()?.join("edit");
            app.manage(edit::Edits::new(app.handle().clone(), ssh, q, log, copies));
            if let Some(w) = app.get_webview_window("main") {
                webview::harden(&w);
                webview::explorer_drop(&w);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::state_load,
            ipc::state_save,
            ipc::servers_load,
            ipc::servers_save,
            ipc::import_read,
            ipc::export_write,
            ipc::term_load,
            ipc::term_save,
            ipc::term_prune,
            ipc::ssh::ssh_connect,
            ipc::ssh::ssh_disconnect,
            ipc::ssh::ssh_test,
            ipc::ssh::ssh_first_login,
            ipc::ssh::hostkey_respond,
            ipc::ssh::passphrase_respond,
            ipc::ssh::prompts_reset,
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
            ipc::fs::fs_home,
            ipc::fs::fs_resolve,
            ipc::fs::fs_list,
            ipc::fs::fs_mkdir,
            ipc::fs::fs_rename,
            ipc::fs::fs_delete,
            ipc::fs::fs_trash,
            ipc::fs::fs_chmod,
            ipc::fs::fs_read_text,
            ipc::fs::fs_drives,
            ipc::term::pty_open,
            ipc::term::pty_write,
            ipc::term::pty_resize,
            ipc::term::pty_pause,
            ipc::term::pty_close,
            ipc::transfer::transfer_enqueue,
            ipc::transfer::transfer_list,
            ipc::transfer::transfer_resync,
            ipc::transfer::transfer_pause,
            ipc::transfer::transfer_resume,
            ipc::transfer::transfer_cancel,
            ipc::transfer::transfer_pause_all,
            ipc::transfer::transfer_retry,
            ipc::transfer::transfer_clear_done,
            ipc::transfer::transfer_set_parallel,
            ipc::transfer::transfer_keep_time,
            ipc::transfer::transfer_detach,
            ipc::edit::edit_open,
            ipc::edit::edit_upload,
            ipc::edit::edit_reload,
            ipc::edit::edit_editors,
            ipc::edit::edit_unsent,
            ipc::edit::edit_autosave,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Kite")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // The queue is saved at most every 2 s: what changed since must not be lost on closing.
                if let Some(q) = app.try_state::<Arc<transfer::Transfers>>() {
                    q.save_now();
                }
                // Edit copies go, except those an unfinished upload still reads.
                if let Some(e) = app.try_state::<Arc<edit::Edits>>() {
                    e.close();
                }
            }
        });
}
