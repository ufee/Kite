/*
 * WebView keys that would break the app's illusion of being native: reload, print, find, zoom,
 * devtools, caret browsing, history navigation. They are swallowed in the capture phase, before
 * the browser acts; the app's own handlers still see the event, because only the default is prevented.
 * The core disables the same keys in WebView2 itself (browser accelerator keys off); this is the second wall.
 */
const BLOCK_CTRL = new Set(['KeyR', 'KeyP', 'KeyU', 'KeyF', 'KeyG', 'KeyJ', 'KeyS', 'KeyO', 'KeyN', 'KeyT', 'KeyW', 'Equal', 'Minus', 'Digit0', 'NumpadAdd', 'NumpadSubtract', 'Numpad0']);
const BLOCK_CTRL_SHIFT = new Set(['KeyI', 'KeyJ', 'KeyC', 'KeyR', 'KeyP', 'KeyN', 'KeyT', 'KeyW', 'Delete']);
const BLOCK_PLAIN = new Set(['F5', 'F7', 'F12', 'BrowserBack', 'BrowserForward', 'BrowserRefresh', 'BrowserSearch', 'BrowserHome']);

function blocked(e: KeyboardEvent): boolean {
  const ctrl = e.ctrlKey || e.metaKey;
  if (BLOCK_PLAIN.has(e.code) || BLOCK_PLAIN.has(e.key)) return true;
  if (e.altKey && !ctrl && (e.code === 'ArrowLeft' || e.code === 'ArrowRight' || e.code === 'Home')) return true;
  if (ctrl && e.shiftKey && BLOCK_CTRL_SHIFT.has(e.code)) return true;
  if (ctrl && !e.shiftKey && !e.altKey && BLOCK_CTRL.has(e.code)) return true;
  return false;
}

/** Text fields keep their editing keys; only the browser-level defaults above are cut. */
export function guardWebviewKeys() {
  addEventListener('keydown', e => { if (blocked(e)) e.preventDefault(); }, { capture: true });
  // Ctrl + wheel zooms the page.
  addEventListener('wheel', e => { if (e.ctrlKey) e.preventDefault(); }, { capture: true, passive: false });
  // Mouse buttons 3 and 4 navigate history; the app reads them on mouseup itself.
  const nav = (e: MouseEvent) => { if (e.button === 3 || e.button === 4) e.preventDefault(); };
  addEventListener('mousedown', nav, { capture: true });
  addEventListener('mouseup', nav, { capture: true });
  addEventListener('auxclick', nav, { capture: true });
  // The browser context menu: only text fields keep it (copy, paste); app menus call preventDefault themselves.
  addEventListener('contextmenu', e => {
    if (!(e.target as Element).closest?.('input, textarea')) e.preventDefault();
  });
  // Dropping a file outside a drop zone would navigate the WebView to it.
  addEventListener('dragover', e => {
    if (e.defaultPrevented) return; // a drop zone accepted it
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'none';
  });
  addEventListener('drop', e => e.preventDefault());
}
