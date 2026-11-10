/* The two file panes: navigation, selection, file operations, drag and drop, viewer, editing. */
import { computed, markRaw, nextTick, reactive } from 'vue';
import { backend, isBackendError, type BackendError, type Entry, type Src } from '../backend';
import { tr } from '../i18n';
import { collator, copyText, fmtSize, objs } from '../util/format';
import { baseName, childOf, homeOf, isLocal, parentOf, rootOf, within } from '../util/paths';
import { settings, ui, type ColKey } from './prefs';
import { ask, closeDlg, confirmBox, dlg, openMenu, toast } from './overlay';
import { label, srvBy, touchDir, touchRecent } from './servers';
import { transfer } from './queue';

export interface Pane {
  src: Src;
  user: string;
  path: string;
  input: string;
  list: Entry[];
  sel: string[];
  cur: string | null;
  anchor: string | null;
  back: string[];
  state: 'ok' | 'loading' | 'error' | 'off';
  msg: { title: string; text?: string } | null;
  edit: { name: string | null; value: string; isNew: boolean } | null;
  pathErr: string;
  dropping: boolean;
  dropRow: string | null;
  spin: boolean;
  sort: { key: 'name' | 'size' | 'date'; dir: number };
  token: number;
  nav: number;
}
interface Saved { path: string; sel: string[]; cur: string | null; anchor: string | null; back: string[]; sort: Pane['sort']; scroll: number }
/** What one pane remembers: the last user per source and the last state per source + user. */
export interface PaneMem { last: Record<string, string>; states: Record<string, Saved> }

const newPane = (src: Src, user: string, path: string): Pane => ({
  src, user, path, input: path, list: markRaw([]), sel: [], cur: null, anchor: null, back: [],
  state: 'loading', msg: { title: tr('Загрузка…') }, edit: null, pathErr: '', dropping: false, dropRow: null, spin: false,
  sort: { key: 'name', dir: 1 }, token: 0, nav: 0,
});
export const panes = reactive<Pane[]>([newPane('local', '', ''), newPane('local', '', '')]);
const mem: PaneMem[] = [{ last: {}, states: {} }, { last: {}, states: {} }];
export const other = (i: number) => 1 - i;

/** Hooks the virtual list registers, so the state can scroll and read the scroll offset. */
export const listApi: { scrollTo?: (name: string | null) => void; scrollTop?: (y?: number) => number; focus?: () => void }[] = [{}, {}];

const errOf = (e: unknown): BackendError => (isBackendError(e) ? e : { code: 'EINVAL', title: String(e), text: '' });

/* ---------- Rows, columns, footer ---------- */
function rowsOf(p: Pane): Entry[] {
  if (p.state !== 'ok') return [];
  let list = p.list;
  if (!ui.hidden) list = list.filter(e => !e.hidden && !e.name.startsWith('.'));
  const { key, dir } = p.sort;
  const cmp = (a: Entry, b: Entry) => (key === 'size' ? a.size - b.size : key === 'date' ? a.mtime - b.mtime : collator.compare(a.name, b.name));
  return [...list].sort((a, b) => (+b.dir - +a.dir) || cmp(a, b) * dir || collator.compare(a.name, b.name));
}
const rowsC = [computed(() => rowsOf(panes[0])), computed(() => rowsOf(panes[1]))];
export const rows = (i: number) => rowsC[i].value;
export const selSet = [computed(() => new Set(panes[0].sel)), computed(() => new Set(panes[1].sel))];

