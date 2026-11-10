/* Terminal tabs. Each tab is an xterm.js instance bound to a PTY in the core. */
import { computed, nextTick, reactive, watch } from 'vue';
import { Terminal, type ITheme } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import { backend, isBackendError, type Src } from '../backend';
import { tr } from '../i18n';
import { copyText } from '../util/format';
import { childOf, isLocal, parentOf } from '../util/paths';
import { settings, theme, ui } from './prefs';
import { toast } from './overlay';
import { label, srvBy } from './servers';
import { panes } from './panes';

export interface Term {
  id: number;
  src: Src;
  user: string;
  cwd: string;
  pty: number | null;
  dead: boolean;
  busy: boolean;
}
interface View {
  xt: Terminal;
  fit: FitAddon;
  el: HTMLDivElement;
  opened: boolean;
}

export const terms = reactive<Term[]>([]);
const views = new Map<number, View>();
let termId = 0;
export const curTerm = computed(() => terms.find(t => t.id === ui.term) || null);
export const termTitle = (t: Term) => (isLocal(t.src) ? shellName() : `${t.user}@${label(t.src)}`);
const shellName = () => (settings.shell === 'cmd' ? 'cmd' : settings.shell === 'powershell' ? 'Windows PowerShell' : 'PowerShell');

/** Keys the app handles even when the terminal has focus: xterm must not send them to the shell. */
export function appKey(e: KeyboardEvent): boolean {
  const ctrl = e.ctrlKey || e.metaKey;
  if (ctrl && !e.shiftKey && ['KeyK', 'KeyP', 'KeyB', 'Comma', 'Backquote'].includes(e.code)) return true;
  if ((ctrl || e.altKey) && !e.shiftKey && ['Digit1', 'Digit2', 'Digit3'].includes(e.code)) return true;
  return false;
}

/* ---------- Look ---------- */
function xtTheme(): ITheme {
  const el = document.querySelector('.tpanel') ?? document.documentElement, cs = getComputedStyle(el);
  const v = (n: string) => cs.getPropertyValue(n).trim();
  const g = v('--t-g'), b = v('--t-b'), y = v('--t-y'), r = v('--t-r'), m = v('--t-m'), tx = v('--t-tx');
  return {
    background: v('--term'), foreground: tx, cursor: tx, cursorAccent: v('--term'),
    selectionBackground: 'rgba(127,176,255,.32)',
    black: '#1b1c1f', red: r, green: g, yellow: y, blue: b, magenta: '#c49cf0', cyan: '#6fc8d1', white: tx,
    brightBlack: m, brightRed: r, brightGreen: g, brightYellow: y, brightBlue: b, brightMagenta: '#d4b4f5', brightCyan: '#8fd8df', brightWhite: '#ffffff',
  };
}
const MONO = "'JetBrains Mono Variable', 'Cascadia Mono', Consolas, monospace";

function makeView(t: Term): View {
  const xt = new Terminal({
    fontFamily: MONO, fontSize: settings.termFont, lineHeight: 1.2, cursorBlink: true, cursorStyle: 'bar',
    scrollback: 5000, allowProposedApi: false, theme: xtTheme(), drawBoldTextInBrightColors: false,
    macOptionIsMeta: true, rightClickSelectsWord: false,
  });
  const fit = new FitAddon();
  xt.loadAddon(fit);
  const el = document.createElement('div');
  el.className = 'xterm-host';
  xt.attachCustomKeyEventHandler(e => {
    if (e.type !== 'keydown') return true;
    if (appKey(e)) return false;
    // Ctrl C copies when something is selected, otherwise it is an interrupt for the shell.
    if ((e.ctrlKey || e.metaKey) && e.code === 'KeyC' && xt.hasSelection()) {
      copyText(xt.getSelection()); xt.clearSelection(); e.preventDefault(); return false;
    }
    return true;
  });
  xt.onData(d => input(t, d));
  // Selecting text copies it at once, as in PuTTY/KiTTY.
  el.addEventListener('mouseup', ev => {
    if (ev.button === 0 && settings.copyOnSelect && xt.hasSelection()) copyText(xt.getSelection());
  });
  // Right click pastes, as in PuTTY/KiTTY. xterm wraps it in bracketed paste when the shell asked for it.
  el.addEventListener('contextmenu', async ev => {
    ev.preventDefault();
    if (t.dead || t.busy) return;
    let text: string;
    try { text = await navigator.clipboard.readText(); } catch { toast('Нет доступа к буферу', 'bad'); return; }
    if (text) xt.paste(text);
    xt.focus();
  });
  return { xt, fit, el, opened: false };
}

