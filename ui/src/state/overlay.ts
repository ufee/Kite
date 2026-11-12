/* Toasts, dialogs and menus: everything that floats above the workspace. */
import { computed, nextTick, reactive, shallowReactive } from 'vue';
import { tr } from '../i18n';

/* ---------- Toasts ---------- */
export interface Toast {
  id: number;
  text: string;
  kind: 'ok' | 'bad' | 'info';
  action: { label: string; run: () => void } | null;
}
export const toasts = reactive<Toast[]>([]);
let toastId = 0;

export function toast(text: string, kind: Toast['kind'] = 'ok', action: Toast['action'] = null, ms = 3400) {
  const t: Toast = { id: ++toastId, text: tr(text), kind, action: action && { ...action, label: tr(action.label) } };
  toasts.push(t);
  if (toasts.length > 3) toasts.shift();
  setTimeout(() => dropToast(t.id), ms);
}
export function dropToast(id: number) {
  const k = toasts.findIndex(t => t.id === id);
  if (k >= 0) toasts.splice(k, 1);
}

/* ---------- Dialogs ---------- */
/*
 * A stack: the top one is visible, the ones below wait. A host key request that arrives
 * while another one is shown goes under it, so requests are answered in arrival order.
 */
export type DlgKind = 'confirm' | 'conflict' | 'hostkey' | 'server' | 'import' | 'chmod' | 'settings' | 'dest' | 'file' | 'keys' | 'passphrase';
interface Dlg {
  id: number;
  kind: DlgKind;
  data: Record<string, any>;
  resolve: (v: unknown) => void;
}
const stack = shallowReactive<Dlg[]>([]);
let dlgId = 0;

export const dlg = computed(() => stack[stack.length - 1] ?? null);
export const dlgOpen = computed(() => stack.length > 0);

export function ask<T = unknown>(kind: DlgKind, data: Record<string, any> = {}): Promise<T | null> {
  closeMenu();
  return new Promise(resolve => {
    const d: Dlg = { id: ++dlgId, kind, data: reactive(data), resolve: resolve as (v: unknown) => void };
    let at = stack.length;
    if (kind === 'hostkey') while (at > 0 && stack[at - 1].kind === 'hostkey') at--;
    if (kind === 'hostkey' && at < stack.length) stack.splice(at, 0, d);
    else stack.push(d);
  });
}
/** Closes the top dialog (or the one given) with a result. */
export function closeDlg(result: unknown = null, which: Dlg | null = dlg.value) {
  if (!which) return;
  const k = stack.indexOf(which);
  if (k >= 0) stack.splice(k, 1);
  which.resolve(result);
}
/** The open dialog that renders this data, wherever it is in the stack. */
export const dlgOf = (data: Record<string, any>) => stack.find(d => d.data === data) ?? null;
/** Closes every dialog of a kind, used when what they ask about is gone. */
export function dropDialogs(kind: DlgKind, match: (data: Record<string, any>) => boolean) {
  for (const d of [...stack]) if (d.kind === kind && match(d.data)) closeDlg(null, d);
}

export const confirmBox = (title: string, text: string, ok: string, danger = false) =>
  ask<boolean>('confirm', { title: tr(title), text: tr(text), ok: tr(ok), danger });
/** Two answers besides "cancel": `ok` is the main one. */
export const choiceBox = (title: string, text: string, ok: string, alt: string) =>
  ask<'ok' | 'alt'>('confirm', { title: tr(title), text: tr(text), ok: tr(ok), alt: tr(alt) });

/* ---------- Menus ---------- */
export type MenuKey = 'src' | 'user' | 'cols' | 'row' | 'blank' | 'srv' | 'sort';
export const menu = reactive({ key: null as MenuKey | null, x: 0, y: 0, data: {} as Record<string, any>, id: '', ready: false });
let lastClosed = { key: '', t: 0 };

export function openMenu(key: MenuKey, ev: MouseEvent, data: Record<string, any> = {}, align: 'left' | 'right' = 'left') {
  const id = key + JSON.stringify(data);
  if (lastClosed.key === id && Date.now() - lastClosed.t < 250) return;
  let ax = ev.clientX, ay = ev.clientY, top = ev.clientY;
  const ctx = ev.type === 'contextmenu';
  if (!ctx) {
    const r = (ev.currentTarget as HTMLElement).getBoundingClientRect();
    ax = align === 'right' ? r.right : r.left; ay = r.bottom + 4; top = r.top;
  }
  Object.assign(menu, { key, data, id, x: ax, y: ay, ready: false });
  nextTick(() => {
    const el = document.querySelector<HTMLElement>('.menu');
    if (!el) return;
    const w = el.offsetWidth, h = el.offsetHeight;
    let x = align === 'right' && !ctx ? ax - w : ax, y = ay;
    x = Math.max(8, Math.min(x, innerWidth - w - 8));
    if (y + h > innerHeight - 8) y = ctx ? innerHeight - h - 8 : Math.max(8, top - h - 4);
    menu.x = x; menu.y = y; menu.ready = true;
  });
}
export function closeMenu() {
  if (menu.key) lastClosed = { key: menu.id, t: Date.now() };
  menu.key = null;
}
export function act(fn: () => unknown) { closeMenu(); fn(); }