export const COLS: { k: ColKey; t: string }[] = [{ k: 'size', t: 'Размер' }, { k: 'date', t: 'Изменён' }, { k: 'perm', t: 'Права' }, { k: 'owner', t: 'Владелец' }, { k: 'group', t: 'Группа' }];
// Optional columns drop out (lowest priority first) when the pane is too narrow for the name.
const COL_W: Record<ColKey, number> = { size: 72, date: 92, perm: 84, owner: 56, group: 64 };
export const paneW = reactive([720, 720]);
export const vis = computed(() => panes.map((p, i) => {
  let room = paneW[i] - 40 - 160;
  const out: Partial<Record<ColKey, boolean>> = {};
  for (const k of ['size', 'date', 'perm', 'owner', 'group'] as ColKey[]) {
    if (!ui.cols[k] || (isLocal(p.src) && !['size', 'date'].includes(k))) continue;
    if (room < COL_W[k] + 12) break;
    room -= COL_W[k] + 12; out[k] = true;
  }
  return out;
}));
export const gridOf = (i: number) => {
  const v = vis.value[i];
  return ['minmax(0,1fr)', ...(Object.keys(COL_W) as ColKey[]).filter(k => v[k]).map(k => COL_W[k] + 'px')].join(' ');
};
export function sortBy(i: number, key: Pane['sort']['key']) {
  const s = panes[i].sort;
  if (s.key === key) s.dir = -s.dir; else { s.key = key; s.dir = key === 'name' ? 1 : -1; }
}
export function paneFoot(i: number) {
  const p = panes[i], list = rows(i);
  if (p.state !== 'ok') return '';
  // Sums only the listed files: folder contents are never walked.
  const total = (l: Entry[]) => l.filter(r => !r.dir).reduce((a, r) => a + r.size, 0);
  const withSize = (text: string, size: number) => (size ? `${text} · ${fmtSize(size)}` : text);
  const set = selSet[i].value, sel = set.size ? list.filter(r => set.has(r.name)) : [];
  const s = withSize(objs(list.length), total(list));
  return sel.length ? `${s} · ${withSize(tr('выбрано {n}', { n: sel.length }), total(sel))}` : s;
}
export function idleInfo() {
  const p = panes[ui.focus];
  if (isLocal(p.src)) return label('local') + ' · ' + p.path;
  const s = srvBy(p.src);
  if (!s) return '';
  const u = s.users.find(x => x.name === p.user);
  const file = u && u.key !== 'auto' ? u.key : s.keyMode === 'file' ? s.keyFile.split(/[\\/]/).pop() : '';
  const key = s.authPass ? tr('вход по паролю') : file ? tr('ключ {k}', { k: file }) : tr('ключ из агента');
  return `${p.user}@${s.code} · ${s.host}${s.port !== 22 ? ':' + s.port : ''} · ${key}`;
}
export const hasUp = (i: number) => panes[i].state === 'ok' && !!parentOf(panes[i].src, panes[i].path);

/* ---------- Memory per source and user ---------- */
function paneSave(i: number) {
  const p = panes[i];
  if (p.state !== 'ok') return; // keep the last good state while connecting or failed
  mem[i].last[p.src] = p.user;
  mem[i].states[p.src + '|' + p.user] = {
    path: p.path, sel: [...p.sel], cur: p.cur, anchor: p.anchor, back: [...p.back], sort: { ...p.sort },
    scroll: listApi[i].scrollTop?.() ?? 0,
  };
}
/** Both panes' memory for session.json, with the current places saved first. Removed servers drop out. */
export function paneMemory(): PaneMem[] {
  panes.forEach((_, i) => paneSave(i));
  const keep = (src: string) => isLocal(src) || !!srvBy(src);
  return mem.map(m => ({
    last: Object.fromEntries(Object.entries(m.last).filter(([src]) => keep(src))),
    states: Object.fromEntries(Object.entries(m.states).filter(([k]) => keep(k.slice(0, k.indexOf('|'))))),
  }));
}
export function setPaneMemory(saved: PaneMem[]) {
  saved.slice(0, 2).forEach((m, i) => { mem[i] = { last: { ...m.last }, states: { ...m.states } }; });
}
// This pane's own memory first, then whatever the other pane remembers about the same server.
const paneState = (i: number, src: Src, user: string) => mem[i].states[src + '|' + user] || mem[other(i)].states[src + '|' + user] || null;
function paneUser(i: number, id: string) {
  const s = srvBy(id)!;
  const u = mem[i].last[id] || mem[other(i)].last[id];
  return s.users.some(x => x.name === u) ? u : s.defaultUser;
}
function paneRestore(i: number, saved: Saved | null) {
  const p = panes[i];
  if (!saved) return;
  p.sort = { ...saved.sort };
  if (saved.path !== p.path) return; // the folder is gone or another one was asked for
  const has = (n: string | null) => !!n && (n === '..' || p.list.some(e => e.name === n));
  p.sel = saved.sel.filter(has);
  if (has(saved.cur)) p.cur = saved.cur;
  p.anchor = has(saved.anchor) ? saved.anchor : p.cur;
  p.back = [...saved.back];
  nextTick(() => listApi[i].scrollTop?.(saved.scroll));
}

