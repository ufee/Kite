/* Operation log from the core and the activity meter in the status bar. */
import { computed, reactive } from 'vue';
import type { LogRow } from '../backend';
import { copyText } from '../util/format';
import { ui } from './prefs';
import { toast } from './overlay';

const LOG_MAX = 1000;
export const log = reactive<LogRow[]>([]);
export const logView = computed(() => (ui.logOnlyErr ? log.filter(r => r.state === 'err') : log));
export const logLine = (r: LogRow) => `${r.time} ${r.src} ${r.tag} ${r.text}${r.note ? ' → ' + r.note : ''}`;

export function logPush(row: LogRow) {
  log.push(row);
  if (log.length > LOG_MAX) log.splice(0, log.length - LOG_MAX);
  pulse(row.state);
}
export function logUpdate(row: LogRow) {
  for (let k = log.length - 1; k >= 0; k--) if (log[k].id === row.id) { Object.assign(log[k], row); break; }
  pulse(row.state);
}
export function toggleLog() {
  ui.logOpen = !ui.logOpen;
  if (ui.logOpen) ui.logErr = false;
}
export const toggleLogErr = () => { ui.logOnlyErr = !ui.logOnlyErr; };
export function copyLog() {
  copyText(logView.value.map(logLine).join('\n'));
  toast('Журнал скопирован', 'ok', null, 1800);
}
export function clearLog() { log.length = 0; ui.logErr = false; }

/*
 * The meter scrolls: every step pushes a bar sized by what happened since the last one.
 * It lives outside Vue reactivity: only the meter itself repaints, ten times a second.
 */
const BARS = 7;
const wave: number[] = Array(BARS).fill(0);
let hits = 0, bad = false, timer = 0;
let busy: () => boolean = () => false;
const subs = new Set<(v: number[]) => void>();

export function waveBusy(fn: () => boolean) { busy = fn; waveWake(); }
export function waveSubscribe(cb: (v: number[]) => void) {
  subs.add(cb);
  cb(wave);
  return () => subs.delete(cb);
}
function step() {
  const run = busy();
  const level = Math.max(hits ? Math.min(1, 0.45 + hits * 0.2) : 0, run ? 0.3 + Math.random() * 0.5 : 0);
  wave.shift();
  wave.push(bad ? -level : level); // negative: an error bar
  hits = 0; bad = false;
  for (const cb of subs) cb(wave);
  if (!level && wave.every(v => !v)) { clearInterval(timer); timer = 0; }
}
export function waveWake() { if (!timer) timer = window.setInterval(step, 110); }
function pulse(state: LogRow['state']) {
  hits++;
  if (state === 'err') { bad = true; if (!ui.logOpen) ui.logErr = true; }
  waveWake();
}
