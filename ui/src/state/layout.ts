/* Layout: view mode, resizable parts and the floating windows (log, queue). */
import { computed, nextTick, reactive, watch } from 'vue';
import { clamp } from '../util/format';
import { store, ui, type View } from './prefs';
import { closeMenu } from './overlay';
import { focusList, panes } from './panes';
import { focusTerm, openTerm, terms } from './terms';

export const VIEWS: Record<View, string> = { files: 'Только файлы', both: 'Файлы и терминал', term: 'Только терминал' };
export function setView(v: View) {
  ui.view = v;
  if (v !== 'files') {
    const p = panes[ui.focus];
    if (!terms.length) openTerm(p.src, p.user, p.state === 'ok' ? p.path : undefined);
    focusTerm();
  } else focusList(ui.focus);
}
export const toggleTerm = () => setView(ui.view === 'files' ? 'both' : 'files');

export function toggleSide() { ui.collapsed = !ui.collapsed; if (!ui.collapsed && ui.sideW < 184) ui.sideW = 232; }
export const sideWidth = computed(() => (ui.collapsed ? 96 : ui.sideW));
export function focusSearch() {
  ui.collapsed = false;
  nextTick(() => document.querySelector<HTMLInputElement>('.search input')?.focus());
}

function track(kind: 'side' | 'split' | 'term', mv: (e: MouseEvent) => void, done?: () => void) {
  ui.drag = kind;
  const up = () => { ui.drag = null; removeEventListener('mousemove', mv); removeEventListener('mouseup', up); done?.(); };
  addEventListener('mousemove', mv);
  addEventListener('mouseup', up);
}
export function startSideDrag(ev: MouseEvent) {
  const x0 = ev.clientX, w0 = sideWidth.value;
  track('side', e => {
    const w = w0 + e.clientX - x0;
    if (w < 150) ui.collapsed = true;
    else { ui.collapsed = false; ui.sideW = clamp(w, 184, 360); }
  });
}
// The share is stored, the pixel limit is enforced by the panes' CSS min-width when the window shrinks.
export function startSplitDrag(ev: MouseEvent) {
  closeMenu();
  const box = (ev.currentTarget as HTMLElement).closest('.panes')!.getBoundingClientRect(), edge = Math.min(280, box.width / 2) / box.width;
  const SNAP_PX = 10; // the centre holds the divider within this distance
  track('split', e => {
    const x = e.clientX - box.left;
    ui.split = Math.abs(x - box.width / 2) <= SNAP_PX ? 0.5 : clamp(x / box.width, edge, 1 - edge);
  });
}
export const termHeight = computed(() => (ui.view === 'files' ? 0 : ui.view === 'term' ? ui.stackH : Math.min(ui.termH, Math.max(120, ui.stackH - 140))));
export function startTermDrag(ev: MouseEvent) {
  const y0 = ev.clientY, h0 = termHeight.value;
  let want = h0;
  track('term', e => {
    want = h0 - (e.clientY - y0);
    if (ui.view === 'term' && want < ui.stackH - 40) ui.view = 'both';
    ui.termH = Math.max(120, Math.min(ui.stackH - 140, want));
  }, () => { if (want < 80) ui.view = 'files'; });
}

/* ---------- Floating windows ---------- */
export type WinKey = 'log' | 'queue';
export const WIN_EDGES = ['n', 's', 'e', 'w', 'ne', 'nw', 'se', 'sw'];
const WIN_MIN: Record<WinKey, { w: number; h: number }> = { log: { w: 300, h: 120 }, queue: { w: 340, h: 120 } };
// x/y stay null until the user moves something: the log docks bottom-right, the queue to its left.
type WinState = { x: number | null; y: number | null; w: number; h: number; z: number };
export const wins = reactive<Record<WinKey, WinState>>(store.get('wins', {
  log: { x: null, y: null, w: 480, h: 220, z: 56 },
  queue: { x: null, y: null, w: 360, h: 220, z: 55 },
}));
watch(wins, v => store.set('wins', v), { deep: true });

function winBox(k: WinKey): { x: number; y: number; w: number; h: number } {
  const s = wins[k], w = Math.min(s.w, ui.vw), h = Math.min(s.h, ui.vh);
  const log = k === 'queue' ? winBox('log') : null;
  const x = s.x ?? (log ? log.x - 8 - w : ui.vw - 8 - w);
  const y = s.y ?? (log ? log.y + log.h - h : ui.vh - 44 - h);
  return { x: clamp(x, 0, ui.vw - w), y: clamp(y, 0, ui.vh - h), w, h };
}
export function winStyle(k: WinKey) {
  const b = winBox(k);
  return { left: b.x + 'px', top: b.y + 'px', width: b.w + 'px', height: b.h + 'px', zIndex: wins[k].z };
}
export function winRaise(k: WinKey) { wins.log.z = wins.queue.z = 55; wins[k].z = 56; }
// Once the user touches one window, every open window keeps its place on its own.
function winFreeze() {
  for (const k of ['log', 'queue'] as WinKey[]) {
    if (!ui[k === 'log' ? 'logOpen' : 'queueOpen'] || wins[k].x !== null) continue;
    const { x, y } = winBox(k);
    wins[k].x = x; wins[k].y = y;
  }
}
function winTrack(ev: PointerEvent, onMove: (dx: number, dy: number) => void) {
  ev.preventDefault();
  winFreeze();
  document.body.classList.add('win-moving');
  document.body.style.cursor = getComputedStyle(ev.target as Element).cursor;
  const move = (e: PointerEvent) => onMove(e.clientX - ev.clientX, e.clientY - ev.clientY);
  const stop = () => {
    removeEventListener('pointermove', move);
    removeEventListener('pointerup', stop);
    removeEventListener('pointercancel', stop);
    document.body.classList.remove('win-moving');
    document.body.style.cursor = '';
  };
  addEventListener('pointermove', move);
  addEventListener('pointerup', stop);
  addEventListener('pointercancel', stop);
}
export function winDrag(k: WinKey, ev: PointerEvent) {
  if (ev.button || (ev.target as Element).closest('button')) return;
  const s = wins[k], b = winBox(k);
  winTrack(ev, (dx, dy) => { s.x = clamp(b.x + dx, 0, ui.vw - b.w); s.y = clamp(b.y + dy, 0, ui.vh - b.h); });
}
export function winResize(k: WinKey, edge: string, ev: PointerEvent) {
  if (ev.button) return;
  const min = WIN_MIN[k], s = wins[k], b = winBox(k), right = b.x + b.w, bottom = b.y + b.h;
  winTrack(ev, (dx, dy) => {
    if (edge.includes('e')) s.w = clamp(b.w + dx, min.w, ui.vw - b.x);
    if (edge.includes('w')) { s.w = clamp(b.w - dx, min.w, right); s.x = right - s.w; }
    if (edge.includes('s')) s.h = clamp(b.h + dy, min.h, ui.vh - b.y);
    if (edge.includes('n')) { s.h = clamp(b.h - dy, min.h, bottom); s.y = bottom - s.h; }
  });
}