/* ---------- Navigation ---------- */
export async function setSource(i: number, src: Src, user?: string, path?: string) {
  const p = panes[i];
  paneSave(i);
  Object.assign(p, { edit: null, sel: [], cur: null, anchor: null, pathErr: '', back: [] });
  const token = ++p.token;
  if (isLocal(src)) {
    Object.assign(p, { src, user: '', state: 'ok', msg: null });
    const saved = paneState(i, 'local', '');
    const home = await backend.fsHome('local', '');
    // A folder that is gone falls back to the home folder, then to the root of its drive.
    if (!(await go(i, path || saved?.path || home, false, !path)) && p.token === token) {
      if (!(await go(i, home, false, true)) && p.token === token) await go(i, rootOf('local', home), false);
    }
    if (p.token === token) paneRestore(i, saved);
    return;
  }
  const s = srvBy(src);
  if (!s) return;
  user = user || paneUser(i, src);
  const saved = paneState(i, src, user);
  const target = path || saved?.path || s.startDir || homeOf(user);
  Object.assign(p, { src, user, path: target, input: target, list: markRaw([]), state: 'loading', msg: { title: tr('Подключение к {who}…', { who: `${user}@${s.code}` }) } });
  touchRecent(src);
  try { await backend.sshConnect(s, user); } catch (e) {
    if (p.token !== token) return;
    p.state = 'error'; p.msg = errOf(e);
    return;
  }
  if (p.token !== token) return;
  p.state = 'ok';
  if (!(await go(i, target, false, true)) && p.token === token) {
    if (!(await go(i, homeOf(user), false, true)) && p.token === token) await go(i, '/', false);
  }
  if (p.token === token) paneRestore(i, saved);
}
export const retry = (i: number) => { const p = panes[i]; return setSource(i, p.src, p.user, p.path); };

