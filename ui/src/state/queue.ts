/* Transfer queue: the core runs it, the UI mirrors its state and reports batches. */
import { computed, shallowRef } from 'vue';
import { backend, isBackendError, type ConflictPolicy, type Endpoint, type Entry, type ServerId, type TransferItem } from '../backend';
import { tr } from '../i18n';
import { objs } from '../util/format';
import { childOf, isLocal, LOCAL_WIN, within } from '../util/paths';
import { settings, ui } from './prefs';
import { ask, toast } from './overlay';
import { label } from './servers';
import { waveBusy } from './log';

/** Replaced whole on every update from the core: no deep reactivity at five updates a second. */
export const queue = shallowRef<TransferItem[]>([]);
const ACTIVE = ['run', 'count', 'wait', 'pause'];
export const active = computed(() => queue.value.filter(q => ACTIVE.includes(q.state)));
export const qErr = computed(() => queue.value.filter(q => q.state === 'err'));
/** Unfinished transfers with a server on either end; `running` leaves out the paused ones. */
export const transfersOn = (id: ServerId, running = false) =>
  active.value.filter(q => (q.from.src === id || q.to.src === id) && !(running && q.state === 'pause')).length;
export const qStat = computed(() => {
  const a = active.value;
  if (!a.length) return null;
  const total = a.reduce((s, q) => s + q.size, 0), done = a.reduce((s, q) => s + q.done, 0);
  const cur = a.find(q => q.state === 'run') || a[0];
  return { cur, pct: total ? (done / total) * 100 : 0, speed: a.reduce((s, q) => s + (q.state === 'run' ? q.speed : 0), 0), paused: a.every(q => q.state === 'pause'), n: a.length };
});
export const qPct = (q: TransferItem) => (q.size ? Math.floor((q.done / q.size) * 100) : 0);
export const routeLabel = (q: TransferItem) => {
  const ext = q.from.src === 'ext', relay = !ext && !isLocal(q.from.src) && !isLocal(q.to.src);
  // Running or done: the route it took. Otherwise the one its next start takes: one server copies
  // within itself through Kite, and a relay that began (bytes done, not directly) stays the relay.
  // A relay paused before its first byte reads as direct here; the core keeps it on the relay.
  const live = q.state === 'run' || q.state === 'count' || q.state === 'done';
  const next = settings.agentFwd && !q.relay && q.from.src !== q.to.src && (q.done === 0 || !!q.direct);
  const direct = relay && (live ? !!q.direct : next);
  return `${ext ? tr('Проводник') : label(q.from.src as string)} → ${label(q.to.src as string)}` + (direct ? tr(' · напрямую') : relay ? tr(' · через Kite') : '');
};

/* ---------- A direct run that failed: ask to go through Kite ---------- */

const directFailed = (q: TransferItem) => q.state === 'err' && q.code === 'DIRECT_FAILED';
/** One question per batch and pair of servers; the answer covers all of its items. */
const pairKey = (q: TransferItem) => `${q.batch}|${q.from.src}|${q.to.src}`;
/** Items a question already covered. A covered item that is retried and fails again is a new failure. */
const covered = new Set<number>();
let failedBefore = new Set<number>();
/** The unfinished items of that batch and pair: the answer moves them all to Kite. */
const pairIds = (q: TransferItem, items = queue.value) =>
  items.filter(x => pairKey(x) === pairKey(q) && x.state !== 'done' && (x.state !== 'err' || directFailed(x))).map(x => x.id);
export const relayQ = (q: TransferItem) => backend.transferRelay([q.id]);

async function askRelay(q: TransferItem, items: TransferItem[]) {
  for (const id of pairIds(q, items)) covered.add(id);
  const go = await ask<boolean>('confirm', {
    title: tr('Напрямую не получилось'),
    // The reason without the title, which the dialog already shows.
    text: `${q.err.replace(/^[^.]*\.\s*/, '').replace(/\.?$/, '.')} ${tr('Скопировать {from} → {to} через Kite?', { from: label(q.from.src as string), to: label(q.to.src as string) })}`,
    ok: tr('Через Kite'),
    cancel: tr('Оставить'),
  });
  if (go) backend.transferRelay(pairIds(q));
}

