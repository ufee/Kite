/* Command palette (Ctrl K): servers, recent folders and commands in one list. */
import { computed, nextTick, reactive, watch } from 'vue';
import type { Src } from '../backend';
import { tr } from '../i18n';
import { collator } from '../util/format';
import { theme, toggleTheme, ui } from './prefs';
import { ask, closeMenu } from './overlay';
import { label, recent, recentDirs, servers, srvState } from './servers';
import { other, setSource, swapPanes } from './panes';
import { newTermHere } from './terms';
import { setView } from './layout';
import { openServer } from './serverCard';
import { openImport } from './importExport';

export const pal = reactive({ open: false, q: '', idx: 0, target: null as number | null });

export function openPalette(target: number | null = null, q = '') {
  closeMenu();
  Object.assign(pal, { open: true, q, idx: 0, target });
  nextTick(() => document.querySelector<HTMLInputElement>('.pal-in input')?.focus());
}
export const closePalette = () => { pal.open = false; };
export const openSettings = () => ask('settings', {});
export const openKeys = () => ask('keys', {});

const COMMANDS = () => [
  { title: 'Новый терминал', hint: 'Ctrl `', run: () => newTermHere() },
  { title: 'Вид: только файлы', hint: 'Ctrl 1', run: () => setView('files') },
  { title: 'Вид: файлы и терминал', hint: 'Ctrl 2', run: () => setView('both') },
  { title: 'Вид: только терминал', hint: 'Ctrl 3', run: () => setView('term') },
  { title: 'Поменять панели местами', hint: 'Ctrl U', run: () => swapPanes() },
  { title: theme.value === 'dark' ? 'Тема: светлая' : 'Тема: тёмная', hint: '', run: () => toggleTheme() },
  { title: ui.hidden ? 'Не показывать скрытые файлы' : 'Показать скрытые файлы', hint: 'Ctrl H', run: () => { ui.hidden = !ui.hidden; } },
  { title: 'Новый сервер…', hint: '', run: () => openServer(null) },
  { title: 'Импорт и экспорт…', hint: '', run: () => openImport() },
  { title: 'Настройки', hint: 'Ctrl ,', run: () => openSettings() },
  { title: 'Горячие клавиши', hint: 'F1', run: () => openKeys() },
].map(c => ({ ...c, title: tr(c.title) }));

export interface PalItem {
  sec: string;
  kind: 'srv' | 'dir' | 'cmd';
  icon: string;
  title: string;
  sub?: string;
  hint?: string;
  live?: boolean;
  run: (alt: boolean) => void;
}

export const palItems = computed<PalItem[]>(() => {
  const raw = pal.q.trim(), items: PalItem[] = [];
  const cmdOnly = raw.startsWith('>');
  const m = raw.match(/^([\w.-]+)@(.*)$/);
  const user = m ? m[1] : undefined, q = (cmdOnly ? raw.slice(1) : m ? m[2] : raw).trim().toLowerCase();
  if (!cmdOnly) {
    const rank = (id: string) => recent.indexOf(id) + 1 || 99;
    const list = servers.filter(s => !q || `${s.code} ${s.description} ${s.host}`.toLowerCase().includes(q))
      .sort((a, b) => (q ? +b.code.startsWith(q) - +a.code.startsWith(q) : 0) || rank(a.id) - rank(b.id) || collator.compare(a.code, b.code))
      .slice(0, q ? 6 : 5);
    for (const s of list) {
      items.push({ sec: tr(q ? 'Серверы' : 'Недавние'), kind: 'srv', icon: 'server', title: (user ? user + '@' : '') + s.code, sub: s.description, live: srvState(s.id) === 'live', run: alt => openFrom(s.id, user, alt) });
    }
    if (q && !user) {
      for (const d of recentDirs.filter(d => d.path.toLowerCase().includes(q)).slice(0, 3))
        items.push({ sec: tr('Папки'), kind: 'dir', icon: 'folder', title: d.path, sub: label(d.src), run: alt => openFrom(d.src, d.user, alt, d.path) });
    }
  }
  if (!user) {
    const cmds = COMMANDS().filter(c => !q || c.title.toLowerCase().includes(q)).slice(0, q || cmdOnly ? 8 : 4);
    for (const c of cmds) items.push({ sec: tr('Команды'), kind: 'cmd', icon: 'sliders', title: c.title, hint: c.hint, run: c.run });
  }
  return items;
});

/** Alt+Enter opens the server or folder in the other pane. */
function openFrom(src: Src, user: string | undefined, alt: boolean, path?: string) {
  const base = pal.target ?? ui.focus, i = alt ? other(base) : base;
  ui.focus = i;
  setSource(i, src, user, path);
}
export function runPal(k: number, alt = false) {
  const it = palItems.value[k];
  if (!it) return;
  closePalette();
  it.run(alt);
}
export function palKey(ev: KeyboardEvent) {
  const n = palItems.value.length;
  if (ev.key === 'ArrowDown') { ev.preventDefault(); pal.idx = n ? (pal.idx + 1) % n : 0; }
  else if (ev.key === 'ArrowUp') { ev.preventDefault(); pal.idx = n ? (pal.idx - 1 + n) % n : 0; }
  else if (ev.key === 'Enter') { ev.preventDefault(); runPal(pal.idx, ev.altKey); }
  else if (ev.key === 'Escape') { ev.preventDefault(); closePalette(); }
  nextTick(() => document.querySelector('.pal-it.on')?.scrollIntoView({ block: 'nearest' }));
}
watch(() => pal.q, () => { pal.idx = 0; });