export async function go(i: number, raw: string, push = true, quiet = false): Promise<boolean> {
  const p = panes[i];
  if (p.state !== 'ok') return false;
  const nav = ++p.nav, src = p.src, user = p.user;
  let path: string, list: Entry[];
  try {
    // A relative path is taken from the folder on screen.
    path = await backend.fsResolve(src, user, raw, p.path || undefined);
    list = await backend.fsList(src, user, path);
  } catch (e) {
    if (p.nav !== nav) return false;
    const err = errOf(e);
    p.pathErr = err.title;
    if (!quiet) toast(err.title, 'bad');
    return false;
  }
  if (p.nav !== nav || p.src !== src || p.user !== user) return false;
  if (push && path !== p.path) { p.back.push(p.path); if (p.back.length > 50) p.back.shift(); }
  const prev = p.path;
  Object.assign(p, { path, input: path, pathErr: '', edit: null, list: markRaw(list) });
  const fromChild = prev && parentOf(src, prev) === path ? baseName(src, prev) : null;
  const first = rows(i)[0]?.name || null;
  p.sel = fromChild ? [fromChild] : []; p.cur = fromChild || first; p.anchor = p.cur;
  touchDir(src, user, path);
  nextTick(() => scrollCur(i));
  return true;
}
/** Re-reads the folder, keeping the selection of names that are still there. */
export async function reload(i: number) {
  const p = panes[i];
  if (p.state !== 'ok') return;
  const nav = ++p.nav, { src, user, path } = p;
  try {
    const list = await backend.fsList(src, user, path);
    if (p.nav !== nav || p.path !== path || p.src !== src) return;
    p.list = markRaw(list);
    const has = (n: string) => list.some(e => e.name === n);
    p.sel = p.sel.filter(has);
    if (p.cur && p.cur !== '..' && !has(p.cur)) p.cur = rows(i)[0]?.name ?? null;
  } catch (e) { toast(errOf(e).title, 'bad'); }
}
export const goUp = (i: number) => { const par = parentOf(panes[i].src, panes[i].path); if (par) go(i, par); };
export const goBack = (i: number) => { const b = panes[i].back.pop(); if (b) go(i, b, false); };
export function refresh(i: number) {
  const p = panes[i];
  reload(i);
  p.spin = false;
  nextTick(() => { p.spin = true; setTimeout(() => { p.spin = false; }, 500); });
}
export async function pathKey(i: number, ev: KeyboardEvent) {
  const p = panes[i], el = ev.target as HTMLInputElement;
  if (ev.key === 'Enter') { if (await go(i, p.input)) el.blur(); }
  else if (ev.key === 'Escape') { p.input = p.path; p.pathErr = ''; el.blur(); }
}
export function pathBlur(i: number) { const p = panes[i]; setTimeout(() => { p.input = p.path; p.pathErr = ''; }, 120); }
export async function copyPath(i: number) {
  await copyText(panes[i].path);
  ui.copied = i;
  setTimeout(() => { if (ui.copied === i) ui.copied = null; }, 1400);
}
export const scrollCur = (i: number) => listApi[i].scrollTo?.(panes[i].cur);
export function focusList(i: number) { ui.focus = i; nextTick(() => listApi[i].focus?.()); }
export function swapPanes() {
  panes.reverse(); mem.reverse(); paneW.reverse();
  ui.focus = other(ui.focus);
}

/* ---------- Selection and cursor ---------- */
export function clickRow(i: number, e: Entry, ev: MouseEvent) {
  const p = panes[i], names = rows(i).map(r => r.name);
  ui.focus = i;
  if (ev.shiftKey && p.anchor && names.includes(p.anchor)) {
    const a = names.indexOf(p.anchor), b = names.indexOf(e.name);
    p.sel = names.slice(Math.min(a, b), Math.max(a, b) + 1);
  } else if (ev.ctrlKey || ev.metaKey) {
    p.sel = p.sel.includes(e.name) ? p.sel.filter(n => n !== e.name) : [...p.sel, e.name]; p.anchor = e.name;
  } else { p.sel = [e.name]; p.anchor = e.name; }
  p.cur = e.name;
}
export function openRow(i: number, e: Entry | undefined) {
  if (!e) return;
  const p = panes[i];
  if (e.dir) { go(i, childOf(p.src, p.path, e.name)); return; }
  openFile(i, e, 'view');
}
// ".." row: a cursor stop that is never part of the selection
export function clickUp(i: number) { const p = panes[i]; ui.focus = i; p.sel = []; p.cur = '..'; p.anchor = null; }
export function moveCur(i: number, delta: number, ev: KeyboardEvent, abs?: number) {
  const p = panes[i], names = [...(hasUp(i) ? ['..'] : []), ...rows(i).map(r => r.name)];
  if (!names.length) return;
  let k = names.indexOf(p.cur ?? '');
  k = abs != null ? abs : k < 0 ? 0 : Math.max(0, Math.min(names.length - 1, k + delta));
  if (abs === -1) k = names.length - 1;
  p.cur = names[k];
  if (ev.shiftKey) {
    const a = Math.max(0, names.indexOf(p.anchor ?? p.cur));
    p.sel = names.slice(Math.min(a, k), Math.max(a, k) + 1).filter(n => n !== '..');
  } else if (p.cur === '..') { p.sel = []; p.anchor = null; }
  else { p.sel = [p.cur]; p.anchor = p.cur; }
  nextTick(() => scrollCur(i));
}
export const selEntries = (i: number) => {
  const p = panes[i], names = new Set(p.sel.length ? p.sel : p.cur ? [p.cur] : []);
  return p.list.filter(e => names.has(e.name));
};
export const rowByName = (i: number, name: string) => panes[i].list.find(e => e.name === name);
export function rowMenu(i: number, e: Entry, ev: MouseEvent) {
  const p = panes[i];
  ui.focus = i;
  if (!p.sel.includes(e.name)) { p.sel = [e.name]; p.anchor = e.name; }
  p.cur = e.name;
  openMenu('row', ev, { i, name: e.name, dir: e.dir });
}

