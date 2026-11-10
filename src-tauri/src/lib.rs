//! Kite: SSH and SFTP in one window.
//!
//! Stage 1 is the shell only: one window, plugins and WebView hardening.
//! The UI talks to `MockBackend`; the real core modules arrive in later stages.

mod webview;

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
            if let Some(w) = app.get_webview_window("main") {
                webview::harden(&w);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Kite");
}
