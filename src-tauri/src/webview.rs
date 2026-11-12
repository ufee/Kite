//! WebView hardening: the app must not behave like a browser.
//!
//! The UI also swallows browser keys in JS (see `ui/src/platform/keys.ts`); this is the native layer,
//! so F5, Ctrl R, Ctrl P, Ctrl F, F12 and zoom stay dead even when the page has no focus handler.

use tauri::WebviewWindow;

#[cfg(windows)]
pub fn harden(window: &WebviewWindow) {
    use webview2_com::{
        Microsoft::Web::WebView2::Win32::{
            COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ, COREWEBVIEW2_PERMISSION_STATE_ALLOW,
            ICoreWebView2Settings3,
        },
        PermissionRequestedEventHandler, take_pwstr,
    };
    use windows_core::{Interface, PWSTR};

    let _ = window.with_webview(|wv| unsafe {
        let Ok(core) = wv.controller().CoreWebView2() else { return };
        let Ok(settings) = core.Settings() else { return };
        let _ = settings.SetIsZoomControlEnabled(false);
        let _ = settings.SetIsStatusBarEnabled(false);
        // The default context menu stays on: the UI cancels it everywhere except text fields,
        // which keep cut, copy and paste.
        // Debug builds keep the accelerators so the DevTools stay reachable.
        if !cfg!(debug_assertions)
            && let Ok(s3) = settings.cast::<ICoreWebView2Settings3>()
        {
            let _ = s3.SetAreBrowserAcceleratorKeysEnabled(false);
        }
        // "From clipboard" in the import reads the clipboard: the app's own page gets no browser prompt.
        // Only the app's origin: anything else keeps the default (a prompt, or a refusal).
        let handler = PermissionRequestedEventHandler::create(Box::new(|_, args| {
            let Some(args) = args else { return Ok(()) };
            let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
            args.PermissionKind(&mut kind)?;
            let mut uri = PWSTR::null();
            args.Uri(&mut uri)?;
            if kind == COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ && own_page(&take_pwstr(uri)) {
                args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
            }
            Ok(())
        }));
        let mut token = 0i64;
        let _ = core.add_PermissionRequested(&handler, &mut token);
    });
}

/// The app's page: `tauri.localhost` as built, the Vite server in a debug build.
#[cfg(windows)]
fn own_page(uri: &str) -> bool {
    let Ok(url) = tauri::Url::parse(uri) else { return false };
    // A debug build runs on the Vite server.
    matches!(url.host_str(), Some("tauri.localhost")) || (cfg!(debug_assertions) && url.host_str() == Some("localhost"))
}

#[cfg(not(windows))]
pub fn harden(_window: &WebviewWindow) {}

/// Paths of files dropped from Explorer.
///
/// The OS drop stays with the page (`dragDropEnabled: false`), so the panes keep HTML5 drag and drop
/// between them, but a page `File` has no path. The UI passes the dropped files to
/// `chrome.webview.postMessageWithAdditionalObjects`; WebView2 hands them over as `ICoreWebView2File`
/// with the real path. Only files the user dropped or picked can get here: a script cannot make one.
/// Tauri's own IPC handler sees the same message, fails to parse it as a call and drops it.
#[cfg(windows)]
pub fn explorer_drop(window: &WebviewWindow) {
    use tauri::{Emitter, Manager};
    use webview2_com::{
        Microsoft::Web::WebView2::Win32::{ICoreWebView2File, ICoreWebView2WebMessageReceivedEventArgs2},
        WebMessageReceivedEventHandler, take_pwstr,
    };
    use windows_core::{Interface, PWSTR};

    #[derive(serde::Deserialize)]
    struct Msg {
        kite: String,
        id: u64,
    }

    /// Files dropped from Explorer, grouped by the folder they came from.
    #[derive(serde::Serialize, Clone)]
    struct Dropped {
        id: u64,
        groups: Vec<DropGroup>,
    }

    #[derive(serde::Serialize, Clone)]
    struct DropGroup {
        path: String,
        entries: Vec<crate::fs::Entry>,
    }

    let app = window.app_handle().clone();
    let _ = window.with_webview(move |wv| unsafe {
        let Ok(core) = wv.controller().CoreWebView2() else { return };
        let handler = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            let mut from = PWSTR::null();
            if args.Source(&mut from).is_err() || !own_page(&take_pwstr(from)) {
                return Ok(());
            }
            let mut raw = PWSTR::null();
            if args.TryGetWebMessageAsString(&mut raw).is_err() {
                return Ok(());
            }
            let Ok(msg) = serde_json::from_str::<Msg>(&take_pwstr(raw)) else { return Ok(()) };
            if msg.kite != "drop" {
                return Ok(());
            }
            let Ok(objs) = args.cast::<ICoreWebView2WebMessageReceivedEventArgs2>().and_then(|a| a.AdditionalObjects()) else {
                return Ok(());
            };
            let mut n = 0;
            let _ = objs.Count(&mut n);
            let mut paths = Vec::new();
            for i in 0..n {
                let Ok(file) = objs.GetValueAtIndex(i).and_then(|o| o.cast::<ICoreWebView2File>()) else { continue };
                let mut p = PWSTR::null();
                if file.Path(&mut p).is_ok() {
                    paths.push(std::path::PathBuf::from(take_pwstr(p)));
                }
            }
            let app = app.clone();
            // A network drive may take a while to answer; the UI thread must not wait for it.
            tauri::async_runtime::spawn_blocking(move || {
                let mut groups: Vec<DropGroup> = Vec::new();
                for (path, e) in paths.iter().filter_map(|p| crate::fs::local::stat(p)) {
                    match groups.iter_mut().find(|g| g.path == path) {
                        Some(g) => g.entries.push(e),
                        None => groups.push(DropGroup { path, entries: vec![e] }),
                    }
                }
                let _ = app.emit("drop://files", Dropped { id: msg.id, groups });
            });
            Ok(())
        }));
        let mut token = 0i64;
        let _ = core.add_WebMessageReceived(&handler, &mut token);
    });
}

#[cfg(not(windows))]
pub fn explorer_drop(_window: &WebviewWindow) {}
