/* Server list, groups, SSH sessions and recent places. Everything points to the server id. */
import { computed, reactive } from 'vue';
import { backend, type Server, type ServerId, type SessionState, type Src } from '../backend';
import { tr } from '../i18n';
import { collator } from '../util/format';
import { isLocal } from '../util/paths';
import { ui } from './prefs';
import { toast } from './overlay';

export const servers = reactive<Server[]>([]);
export const groupOrder = reactive<string[]>([]);
/** Session state per server and user: `id|user`. */
export const sessions = reactive<Record<string, SessionState>>({});
export const recent = reactive<ServerId[]>([]);
export const lastUsed = reactive<Record<ServerId, number>>({});
let useTick = 10;
export const recentDirs = reactive<{ src: Src; user: string; path: string }[]>([]);

export const srvBy = (id: ServerId | null | undefined) => (id ? servers.find(s => s.id === id) : undefined);
export const srvByCode = (code: string) => servers.find(s => s.code === code);
export const label = (src: Src) => (isLocal(src) ? tr('Этот ПК') : srvBy(src)?.code ?? '?');
export const sessionOf = (id: ServerId, user: string) => sessions[id + '|' + user];

const RANK: Record<SessionState, number> = { live: 3, wait: 2, bad: 1 };
/** The best state of any of the server's sessions: drives the dot in the sidebar. */
export function srvState(id: ServerId): SessionState | '' {
  let best: SessionState | '' = '';
  for (const [k, st] of Object.entries(sessions)) if (k.startsWith(id + '|') && (!best || RANK[st] > RANK[best])) best = st;
  return best;
}
export const newServerId = () => 's-' + crypto.randomUUID().slice(0, 8);

export async function loadServers() {
  const cfg = await backend.serversLoad();
  servers.splice(0, servers.length, ...cfg.servers);
  groupOrder.splice(0, groupOrder.length, ...cfg.groups);
}
export function saveServers() {
  for (const g of new Set(servers.map(s => s.group))) if (!groupOrder.includes(g)) groupOrder.push(g);
  return backend.serversSave({ servers: servers.map(s => ({ ...s, users: s.users.map(u => ({ ...u })) })), groups: [...groupOrder] });
}

export function touchRecent(id: ServerId) {
  const k = recent.indexOf(id);
  if (k >= 0) recent.splice(k, 1);
  recent.unshift(id);
  if (recent.length > 6) recent.pop();
  lastUsed[id] = ++useTick;
}
export function touchDir(src: Src, user: string, path: string) {
  const k = recentDirs.findIndex(d => d.src === src && d.path === path);
  if (k >= 0) recentDirs.splice(k, 1);
  recentDirs.unshift({ src, user, path });
  if (recentDirs.length > 10) recentDirs.pop();
}

export const sortedServers = (list: Server[]) =>
  [...list].sort((a, b) => (ui.sideSort === 'recent' ? (lastUsed[b.id] || 0) - (lastUsed[a.id] || 0) : 0) || collator.compare(a.code, b.code));

export const sideGroups = computed(() => {
  const q = ui.search.trim().toLowerCase();
  const names = [...groupOrder, ...new Set(servers.map(s => s.group).filter(g => !groupOrder.includes(g)))];
  return names.map(name => {
    const all = servers.filter(s => s.group === name);
    const items = sortedServers(all.filter(s => !q || `${s.code} ${s.description} ${s.host}`.toLowerCase().includes(q)));
    const live = all.filter(s => srvState(s.id) === 'live').length;
    return { name, items, total: all.length, live, closed: !q && !!ui.closed[name] };
  }).filter(g => g.items.length || (!q && g.total));
});
export const toggleGroup = (name: string) => { ui.closed[name] = !ui.closed[name]; };
export function collapseAllGroups(v: boolean) { for (const g of sideGroups.value) ui.closed[g.name] = v; }

export function srvTitle(s: Server | undefined) {
  if (!s) return '';
  const st = srvState(s.id);
  const t = tr(st === 'live' ? 'подключён' : st === 'bad' ? 'не отвечает' : st === 'wait' ? 'подключение…' : 'не подключён');
  return `${s.code} · ${s.description}\n${s.defaultUser}@${s.host}${s.port !== 22 ? ':' + s.port : ''} · ${t}`;
}

export async function disconnect(id: ServerId) {
  const s = srvBy(id);
  await backend.sshDisconnect(id);
  if (s) toast(tr('Отключено от {c}', { c: s.code }), 'info');
}

export function initServers() {
  backend.onSshStatus((server, user, state) => {
    const k = server + '|' + user;
    if (state) sessions[k] = state; else delete sessions[k];
  });
}
