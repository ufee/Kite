/* Import and export of the server list as servers.json. Servers are matched by code. */
import { computed } from 'vue';
import { backend, type Server, type ServerUser } from '../backend';
import { lang, tr } from '../i18n';
import { collator, copyText, MB } from '../util/format';
import { ask, closeDlg, dlg, toast } from './overlay';
import { groupOrder, newServerId, saveServers, servers, sideGroups, sortedServers, srvByCode } from './servers';
import { dropServer } from './serverCard';

type Parsed = { code: string; desc: string; host: string; port: number; users: ServerUser[]; defUser: string; group: string };
type Status = 'new' | 'changed' | 'same' | 'removed';

/** Shown as an example in the import tab. It overlaps the demo list, so the diff has every kind of row. */
export const SAMPLE = JSON.stringify({
  version: 1,
  groups: [
    { name: 'Production', servers: [
      { code: 'web1', description: 'Главный веб-сервер', host: '203.0.113.17', port: 22, users: [{ name: 'root', default: true }, { name: 'admin', key: 'id_ed25519' }, { name: 'deploy', key: 'deploy_ed25519' }] },
      { code: 'web2', description: 'Второй веб-узел', host: '203.0.113.18', port: 2222, users: [{ name: 'root', default: true }, { name: 'admin', key: 'id_ed25519' }, { name: 'deploy', key: 'deploy_ed25519' }] },
      { code: 'cache1', description: 'Redis и Memcached', host: '203.0.113.25', port: 22, users: [{ name: 'root', default: true }, { name: 'admin', key: 'id_ed25519' }] },
    ] },
    { name: 'Staging', servers: [
      { code: 'stage1', description: 'Тестовый веб и превью веток', host: '198.51.100.3', port: 22, users: [{ name: 'root', default: true }] },
      { code: 'qa1', description: 'Автотесты', host: '198.51.100.30', port: 22, users: [{ name: 'root', default: true }, { name: 'deploy', key: 'deploy_ed25519' }] },
    ] },
    { name: 'Дом', servers: [
      { code: 'nas', description: 'NAS и медиа', host: '192.0.2.10', port: 22, users: [{ name: 'root', default: true }] },
    ] },
  ],
}, null, 2);

const AGENT_FORMAT = `{
  "version": 1,
  "groups": [
    { "name": "Production", "servers": [
      { "code": "web1", "description": "Web", "host": "203.0.113.17", "port": 22,
        "users": [ { "name": "root", "default": true }, { "name": "deploy", "key": "deploy_ed25519" } ] }
    ] }
  ]
}`;
const AGENT_PROMPT = {
  ru: `Собери из моих заметок о серверах файл servers.json для импорта в Kite.
Формат:
${AGENT_FORMAT}
Правила. code: уникальное имя сервера. description: коротко, что на сервере. key: только имя файла ключа из ~/.ssh, без пути. Без паролей, токенов и приватных ключей.`,
  en: `Build a servers.json file for importing into Kite from my notes about the servers.
Format:
${AGENT_FORMAT}
Rules. code: unique server name. description: briefly what runs on the server. key: only the key file name from ~/.ssh, no path. No passwords, tokens or private keys.`,
};

export function openImport(tab: 'import' | 'export' = 'import') {
  const sel: Record<string, boolean> = {};
  for (const g of sideGroups.value) sel[g.name] = true;
  ask('import', { tab, fileName: '', text: '', parsed: null, err: '', removeMissing: false, over: false, exportGroups: sel, exportUsers: true });
}

export function parseImport(text: string, fileName: string) {
  const d = dlg.value!.data;
  d.text = text; d.fileName = fileName; d.parsed = null; d.err = '';
  let json: any;
  try { json = JSON.parse(text); } catch (e) { d.err = tr('Это не JSON: {e}', { e: (e as Error).message }); return; }
  const groups = Array.isArray(json?.groups) ? json.groups : Array.isArray(json?.servers) ? [{ name: tr('Без группы'), servers: json.servers }] : null;
  if (!groups) { d.err = tr('Нет списка groups'); return; }
  const list: Parsed[] = [];
  for (const g of groups) for (const s of g.servers || []) {
    if (!s.code || !s.host) { d.err = tr('У сервера {c} нет code или host', { c: s.code || tr('(без code)') }); return; }
    const raw: { name: string; key?: string; default?: boolean }[] = s.users?.length ? s.users : [{ name: 'root', default: true }];
    const users = raw.map(u => ({ name: String(u.name), key: u.key || 'auto' }));
    list.push({
      code: String(s.code), desc: s.description || '', host: String(s.host), port: +s.port || 22, users,
      defUser: raw.find(u => u.default)?.name || users[0].name, group: s.group || g.name || tr('Без группы'),
    });
  }
  d.parsed = list;
}

