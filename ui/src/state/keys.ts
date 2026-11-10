/* App keyboard: global shortcuts first, then the dialog on top, then the focused pane. */
import { closeDlg, closeMenu, dlg, menu } from './overlay';
import { ui } from './prefs';
import {
  copyTo, focusList, goBack, goUp, moveCur, newFolder, openFile, openRow, other, panes, refresh, removeSel, rows,
  fileEdit, startRename, swapPanes,
} from './panes';
import { focusSearch, setView, toggleSide, toggleTerm } from './layout';
import { closePalette, openKeys, openPalette, openSettings, pal } from './palette';

export const HOTKEYS: Record<string, string> = {
  'Серверы, папки, команды': 'Ctrl K', 'Поиск серверов': 'Ctrl P', 'Свернуть панель серверов': 'Ctrl B',
  'Только файлы · оба · только терминал': 'Ctrl 1 / 2 / 3', 'Показать или скрыть терминал': 'Ctrl `',
  'Другая панель': 'Tab', 'Открыть': 'Enter', 'Просмотр': 'F3', 'Редактировать': 'F4', 'Наверх': 'Backspace', 'Назад': 'Alt ←',
  'Копировать в другую панель': 'F5', 'Переместить в другую панель': 'F6', 'Переименовать': 'F2', 'Новая папка': 'F7', 'Удалить': 'Del',
  'Выделить всё': 'Ctrl A', 'Править путь': 'Ctrl L', 'Обновить': 'Ctrl R', 'Поменять панели местами': 'Ctrl U', 'Скрытые файлы': 'Ctrl H',
  'Открыть сервер в другой панели': 'Ctrl + щелчок', 'Переместить перетаскиванием': 'Shift + перетащить', 'Настройки': 'Ctrl ,',
};

export function onKey(e: KeyboardEvent) {
  const k = e.key, ctrl = e.ctrlKey || e.metaKey, target = e.target as Element | null;
  const inField = !!target?.closest?.('input, textarea, select');
  if (ctrl && e.code === 'KeyK') { e.preventDefault(); if (pal.open) closePalette(); else openPalette(); return; }
  if (ctrl && e.code === 'KeyP') { e.preventDefault(); closePalette(); focusSearch(); return; }
  if (ctrl && e.code === 'Backquote') { e.preventDefault(); toggleTerm(); return; }
  if ((ctrl || e.altKey) && !e.shiftKey && ['Digit1', 'Digit2', 'Digit3'].includes(e.code)) {
    e.preventDefault();
    setView((['files', 'both', 'term'] as const)[+e.code.slice(-1) - 1]);
    return;
  }
  if (ctrl && e.code === 'KeyB') { e.preventDefault(); toggleSide(); return; }
  if (ctrl && e.code === 'Comma') { e.preventDefault(); openSettings(); return; }
  if (pal.open) return;
  const top = dlg.value;
  if (top?.kind === 'file') {
    if (k === 'Escape') { e.preventDefault(); closeDlg(null); }
    else if (k === 'F4') { e.preventDefault(); fileEdit(top.data); }
    return;
  }
  if (top) { if (k === 'Escape') { e.preventDefault(); closeDlg(null); } return; }
  if (k === 'Escape' && menu.key) { closeMenu(); return; }
  if (k === 'F1') { e.preventDefault(); openKeys(); return; }
  if (inField || target?.closest?.('.tpanel') || ui.view === 'term') return;
  paneKey(e, ctrl);
}

function paneKey(e: KeyboardEvent, ctrl: boolean) {
  const i = ui.focus, p = panes[i];
  if (p.edit) return;
  const handled = () => e.preventDefault();
  const list = rows(i), cur = () => list.find(x => x.name === p.cur);
  switch (e.key) {
    case 'ArrowDown': handled(); moveCur(i, 1, e); return;
    case 'ArrowUp': handled(); moveCur(i, -1, e); return;
    case 'PageDown': handled(); moveCur(i, 10, e); return;
    case 'PageUp': handled(); moveCur(i, -10, e); return;
    case 'Home': handled(); moveCur(i, 0, e, 0); return;
    case 'End': handled(); moveCur(i, 0, e, -1); return;
    case 'Enter': handled(); if (p.cur === '..') goUp(i); else openRow(i, cur()); return;
    case 'Backspace': handled(); goUp(i); return;
    case 'Tab': handled(); focusList(other(i)); return;
    case 'F3': case 'F4': { handled(); const r = cur(); if (r && !r.dir) openFile(i, r, e.key === 'F3' ? 'view' : 'edit'); return; }
    case 'F5': handled(); if (ctrl) refresh(i); else copyTo(i, false); return;
    case 'F6': handled(); copyTo(i, true); return;
    case 'F2': handled(); startRename(i); return;
    case 'F7': handled(); newFolder(i); return;
    case 'Delete': handled(); removeSel(i); return;
    case 'Escape': p.sel = []; return;
    case 'ArrowLeft': if (e.altKey) { handled(); goBack(i); } return;
  }
  if (!ctrl) return;
  switch (e.code) {
    case 'KeyA': handled(); p.sel = list.map(r => r.name); return;
    case 'KeyR': handled(); refresh(i); return;
    case 'KeyU': handled(); swapPanes(); return;
    case 'KeyH': handled(); ui.hidden = !ui.hidden; return;
    case 'KeyL': handled(); document.querySelector<HTMLInputElement>(`[data-pane="${i}"] .path-in`)?.focus(); return;
  }
}

export function onMouseDown(e: MouseEvent) {
  if (menu.key && !(e.target as Element).closest('.menu')) closeMenu();
}
/** The mouse "back" button goes back in the focused pane. */
export function onMouseUp(e: MouseEvent) {
  if (e.button === 3 && !dlg.value) goBack(ui.focus);
}
