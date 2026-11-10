//! WebView hardening: the app must not behave like a browser.
//!
//! The UI also swallows browser keys in JS (see `ui/src/platform/keys.ts`); this is the native layer,
//! so F5, Ctrl R, Ctrl P, Ctrl F, F12 and zoom stay dead even when the page has no focus handler.

use tauri::WebviewWindow;

#[cfg(windows)]
pub fn harden(window: &WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
    use windows_core::Interface;

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
    });
}

#[cfg(not(windows))]
pub fn harden(_window: &WebviewWindow) {}
