/* Terminal tabs. Each tab is an xterm.js instance bound to a PTY in the core. */
import { computed, nextTick, reactive, watch } from 'vue';
import { Terminal, type ITheme } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import { SerializeAddon } from '@xterm/addon-serialize';
import { backend, isBackendError, type PtyExit, type Src } from '../backend';
import { tr } from '../i18n';
import { copyText, sleep } from '../util/format';
import { childOf, isLocal, LOCAL_WIN, parentOf } from '../util/paths';
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
  serial: SerializeAddon;
  el: HTMLDivElement;
  opened: boolean;
  /** The command typed but not sent yet; null once the shell edits the line itself (arrows, Tab, history). */
  line: string | null;
  /** The shell asked for a password: what is typed now is never remembered. */
  secret: boolean;
  /** Output since the last history save. */
  dirty: boolean;
  lastOut: number;
  /** Output handed to xterm and not parsed yet; above the high mark the core pauses. */
  pending: number;
  paused: boolean;
  /** The end of the output as text across chunks, for the password prompt check. */
  tail: string;
}
/** A tab as session.json keeps it. */
export interface SavedTerm { id: number; src: Src; user: string; cwd: string; input: string; dead: boolean }
/** Saved history per tab, as the setting caps it. */
const HISTORY_LINES = 1000;

export const terms = reactive<Term[]>([]);
const views = new Map<number, View>();
let termId = 0;
/** A local tab was open during this run: the next start keeps a shell ready (`prewarmLocal`). */
let localUsed = false;
export const usedLocal = () => localUsed;
export const curTerm = computed(() => terms.find(t => t.id === ui.term) || null);
export const termTitle = (t: Term) => (isLocal(t.src) ? shellName() : `${t.user}@${label(t.src)}`);
export const shellName = () => (settings.shell === 'cmd' ? 'cmd' : settings.shell === 'powershell' ? 'Windows PowerShell' : 'PowerShell');

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
    scrollback: HISTORY_LINES, allowProposedApi: false, theme: xtTheme(), drawBoldTextInBrightColors: false,
    macOptionIsMeta: true, rightClickSelectsWord: false,
  });
  const fit = new FitAddon(), serial = new SerializeAddon();
  xt.loadAddon(fit);
  xt.loadAddon(serial);
  // OSC 7 reports the shell's folder: a reconnect and the next launch start there.
  xt.parser.registerOscHandler(7, data => { const p = osc7Path(t, data); if (p) t.cwd = p; return true; });
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
  return { xt, fit, serial, el, opened: false, line: '', secret: false, dirty: false, lastOut: 0, pending: 0, paused: false, tail: '' };
}

/**
 * `file://host/path` from OSC 7. A local tab takes only local folders: after `ssh` inside it the
 * remote shell may report its own, which is not where this tab starts next time.
 */
function osc7Path(t: Term, data: string): string | null {
  const m = data.match(/^file:\/\/([^/]*)(\/.*)$/);
  if (!m) return null;
  let path: string;
  try { path = decodeURIComponent(m[2]); } catch { return null; }
  if (!isLocal(t.src)) return path;
  if (LOCAL_WIN) return /^\/[A-Za-z]:\//.test(path) ? path.slice(1).replace(/\//g, '\\') : null;
  return !m[1] || m[1] === 'localhost' ? path : null;
}