export const impDiff = computed(() => {
  const d = dlg.value;
  if (d?.kind !== 'import' || !d.data.parsed) return null;
  const rows: { s: Parsed | Server; status: Status; text: string }[] = [], codes = new Set<string>();
  for (const s of d.data.parsed as Parsed[]) {
    codes.add(s.code);
    const cur = srvByCode(s.code);
    if (!cur) { rows.push({ s, status: 'new', text: tr('новый') }); continue; }
    const ch: string[] = [];
    if (cur.host !== s.host) ch.push(tr('адрес {a} → {b}', { a: cur.host, b: s.host }));
    if (cur.port !== s.port) ch.push(tr('порт {a} → {b}', { a: cur.port, b: s.port }));
    if (cur.description !== s.desc) ch.push(tr('описание'));
    if (cur.group !== s.group) ch.push(tr('группа'));
    const sig = (us: ServerUser[], def: string) => us.map(u => u.name + ':' + u.key).join() + '|' + def;
    if (sig(cur.users, cur.defaultUser) !== sig(s.users, s.defUser)) ch.push(tr('пользователи'));
    rows.push({ s, status: ch.length ? 'changed' : 'same', text: ch.length ? ch.join(', ') : tr('без изменений') });
  }
  const missing = servers.filter(s => !codes.has(s.code));
  if (d.data.removeMissing) for (const s of missing) rows.push({ s, status: 'removed', text: tr('будет удалён') });
  const order: Record<Status, number> = { new: 0, changed: 1, removed: 2, same: 3 };
  rows.sort((a, b) => order[a.status] - order[b.status] || collator.compare(a.s.code, b.s.code));
  const count = (st: Status) => rows.filter(r => r.status === st).length;
  return { rows, nNew: count('new'), nChanged: count('changed'), nSame: count('same'), nRemoved: count('removed'), missing: missing.length };
});
export const statusColor = (st: Status) => ({ new: 'var(--ok)', changed: 'var(--warn)', removed: 'var(--bad)', same: 'var(--line2)' })[st];

export function impFile(ev: Event | DragEvent) {
  const d = dlg.value!.data;
  const f = (ev.target as HTMLInputElement | null)?.files?.[0] || (ev as DragEvent).dataTransfer?.files?.[0];
  d.over = false;
  if (!f) return;
  if (f.size > 2 * MB) { d.err = tr('Файл больше 2 МБ'); return; }
  f.text().then(text => parseImport(text, f.name));
}
export async function impPaste() {
  try { parseImport(await navigator.clipboard.readText(), tr('из буфера обмена')); } catch { toast('Нет доступа к буферу', 'bad'); }
}

export async function applyImport() {
  const diff = impDiff.value;
  if (!diff) return;
  for (const r of diff.rows) {
    if (r.status === 'removed') { await dropServer((r.s as Server).id); continue; }
    if (r.status === 'same') continue;
    const s = r.s as Parsed;
    if (!groupOrder.includes(s.group)) groupOrder.push(s.group);
    if (r.status === 'new') {
      servers.push({
        id: newServerId(), code: s.code, group: s.group, description: s.desc, host: s.host, port: s.port, users: s.users,
        defaultUser: s.defUser, keyMode: 'auto', keyFile: '', authPass: false, startDir: '', keepalive: 30,
      });
    } else {
      const cur = srvByCode(s.code)!;
      // A new address is checked on its own; the old one keeps its known_hosts record (shared with ssh).
      // The keychain holds the password of the default user only.
      if (cur.authPass && cur.defaultUser !== s.defUser) await backend.secretDelete(cur.id, cur.defaultUser).catch(() => undefined);
      Object.assign(cur, { group: s.group, description: s.desc, host: s.host, port: s.port, users: s.users, defaultUser: s.defUser });
    }
  }
  await saveServers();
  closeDlg(true);
  const parts = [diff.nNew && tr('{n} новых', { n: diff.nNew }), diff.nChanged && tr('{n} изменено', { n: diff.nChanged }), diff.nRemoved && tr('{n} удалено', { n: diff.nRemoved })].filter(Boolean);
  toast(tr('Импорт: {x}', { x: parts.join(', ') || tr('без изменений') }), 'ok');
}

export const exportJson = computed(() => {
  const d = dlg.value;
  if (d?.kind !== 'import') return '';
  const groups = sideGroups.value.map(g => g.name).filter(n => d.data.exportGroups[n]).map(name => ({
    name,
    servers: sortedServers(servers.filter(s => s.group === name)).map(s => ({
      code: s.code, description: s.description, host: s.host, port: s.port,
      ...(d.data.exportUsers ? { users: s.users.map(u => ({ name: u.name, ...(u.name === s.defaultUser ? { default: true } : {}), ...(u.key !== 'auto' ? { key: u.key } : {}) })) } : {}),
    })),
  }));
  return JSON.stringify({ version: 1, groups }, null, 2);
});
export const exportCount = computed(() => {
  const d = dlg.value;
  return d?.kind === 'import' ? servers.filter(s => d.data.exportGroups[s.group]).length : 0;
});
export function saveExport() {
  const a = document.createElement('a');
  a.href = URL.createObjectURL(new Blob([exportJson.value], { type: 'application/json' }));
  a.download = 'servers.json';
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
  toast('servers.json сохранён', 'ok');
}
export async function copyExport() { await copyText(exportJson.value); toast('JSON скопирован', 'ok'); }
export async function copyAgent() { await copyText(AGENT_PROMPT[lang.value]); toast('Задание скопировано', 'ok'); }