function watchDirect(items: TransferItem[]) {
  const now = new Set(items.filter(directFailed).map(x => x.id));
  for (const id of failedBefore) if (!now.has(id)) covered.delete(id);
  failedBefore = now;
  for (const q of items) if (directFailed(q) && !covered.has(q.id)) void askRelay(q, items);
}

export async function transfer(from: Endpoint, entries: Entry[], to: Endpoint, move: boolean) {
  if (!entries.length) return;
  if (from.src !== 'ext' && from.src === to.src && from.path === to.path) { toast('Это та же папка', 'info'); return; }
  for (const e of entries) {
    if (!e.dir || from.src === 'ext' || from.src !== to.src) continue;
    if (within(from.src, childOf(from.src, from.path, e.name), to.path)) { toast(tr('Нельзя поместить {n} в саму себя', { n: e.name }), 'bad'); return; }
  }
  let policy: ConflictPolicy | 'ask' = settings.conflict;
  // Windows folders ignore case: `Readme.md` lands on `README.md`.
  const fold = LOCAL_WIN && isLocal(to.src);
  const same = (a: string, b: string) => (fold ? a.toLowerCase() === b.toLowerCase() : a === b);
  try {
    const target = await backend.fsList(to.src as string, to.user, to.path);
    const clash = entries.filter(e => target.some(t => same(t.name, e.name)));
    if (clash.length && policy === 'ask') {
      const r = await ask<ConflictPolicy>('conflict', { names: clash.map(e => e.name), where: `${label(to.src as string)} · ${to.path}`, total: entries.length, remember: false });
      if (!r) return;
      policy = r;
    }
    const batch = await backend.transferEnqueue(from, entries, to, move, policy === 'ask' ? 'replace' : policy);
    // The core skips a target that an unfinished item already writes.
    const queued = entries.some(e => active.value.some(q => q.to.src === to.src && same(q.to.path, to.path) && same(q.tname, e.name)));
    if (!batch) toast(queued ? 'Уже в очереди' : 'Всё уже на месте', 'info');
  } catch (e) {
    toast(isBackendError(e) ? e.title : String(e), 'bad');
  }
}
export function resolveConflict(d: Record<string, any>, policy: ConflictPolicy, close: (r: unknown) => void) {
  if (d.remember) settings.conflict = policy;
  close(policy);
}

export function pauseAll() { backend.transferPauseAll(!qStat.value?.paused); }
export const togglePause = (q: TransferItem) => (q.state === 'pause' ? backend.transferResume(q.id) : backend.transferPause(q.id));
export const cancelQ = (q: TransferItem) => backend.transferCancel(q.id);
export const retryQ = (q: TransferItem, asRoot = false) => backend.transferRetry(q.id, asRoot);
export const clearDone = () => backend.transferClearDone();

export async function initQueue() {
  backend.onTransferState(items => { queue.value = items; watchDirect(items); });
  backend.onTransferBatch(async b => {
    // Failures that are only failed direct runs: the question about Kite says it all. The batch
    // event may come before the last state, so the items are read as they are now and asked here.
    if (b.bad) {
      const list = await backend.transferList();
      const failed = list.filter(q => q.batch === b.batch && q.state === 'err');
      if (failed.length && failed.every(directFailed)) {
        for (const q of failed) if (!covered.has(q.id)) void askRelay(q, list);
        return;
      }
    }
    if (b.bad) toast(tr('Передано {ok} из {n}, ошибок: {bad}', { ok: b.ok, n: b.total, bad: b.bad }), 'bad', { label: 'Очередь', run: () => { ui.queueOpen = true; } }, 5000);
    else if (b.ok) toast(tr(b.move ? 'Перемещено: {what} в {to}' : 'Скопировано: {what} в {to}', { what: objs(b.ok), to: label(b.to) }), 'ok');
  });
  queue.value = await backend.transferList();
  // Failed before this start: the row offers Kite, no question pops up at launch.
  for (const q of queue.value) if (directFailed(q)) covered.add(q.id);
  failedBefore = new Set(covered);
  waveBusy(() => queue.value.some(q => q.state === 'run'));
}