/** Follows what is typed, so an unsent command survives a restart. Best effort: unknown edits give up. */
function trackLine(v: View, d: string) {
  // Bracketed paste markers, and xterm's own replies (cursor position for ConPTY, device attributes, focus).
  d = d.replace(/\x1b\[20[01]~|\x1b\[\d+;\d+R|\x1b\[[?>][\d;]*c|\x1b\[[IO]/g, '');
  const k = d.lastIndexOf('\r');
  if (k >= 0) { v.line = ''; v.secret = false; d = d.slice(k + 1); }
  if (!d || v.secret || v.line === null) return;
  if (d === '\x7f' || d === '\b') v.line = [...v.line].slice(0, -1).join('');
  else if (d === '\x03' || d === '\x15') v.line = '';
  else if (/[\x00-\x1f\x7f]/.test(d)) v.line = null;
  else v.line += d;
}
/** Matched on the last line of the output with escape sequences cut out: ConPTY adds some after the prompt. */
const ASKS_SECRET = /(password|passphrase|пароль)[^\r\n]*:[ \t]*$/i;
const ESCAPES = /\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|\x1b[=>78]/g;
const TAIL = 512;
/** Flow control after xterm's guide: pause the core above the high mark, resume below the low one. */
const HIGH_MARK = 3 << 20, LOW_MARK = 1536 << 10;
const utf8 = new TextDecoder();
function output(t: Term, v: View, d: string | Uint8Array) {
  const n = d.length;
  v.pending += n;
  v.xt.write(d, () => {
    v.pending -= n;
    if (v.paused && v.pending < LOW_MARK) { v.paused = false; if (t.pty) backend.ptyPause(t.pty, false); }
  });
  if (!v.paused && v.pending > HIGH_MARK && t.pty) { v.paused = true; backend.ptyPause(t.pty, true); }
  v.dirty = true;
  v.lastOut = performance.now();
  // A character cut at the start of the slice decodes as U+FFFD, which is fine here.
  const text = typeof d === 'string' ? d : utf8.decode(d.length > TAIL ? d.subarray(-TAIL) : d);
  v.tail = (text.length >= TAIL ? text : v.tail + text).slice(-TAIL);
  if (ASKS_SECRET.test(v.tail.replace(ESCAPES, ''))) { v.secret = true; v.line = ''; }
}
/** The shell is ready once it has printed something and then stayed quiet for a moment. */
async function shellReady(v: View) {
  const until = performance.now() + 5000;
  while (performance.now() < until && (!v.lastOut || performance.now() - v.lastOut < 250)) await sleep(100);
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
/**
 * What a program that died mid-screen (vim, htop, mc, tmux) left on: the alternate screen, mouse reports,
 * focus events, bracketed paste, application keys, a hidden cursor, colors. The next shell needs none of it.
 */
const RESET_MODES = '\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?1l\x1b>\x1b[?25h\x1b[0m';
const DIM = (s: string) => `\x1b[90m${s}\x1b[0m\r\n`, RED = (s: string) => `\x1b[31m${s}\x1b[0m\r\n`;

/** A shell that ends sooner than this after start (a bad setting, a broken profile) keeps its tab to show why. */
const QUICK_EXIT_MS = 1000;

function input(t: Term, d: string) {
  if (t.dead) {
    if (d === '\r') connect(t);
    return;
  }
  if (!t.pty || t.busy) return;
  trackLine(views.get(t.id)!, d);
  backend.ptyWrite(t.pty, d);
}

async function connect(t: Term, quiet = false): Promise<boolean> {
  const v = views.get(t.id)!;
  t.busy = true; t.dead = false;
  if (!isLocal(t.src)) {
    const s = srvBy(t.src);
    if (!s) { v.xt.write(RED(tr('Сервер удалён из списка'))); t.dead = true; t.busy = false; return false; }
    v.xt.write(DIM(tr('Подключение к {who} ({host})…', { who: `${t.user}@${s.code}`, host: s.host })));
  }
  v.line = ''; v.secret = false; v.lastOut = 0; v.tail = '';
  // The shell and its profile (motd, tmux) should see the real size, not xterm's 80×24 before it is laid out.
  // The panel attaches a new tab a tick after it becomes current; a frame covers that (the timer: a hidden window has no frames).
  await Promise.race([new Promise(requestAnimationFrame), sleep(50)]);
  if (v.opened && v.el.clientHeight >= 20) v.fit.fit();
  let started = 0, over = false;
  try {
    // Output and the exit wait until the id is here, so `started` is set by then.
    const id = await backend.ptyOpen(t.src, t.user, t.cwd, settings.shell, v.xt.cols, v.xt.rows,
      d => { if (!over && views.has(t.id)) output(t, v, d); },
      reason => { over = true; exited(t, reason, performance.now() - started); });
    // Closed while connecting: nobody shows this shell.
    if (!views.has(t.id)) { backend.ptyClose(id); return false; }
    t.pty = id;
    started = performance.now();
  } catch (e) {
    if (!views.has(t.id)) return false;
    const err = isBackendError(e) ? e : { title: String(e), text: '' };
    v.xt.write(RED(`${err.title}.${err.text ? ' ' + err.text : ''}`) + DIM(tr('Enter: подключиться снова')));
    t.busy = false; t.dead = true; t.pty = null;
    if (!quiet) focusTerm();
    return false;
  }
  t.busy = false;
  fitTerm(t);
  if (!quiet) focusTerm();
  return true;
}

function exited(t: Term, reason: PtyExit, age: number) {
  const v = views.get(t.id);
  if (!v) return;
  t.pty = null;
  if (reason === 'exit' && age >= QUICK_EXIT_MS) { closeTerm(t.id); return; }
  t.dead = true;
  const why = reason === 'refused' ? tr('Сервер отказал в терминале')
    : reason === 'killed' ? tr('Оболочка завершена сигналом')
    : reason === 'exit' ? tr('Оболочка завершилась сразу после запуска')
    : tr('Соединение с {c} закрыто', { c: label(t.src) });
  // Leaving the alternate screen restores the cursor even when it is not active: on a plain prompt
  // that would move the red line to the top of the screen. Checked once the pending output is parsed.
  v.xt.write('', () => {
    const leave = v.xt.buffer.active.type === 'alternate' ? '\x1b[?1049l' : '';
    v.xt.write(leave + RESET_MODES + '\r\n' + RED(why) + DIM(tr('Enter: подключиться снова')));
  });
}

export async function openTerm(src: Src, user?: string, cwd?: string, quiet = false): Promise<Term | null> {
  if (ui.view === 'files') ui.view = 'both';
  let start = cwd ?? '';
  if (isLocal(src)) localUsed = true;
  else {
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
  pruneHistory();
}
/**
 * pwsh takes a few hundred ms to its first prompt: the core starts the shell the first local tab
 * would get (the local pane's folder, as in `openTerm`), and that tab takes it ready.
 */
export function prewarmLocal() {
  if (terms.some(t => isLocal(t.src))) return;
  const p = panes.find(p => isLocal(p.src));
  backend.ptyPrewarm(p && p.state === 'ok' ? p.path : '', settings.shell);
}
export function selectTerm(id: number) { ui.term = id; focusTerm(); }
export function focusTerm() { nextTick(() => setTimeout(() => { const t = curTerm.value; if (t) views.get(t.id)?.xt.focus(); }, 30)); }
export function newTermHere() { const p = panes[ui.focus]; openTerm(p.src, p.user, p.state === 'ok' ? p.path : undefined); }
export function termHere(i: number, name?: string) {
  const p = panes[i];
  openTerm(p.src, p.user, name ? childOf(p.src, p.path, name) : p.path);
}

/* ---------- Session: tabs, unsent command, folder, history ---------- */
export function termsSnapshot(): SavedTerm[] {
  return terms.map(t => {
    const v = views.get(t.id);
    return { id: t.id, src: t.src, user: t.user, cwd: t.cwd, input: (!v?.secret && v?.line) || '', dead: t.dead };
  });
}

/** Tabs come back in order; every live one opens a new session, the unsent command waits for the prompt. */
export async function restoreTerms(saved: SavedTerm[], active: number | null) {
  const keepHistory = settings.saveTermHistory;
  const history = await Promise.all(saved.map(s => (keepHistory ? backend.termLoad(s.id).catch(() => null) : null)));
  saved.forEach((s, k) => {
    const t = reactive<Term>({ id: s.id, src: s.src, user: s.user, cwd: s.cwd, pty: null, dead: s.dead, busy: !s.dead });
    termId = Math.max(termId, s.id);
    const v = makeView(t);
    views.set(t.id, v);
    terms.push(t);
    const hist = history[k];
    if (hist) v.xt.write(hist + '\r\n');
    if (s.dead) { v.xt.write(DIM(tr('Enter: подключиться снова'))); return; }
    v.xt.write(DIM(tr(hist ? 'Новая сессия, история выше' : 'Новая сессия')));
    void connect(t, true).then(async ok => {
      if (!ok || !s.input) return;
      await shellReady(v);
      if (t.pty && !t.dead) input(t, s.input);
    });
  });
  ui.term = terms.some(t => t.id === active) ? active : terms[0]?.id ?? null;
  pruneHistory();
}

/** Saves the history of tabs with new output; with the setting off nothing is written. */
export async function saveHistory() {
  if (!settings.saveTermHistory) return;
  const jobs: Promise<void>[] = [];
  for (const [id, v] of views) {
    if (!v.dirty) continue;
    v.dirty = false;
    jobs.push(backend.termSave(id, v.serial.serialize({ scrollback: HISTORY_LINES })).catch(() => { v.dirty = true; }));
  }
  await Promise.all(jobs);
}
function pruneHistory() {
  backend.termPrune(settings.saveTermHistory ? terms.map(t => t.id) : []).catch(() => undefined);
}

export async function initTerms(demo: boolean) {
  watch(() => settings.termFont, n => { for (const [, v] of views) v.xt.options.fontSize = n; nextTick(() => fitTerm()); });
  watch(theme, () => nextTick(() => { const th = xtTheme(); for (const [, v] of views) v.xt.options.theme = th; }));
  // Turning history on saves what is on screen now; turning it off deletes the files.
  watch(() => settings.saveTermHistory, on => {
    if (on) for (const [, v] of views) v.dirty = true;
    pruneHistory();
  });
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