/* ---------- Viewing and editing ---------- */
export const isText = (name: string) => !/\.(zip|gz|tgz|tar|7z|rar|png|jpe?g|gif|webp|ico|pdf|exe|dll|bin|iso|woff2?|mp4|dump|archive|xlsx)$/i.test(name);
const EDITORS: Record<string, string> = { system: 'системном редакторе', npp: 'Notepad++', code: 'VS Code' };

export async function openFile(i: number, e: Entry | undefined, mode: 'view' | 'edit') {
  const p = panes[i];
  if (!e || e.dir) return;
  if (!isText(e.name)) { toast(tr('Для {n} нет просмотра', { n: e.name }), 'info'); return; }
  if (mode === 'edit') { editExternal(p.src, p.user, p.path, e.name); return; }
  const { src, user, path } = p;
  try {
    const f = await backend.fsReadText(src, user, childOf(src, path, e.name));
    if (f.binary) { toast(tr('Для {n} нет просмотра', { n: e.name }), 'info'); return; }
    if (f.tooBig) { toast(tr('{n} больше 10 МБ: откройте в редакторе', { n: e.name }), 'info'); return; }
    ask('file', { name: e.name, src, user, path, where: `${label(src)} · ${path}`, text: f.text });
    nextTick(() => document.querySelector<HTMLElement>('.file-text')?.focus());
  } catch (err) { toast(errOf(err).title, 'bad'); }
}
export function fileEdit(d: Record<string, any>) {
  const { src, user, path, name } = d;
  closeDlg(null);
  editExternal(src, user, path, name);
}
// Remote files are downloaded to a temp copy and watched; a save in the editor triggers the upload.
async function editExternal(src: Src, user: string, path: string, name: string) {
  const app = tr(EDITORS[settings.editor]);
  try { await backend.editOpen(src, user, childOf(src, path, name), settings.editor, settings.autosave); } catch (e) {
    toast(errOf(e).title, 'bad'); return;
  }
  if (isLocal(src)) { toast(tr('{n} открыт в {app}', { n: name, app }), 'info'); return; }
  const where = label(src);
  toast(`${tr('{n} открыт в {app}', { n: name, app })}. ${tr(settings.autosave ? 'Правки загрузятся на {src}' : 'Загрузка на {src} после подтверждения', { src: where })}`, 'info', null, 4000);
}
async function editChanged(src: Src, user: string, path: string, uploaded: boolean) {
  const name = baseName(src, path), where = label(src);
  if (!uploaded) {
    // Queued in the dialog stack: a second save waits for its turn instead of being dropped.
    if (!(await confirmBox(tr('Загрузить {n}?', { n: name }), tr('Файл изменён в редакторе. Загрузить на {src}?', { src: where }), 'Загрузить'))) return;
    try { await backend.editUpload(src, user, path); } catch (e) { toast(errOf(e).title, 'bad'); return; }
  }
  toast(tr('{n} загружен на {src}', { n: name, src: where }));
}

