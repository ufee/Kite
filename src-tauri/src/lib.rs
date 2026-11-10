//! Kite: SSH and SFTP in one window.
//!
//! The shell (one window, plugins, WebView hardening) and the file store.
//! Everything else still runs in the UI's `MockBackend`; the core modules arrive stage by stage.

mod ipc;
mod store;
mod webview;

use std::sync::Arc;

use tauri::Manager;
use tauri_plugin_window_state::StateFlags;

pub fn run() {
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
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            app.manage(Arc::new(store::Store::new(dir)?));
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kite");
}
