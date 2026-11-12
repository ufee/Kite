/* The frameless window: caption buttons, close confirmation, showing the window once painted. */
import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { saveWindowState, StateFlags } from '@tauri-apps/plugin-window-state';
import { backend } from '../backend';
import { tr } from '../i18n';
import { confirmBox, toast } from '../state/overlay';
import { active } from '../state/queue';
import { flush } from '../state/persist';

export const inTauri = isTauri();
const win = () => getCurrentWindow();

async function confirmClose(): Promise<boolean> {
  // Saves the editor made that went nowhere yet (not confirmed, or a conflict left open): their copies go with the app.
  const unsent = await backend.editUnsent().catch(() => []);
  const why: string[] = [];
  if (active.value.length) why.push(tr('Идут передачи: {n}. Они прервутся.', { n: active.value.length }));
  if (unsent.length) why.push(tr('Не загружены правки: {list}. Они пропадут.', { list: unsent.map(u => u.name).join(', ') }));
  if (!why.length) return true;
  return !!(await confirmBox('Закрыть Kite?', why.join(' '), 'Закрыть', true));
}
/**
 * The session is written before the window goes: a failed write must not keep the app open.
 * `destroy` skips CloseRequested, where the window-state plugin would note "maximized", so it is saved here.
 */
async function closeNow() {
  const place = saveWindowState(StateFlags.ALL & ~StateFlags.VISIBLE).catch(() => undefined);
  await Promise.race([Promise.all([flush().catch(() => undefined), place]), new Promise(r => setTimeout(r, 2000))]);
  await win().destroy();
}

export async function windowBtn(kind: 'min' | 'max' | 'close') {
  if (!inTauri) {
    // In a plain browser tab (UI development) there is no window to drive.
    if (kind === 'close' && !(await confirmClose())) return;
    toast(kind === 'close' ? 'В браузере окно не закрывается' : kind === 'min' ? 'В браузере окно не сворачивается' : 'В браузере окно и так во всю вкладку', 'info', null, 2000);
    return;
  }
  if (kind === 'min') await win().minimize();
  else if (kind === 'max') await win().toggleMaximize();
  else if (await confirmClose()) await closeNow();
}

export async function initWindow() {
  if (!inTauri) return;
  // Alt+F4 and the taskbar close go through the same question as the caption button.
  await win().onCloseRequested(async e => {
    e.preventDefault();
    if (await confirmClose()) await closeNow();
  });
  // The window starts hidden: it appears after the first paint, without a white flash.
  requestAnimationFrame(() => requestAnimationFrame(() => { win().show(); win().setFocus(); }));
}