/* ---------- "Copy to…" / "Move to…" destination picker ---------- */
export function pickDest(i: number, move = false) {
  const p = panes[i], q = panes[other(i)], list = selEntries(i);
  if (p.state !== 'ok' || !list.length) { toast('Ничего не выбрано', 'info'); return; }
  // Move starts from the active pane's folder, copy from whatever the other pane shows.
  const t = move || q.state !== 'ok' ? p : q;
  const d = { i, move, names: list.map(e => e.name), src: t.src, user: t.user, path: t.path, input: t.path, err: '', dirs: markRaw([] as Entry[]), loading: false };
  ask('dest', d);
  destList(dlg.value!.data);
}
async function destList(d: Record<string, any>) {
  d.loading = true;
  try {
    const list: Entry[] = await backend.fsList(d.src, d.user, d.path);
    d.dirs = markRaw(list.filter(e => e.dir && (ui.hidden || !e.name.startsWith('.'))).sort((a, b) => collator.compare(a.name, b.name)));
  } catch (e) { d.err = errOf(e).title; d.dirs = markRaw([]); }
  d.loading = false;
}
export function destBlocked(d: Record<string, any>) {
  const p = panes[d.i];
  if (d.err) return d.err;
  if (d.loading) return '';
  if (d.src === p.src && d.path === p.path) return tr('Это исходная папка');
  if (d.src === p.src && d.names.some((n: string) => within(p.src, childOf(p.src, p.path, n), d.path))) return tr('Нельзя поместить папку в саму себя');
  return '';
}
export async function destGo(d: Record<string, any>, raw: string) {
  try { d.path = await backend.fsResolve(d.src, d.user, raw, d.path || undefined); } catch (e) { d.err = errOf(e).title; return; }
  d.input = d.path; d.err = '';
  destList(d);
}
export async function destSource(d: Record<string, any>) {
  const open = panes.find(p => p.state === 'ok' && p.src === d.src);
  d.err = '';
  if (open) { d.user = open.user; d.path = open.path; }
  else if (isLocal(d.src)) { d.user = ''; d.path = await backend.fsHome('local', ''); }
  else {
    const s = srvBy(d.src)!;
    d.user = s.defaultUser; d.path = s.startDir || homeOf(s.defaultUser); d.input = d.path; d.loading = true;
    try { await backend.sshConnect(s, d.user); } catch (e) { d.err = errOf(e).title; d.loading = false; return; }
  }
  d.input = d.path;
  destList(d);
}
export function destApply(d: Record<string, any>) {
  if (destBlocked(d) || d.loading) return;
  const p = panes[d.i], names = new Set<string>(d.names);
  const from = { src: p.src, user: p.user, path: p.path }, to = { src: d.src, user: d.user, path: d.path };
  closeDlg(null);
  transfer(from, p.list.filter(e => names.has(e.name)), to, d.move);
}

/* ---------- File operations ---------- */
export function copyTo(i: number, move = false) {
  const p = panes[i], q = panes[other(i)], list = selEntries(i);
  if (p.state !== 'ok' || !list.length) { toast('Ничего не выбрано', 'info'); return; }
  if (q.state !== 'ok') { toast('Другая панель не подключена', 'bad'); return; }
  transfer({ src: p.src, user: p.user, path: p.path }, list, { src: q.src, user: q.user, path: q.path }, move);
}
const editInput = (i: number) => document.querySelector<HTMLInputElement>(`[data-pane="${i}"] .edit-in`);
export function startRename(i: number) {
  const p = panes[i];
  if (!p.cur || p.cur === '..' || p.state !== 'ok') return;
  p.edit = { name: p.cur, value: p.cur, isNew: false };
  nextTick(() => {
    const el = editInput(i);
    if (!el) return;
    el.focus();
    const dot = p.cur!.lastIndexOf('.');
    el.setSelectionRange(0, dot > 0 ? dot : p.cur!.length);
  });
}
export function newFolder(i: number) {
  const p = panes[i];
  if (p.state !== 'ok') return;
  // Rights are checked by the server when the folder is created, not guessed here.
  p.edit = { name: null, value: tr('Новая папка'), isNew: true };
  nextTick(() => { listApi[i].scrollTop?.(0); const el = editInput(i); el?.focus(); el?.select(); });
}
let committing = false;
export async function commitEdit(i: number) {
  const p = panes[i], ed = p.edit;
  if (!ed || committing) return;
  const v = ed.value.trim();
  if (!v || v === ed.name) { p.edit = null; focusList(i); return; }
  if (/[\\/]/.test(v)) { toast('В имени не может быть / и \\', 'bad'); return; }
  if (p.list.some(e => e.name === v)) { toast(tr('{v} уже есть в этой папке', { v }), 'bad'); return; }
  committing = true;
  try {
    if (ed.isNew) await backend.fsMkdir(p.src, p.user, childOf(p.src, p.path, v));
    else await backend.fsRename(p.src, p.user, childOf(p.src, p.path, ed.name!), childOf(p.src, p.path, v));
  } catch (e) {
    const err = errOf(e);
    toast(err.title, 'bad');
    if (err.code !== 'EEXIST') { p.edit = null; focusList(i); }
    return;
  } finally { committing = false; }
  p.edit = null;
  await reload(i);
  p.sel = [v]; p.cur = v; p.anchor = v;
  focusList(i);
  nextTick(() => scrollCur(i));
}
export function cancelEdit(i: number) { panes[i].edit = null; focusList(i); }

