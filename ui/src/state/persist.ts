/*
 * "Nothing gets lost": what is kept between launches and when it is written.
 *
 * settings.json  settings, theme, view, widths, columns, sidebar groups, floating window places; on change
 * session.json   panes, pane memory, recent places, terminals, unfinished transfers, last log rows;
 *                every 2 s when something changed, and on close
 * terms/<id>     terminal history, only with "save terminal history"; every 5 s when changed, and on close
 *
 * Nothing secret goes in: passwords live in the OS keychain, a line typed at a password prompt is dropped.
 */
import { watch } from 'vue';
import { backend, type LogRow, type Src, type TransferItem } from '../backend';
import { tr } from '../i18n';
import { settings, ui, UI_KEPT, type Settings } from './prefs';
import { wins, type WinKey, type WinState } from './layout';
import { log } from './log';
import { paneMemory, panes, setPaneMemory, setSource, type PaneMem } from './panes';
import { queue } from './queue';
import { recentSnapshot, restoreRecent, srvBy, type RecentState } from './servers';
import { restoreTerms, saveHistory, termsSnapshot, type SavedTerm } from './terms';
import { isLocal } from '../util/paths';
import { sleep } from '../util/format';

const VERSION = 1;
const LOG_KEEP = 300;

type KeptUi = Pick<typeof ui, (typeof UI_KEPT)[number]>;
interface SettingsFile { v: number; settings: Partial<Settings>; ui: Partial<KeptUi>; wins: Partial<Record<WinKey, WinState>> }

export interface SessionFile extends RecentState {
  v: number;
  panes: { src: Src; user: string; path: string }[];
  focus: number;
  mem: PaneMem[];
  open: { queue: boolean; log: boolean; logOnlyErr: boolean };
  terms: SavedTerm[];
  term: number | null;
  queue: TransferItem[];
  log: LogRow[];
}

const isObj = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v);

/* ---------- Settings ---------- */
const plain = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
function settingsFile(): SettingsFile {
  const kept = Object.fromEntries(UI_KEPT.map(k => [k, ui[k]])) as Partial<KeptUi>;
  return plain({ v: VERSION, settings, ui: kept, wins });
}

/** Applies settings.json: only known keys of the right type, so an old or hand-edited file cannot break the UI. */
function applySettings(raw: unknown) {
  if (!isObj(raw) || raw.v !== VERSION) return;
  const take = (into: Record<string, unknown>, from: unknown) => {
    if (!isObj(from)) return;
    for (const [k, v] of Object.entries(from)) {
      const cur = into[k];
      if (cur === undefined || v === null || typeof v !== typeof cur) continue;
      into[k] = isObj(cur) ? { ...cur, ...(v as object) } : v;
    }
  };
  take(settings as unknown as Record<string, unknown>, raw.settings);
  const kept: Record<string, unknown> = {};
  if (isObj(raw.ui)) for (const k of UI_KEPT) if (k in raw.ui) kept[k] = raw.ui[k];
  take(ui as unknown as Record<string, unknown>, kept);
  if (isObj(raw.wins)) for (const k of ['log', 'queue'] as WinKey[]) take(wins[k] as unknown as Record<string, unknown>, raw.wins[k]);
}

/** Reads both files before the first paint: the theme and the layout must be right from the first frame. */
export async function loadState(): Promise<SessionFile | null> {
  const [s, session] = await Promise.all([
    backend.stateLoad('settings').catch(() => null),
    backend.stateLoad('session').catch(() => null),
  ]);
  applySettings(s);
  return isObj(session) && session.v === VERSION ? (session as unknown as SessionFile) : null;
}

/* ---------- Session ---------- */
function sessionFile(): SessionFile {
  return {
    v: VERSION,
    panes: panes.map(p => ({ src: p.src, user: p.user, path: p.path })),
    focus: ui.focus,
    mem: paneMemory(),
    ...recentSnapshot(),
    open: { queue: ui.queueOpen, log: ui.logOpen, logOnlyErr: ui.logOnlyErr },
    terms: termsSnapshot(),
    term: ui.term,
    queue: queue.value.filter(q => q.state !== 'done'),
    log: log.slice(-LOG_KEEP).map(r => ({ ...r })),
  };
}

/** Log rows from the last run go first; ids turn negative so they never meet the new rows' ids. */
export function restoreLog(s: SessionFile) {
  const rows = (s.log ?? []).map((r, k, all) => ({
    ...r, id: k - all.length, ...(r.state === 'run' ? { state: 'ok' as const, note: tr('прервано') } : {}),
  }));
  log.unshift(...rows);
}

/** Panes, terminals, open windows and the queue. Connections come up on their own; nothing transfers by itself. */
export async function restoreSession(s: SessionFile) {
  setPaneMemory(s.mem ?? []);
  restoreRecent(s);
  ui.queueOpen = !!s.open?.queue;
  ui.logOpen = !!s.open?.log;
  ui.logOnlyErr = !!s.open?.logOnlyErr;
  if (s.queue?.length) backend.transferRestore(s.queue);
  ui.focus = s.focus === 1 ? 1 : 0;
  const opened = Promise.all(panes.map((_, i) => {
    const p = s.panes?.[i];
    // A removed server sends its pane to This PC.
    if (!p || (!isLocal(p.src) && !srvBy(p.src))) return setSource(i, 'local');
    return setSource(i, p.src, p.user || undefined, p.path || undefined);
  }));
  await restoreTerms(s.terms ?? [], s.term ?? null);
  // A server that takes long to answer must not keep the session unsaved for long.
  await Promise.race([opened, sleep(10_000)]);
}

/* ---------- Writing ---------- */
let lastSession = '';
let started = false;
let settingsTimer = 0;

async function saveSession() {
  const json = JSON.stringify(sessionFile());
  if (json === lastSession) return;
  lastSession = json;
  await backend.stateSave('session', JSON.parse(json)).catch(e => { lastSession = ''; console.error('session save failed', e); });
}
async function saveSettings() {
  clearTimeout(settingsTimer);
  settingsTimer = 0;
  await backend.stateSave('settings', settingsFile()).catch(e => console.error('settings save failed', e));
}

/** Starts writing after the restore is done, so a half-restored state never overwrites the last good one. */
export function startPersist() {
  if (started) return;
  started = true;
  const later = () => { clearTimeout(settingsTimer); settingsTimer = window.setTimeout(saveSettings, 300); };
  watch(settings, later, { deep: true });
  watch(() => UI_KEPT.map(k => ui[k]), later, { deep: true });
  watch(wins, later, { deep: true });
  setInterval(saveSession, 2000);
  setInterval(saveHistory, 5000);
  addEventListener('pagehide', () => { void flush(); });
}

/** Everything pending goes to disk now: the window waits for it before it closes. */
export async function flush() {
  if (!started) return;
  await Promise.all([saveSession(), saveSettings(), saveHistory()]);
}