/** Opens the instance in its host the first time it becomes visible: xterm measures the font on open. */
export function attach(container: HTMLElement, t: Term | null) {
  container.replaceChildren();
  if (!t) return;
  const v = views.get(t.id);
  if (!v) return;
  container.appendChild(v.el);
  if (!v.opened) {
    v.xt.open(v.el);
    v.opened = true;
    try {
      const gl = new WebglAddon();
      gl.onContextLoss(() => gl.dispose()); // falls back to the DOM renderer
      v.xt.loadAddon(gl);
    } catch { /* no WebGL: the DOM renderer is fine */ }
  }
  fitTerm(t);
}
export function fitTerm(t: Term | null = curTerm.value) {
  const v = t && views.get(t.id);
  if (!t || !v?.opened || v.el.clientHeight < 20) return;
  v.fit.fit();
  if (t.pty) backend.ptyResize(t.pty, v.xt.cols, v.xt.rows);
}

/* ---------- Lifecycle ---------- */
const DIM = (s: string) => `\x1b[90m${s}\x1b[0m\r\n`, RED = (s: string) => `\x1b[31m${s}\x1b[0m\r\n`;

function input(t: Term, d: string) {
  if (t.dead) {
    if (d === '\r' && !isLocal(t.src)) connect(t);
    return;
  }
  if (t.pty && !t.busy) backend.ptyWrite(t.pty, d);
}

async function connect(t: Term, quiet = false) {
  const v = views.get(t.id)!;
  t.busy = true; t.dead = false;
  if (!isLocal(t.src)) {
    const s = srvBy(t.src);
    if (!s) { v.xt.write(RED(tr('Сервер удалён из списка'))); t.dead = true; t.busy = false; return; }
    v.xt.write(DIM(tr('Подключение к {who} ({host})…', { who: `${t.user}@${s.code}`, host: s.host })));
  }
  try {
    t.pty = await backend.ptyOpen(t.src, t.user, t.cwd, v.xt.cols, v.xt.rows, d => v.xt.write(d));
  } catch (e) {
    const err = isBackendError(e) ? e : { title: String(e), text: '' };
    v.xt.write(RED(`${err.title}.${err.text ? ' ' + err.text : ''}`) + DIM(tr('Enter: подключиться снова')));
    t.busy = false; t.dead = true; t.pty = null;
    if (!quiet) focusTerm();
    return;
  }
  t.busy = false;
  fitTerm(t);
  if (!quiet) focusTerm();
}

export async function openTerm(src: Src, user?: string, cwd?: string, quiet = false): Promise<Term | null> {
  if (ui.view === 'files') ui.view = 'both';
  let start = cwd ?? '';
  if (!isLocal(src)) {
    const s = srvBy(src);
    if (!s) return null;
    user = user || s.defaultUser;
    start = cwd || s.startDir || '';
  }
  const t = reactive<Term>({ id: ++termId, src, user: user ?? '', cwd: start, pty: null, dead: false, busy: true });
  views.set(t.id, makeView(t));
  terms.push(t);
  ui.term = t.id;
  await connect(t, quiet);
  return t;
}
export function closeTerm(id: number) {
  const k = terms.findIndex(t => t.id === id);
  if (k < 0) return;
  const [t] = terms.splice(k, 1);
  if (t.pty) backend.ptyClose(t.pty);
  views.get(id)?.xt.dispose();
  views.delete(id);
  if (ui.term === id) ui.term = (terms[k] || terms[k - 1])?.id ?? null;
  if (ui.term) focusTerm();
}
export function selectTerm(id: number) { ui.term = id; focusTerm(); }
export function focusTerm() { nextTick(() => setTimeout(() => { const t = curTerm.value; if (t) views.get(t.id)?.xt.focus(); }, 30)); }
export function newTermHere() { const p = panes[ui.focus]; openTerm(p.src, p.user, p.state === 'ok' ? p.path : undefined); }
export function termHere(i: number, name?: string) {
  const p = panes[i];
  openTerm(p.src, p.user, name ? childOf(p.src, p.path, name) : p.path);
}

export async function initTerms(demo: boolean) {
  backend.onPtyExit((pty, reason) => {
    const t = terms.find(x => x.pty === pty);
    if (!t) return;
    t.pty = null;
    if (reason === 'exit') { closeTerm(t.id); return; }
    t.dead = true;
    views.get(t.id)?.xt.write('\r\n' + RED(tr('Соединение с {c} закрыто', { c: label(t.src) })) + DIM(tr('Enter: подключиться снова')));
  });
  watch(() => settings.termFont, n => { for (const [, v] of views) v.xt.options.fontSize = n; nextTick(() => fitTerm()); });
  watch(theme, () => nextTick(() => { const th = xtTheme(); for (const [, v] of views) v.xt.options.theme = th; }));
  await document.fonts.load(`${settings.termFont}px "JetBrains Mono Variable"`).catch(() => undefined);
  if (!demo) return;
  // Demo: two terminals with some history, like after a working day.
  const web = panes[1].src !== 'local' ? panes[1] : null;
  const a = web ? await openTerm(web.src, 'root', web.path, true) : null;
  const b = await openTerm('local', '', parentOf('local', panes[0].path) ?? undefined, true);
  if (a?.pty) { backend.ptyWrite(a.pty, 'uptime\r'); backend.ptyWrite(a.pty, 'df -h\r'); }
  if (b?.pty) backend.ptyWrite(b.pty, 'npm run build\r');
  if (a) ui.term = a.id;
}