export async function removeSel(i: number) {
  const p = panes[i], list = selEntries(i);
  if (!list.length) return;
  const what = list.length === 1 ? list[0].name : objs(list.length), where = label(p.src);
  const ok = isLocal(p.src)
    ? await confirmBox('Удалить в корзину?', tr('{what} из {p}', { what, p: p.path }), 'В корзину')
    : await confirmBox(tr('Удалить с {src}?', { src: where }), tr('{what} из {p}. Это нельзя отменить.', { what, p: p.path }), 'Удалить', true);
  if (!ok) return;
  const paths = list.map(e => childOf(p.src, p.path, e.name));
  try {
    if (isLocal(p.src)) await backend.fsTrash(paths); else await backend.fsDelete(p.src, p.user, paths);
  } catch (e) { toast(errOf(e).title, 'bad'); await reload(i); return; }
  await reload(i);
  p.sel = []; p.cur = rows(i)[0]?.name || null;
  toast(tr(isLocal(p.src) ? 'В корзине: {what}' : 'Удалено: {what}', { what }), 'ok');
  focusList(i);
}

export function openChmod(i: number) {
  const p = panes[i], list = selEntries(i);
  if (isLocal(p.src) || !list.length) return;
  // `S` and `T` are setuid/setgid/sticky without x: the execute box stays off.
  const e = list[0], bits = [0, 1, 2].map(g => [0, 1, 2].map(b => !/[-ST]/.test(e.perm[g * 3 + b] ?? '-')));
  ask('chmod', { i, names: list.map(x => x.name), bits, owner: e.owner, group: e.group, owner0: e.owner, group0: e.group, recursive: false, hasDir: list.some(x => x.dir) });
}
export const octal = (bits: boolean[][]) => bits.map(g => (g[0] ? 4 : 0) + (g[1] ? 2 : 0) + (g[2] ? 1 : 0)).join('');
export function setOctal(d: Record<string, any>, v: string) {
  if (!/^[0-7]{3}$/.test(v)) return;
  d.bits = [...v].map(c => [+c & 4, +c & 2, +c & 1].map(Boolean));
}
export async function applyChmod(d: Record<string, any>) {
  const p = panes[d.i];
  // Owner and group change only when edited: the dialog shows the first file's, others may differ.
  const owner = d.owner.trim() !== d.owner0 ? d.owner.trim() : '', group = d.group.trim() !== d.group0 ? d.group.trim() : '';
  if (d.busy) return;
  d.busy = true;
  try {
    await backend.fsChmod(p.src, p.user, d.names.map((n: string) => childOf(p.src, p.path, n)), parseInt(octal(d.bits), 8), owner, group, d.recursive);
  } catch (e) {
    // Part of the tree may have changed before the error.
    toast(errOf(e).title, 'bad');
    reload(d.i);
    return;
  } finally { d.busy = false; }
  toast(tr('Права {m} · {o}', { m: octal(d.bits), o: `${d.owner}:${d.group}` }) + (d.recursive ? tr(' · рекурсивно') : ''), 'ok');
  closeDlg(true);
  reload(d.i);
}
export function copyEntryPath(i: number, name?: string) {
  const p = panes[i];
  copyText(name ? childOf(p.src, p.path, name) : p.path);
  toast('Путь скопирован', 'ok', null, 1800);
}

