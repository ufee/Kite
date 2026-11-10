/* The frameless window: caption buttons, close confirmation, showing the window once painted. */
import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { tr } from '../i18n';
import { confirmBox, toast } from '../state/overlay';
import { active } from '../state/queue';

export const inTauri = isTauri();
const win = () => getCurrentWindow();

async function confirmClose(): Promise<boolean> {
  if (!active.value.length) return true;
  return !!(await confirmBox('Закрыть Kite?', tr('Идут передачи: {n}. Они прервутся.', { n: active.value.length }), 'Закрыть', true));
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
  else if (await confirmClose()) await win().destroy();
}

export async function initWindow() {
  if (!inTauri) return;
  // Alt+F4 and the taskbar close go through the same question as the caption button.
  await win().onCloseRequested(async e => {
    e.preventDefault();
    if (await confirmClose()) await win().destroy();
  });
  // The window starts hidden: it appears after the first paint, without a white flash.
  requestAnimationFrame(() => requestAnimationFrame(() => { win().show(); win().setFocus(); }));
}