/* ---------- Drag & drop ---------- */
export function dragStart(i: number, e: Entry, ev: DragEvent) {
  const p = panes[i];
  if (!p.sel.includes(e.name)) { p.sel = [e.name]; p.anchor = e.name; p.cur = e.name; }
  ui.dnd = { from: i, names: [...p.sel] };
  ev.dataTransfer!.effectAllowed = 'copyMove';
  ev.dataTransfer!.setData('text/plain', p.sel.map(n => childOf(p.src, p.path, n)).join('\n'));
}
export function dragEnd() { ui.dnd = null; panes.forEach(p => { p.dropping = false; p.dropRow = null; }); }
export function dragOver(i: number, ev: DragEvent) {
  const p = panes[i];
  if (p.state !== 'ok') return;
  const rowEl = (ev.target as HTMLElement).closest<HTMLElement>('.row');
  const name = rowEl?.dataset.name, e = name ? rowByName(i, name) : undefined;
  const own = !!ui.dnd && ui.dnd.from === i;
  const intoDir = name === '..' ? '..' : e && e.dir && !(own && ui.dnd!.names.includes(name!)) ? name! : null;
  if (own && !intoDir) { p.dropping = false; p.dropRow = null; ev.dataTransfer!.dropEffect = 'none'; return; }
  ev.preventDefault();
  ev.dataTransfer!.dropEffect = own || ev.shiftKey ? 'move' : 'copy';
  p.dropRow = intoDir; p.dropping = !intoDir;
}
export function dragLeave(i: number, ev: DragEvent) {
  if (!(ev.currentTarget as HTMLElement).contains(ev.relatedTarget as Node)) { panes[i].dropping = false; panes[i].dropRow = null; }
}
export function drop(i: number, ev: DragEvent) {
  const p = panes[i];
  const target = p.dropRow === '..' ? parentOf(p.src, p.path)! : p.dropRow ? childOf(p.src, p.path, p.dropRow) : p.path;
  const to = { src: p.src, user: p.user, path: target };
  p.dropping = false; p.dropRow = null;
  if (ui.dnd) {
    const f = panes[ui.dnd.from], names = new Set(ui.dnd.names);
    transfer({ src: f.src, user: f.user, path: f.path }, f.list.filter(e => names.has(e.name)), to, ui.dnd.from === i || ev.shiftKey);
    ui.dnd = null;
    return;
  }
  const files = [...(ev.dataTransfer?.files || [])];
  if (files.length) {
    const list: Entry[] = files.map(f => ({ name: f.name, dir: false, size: f.size || 1, mtime: f.lastModified || Date.now(), perm: '', owner: '', group: '' }));
    transfer({ src: 'ext', user: '', path: tr('Проводник') }, list, to, false);
  }
}
export function dropHint(i: number) {
  const p = panes[i], to = p.dropRow === '..' ? tr('родительскую папку') : p.dropRow || tr('эту папку');
  return `${tr(ui.dnd && ui.dnd.from === i ? 'Переместить в {to}' : 'Копировать в {to}', { to })} · ${tr('Shift: переместить')}`;
}

/* ---------- Core events ---------- */
export function initPanes() {
  backend.onFsChanged((src, path) => panes.forEach((p, i) => { if (p.state === 'ok' && p.src === src && p.path === path) reload(i); }));
  backend.onEditChanged(editChanged);
  backend.onSshStatus((server, user, state) => {
    // The core gave up reconnecting a working session: the pane shows the error with "Retry".
    if (state === 'bad') {
      panes.forEach(p => {
        if (p.src !== server || p.user !== user || p.state !== 'ok') return;
        ++p.token;
        p.state = 'error';
        p.msg = { title: tr('Соединение потеряно'), text: tr('{u}@{c}: переподключиться не удалось.', { u: user, c: label(server) }) };
      });
      return;
    }
    if (state) return;
    panes.forEach((p, i) => {
      if (p.src !== server || p.user !== user || p.state === 'off') return;
      paneSave(i);
      ++p.token;
      p.state = 'off'; p.msg = { title: tr('Отключено от {c}', { c: label(server) }) };
    });
  });
}
