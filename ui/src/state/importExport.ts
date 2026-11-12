/* Import and export of the server list as servers.json. Servers are matched by code. */
import { computed } from 'vue';
import { backend, isBackendError, type ImportServer, type Server, type ServerUser } from '../backend';
import { lang, tr } from '../i18n';
import { collator, copyText, MB } from '../util/format';
import { ask, closeDlg, dlg, toast } from './overlay';
import { groupOrder, newServerId, saveServers, servers, srvByCode } from './servers';
import { dropServer } from './serverCard';

type Parsed = ImportServer;
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

/** Every group with servers in the sidebar's group order, whatever the search: the export takes them all. */
export const allGroups = computed(() => {
  const names = [...groupOrder, ...new Set(servers.map(s => s.group).filter(g => !groupOrder.includes(g)))];
  return names.map(name => ({ name, total: servers.filter(s => s.group === name).length })).filter(g => g.total);
});

export function openImport(tab: 'import' | 'export' = 'import') {
  const sel: Record<string, boolean> = {};
  for (const g of allGroups.value) sel[g.name] = true;
  ask('import', { tab, fileName: '', text: '', parsed: null, err: '', removeMissing: false, over: false, exportGroups: sel, exportUsers: true });
}

/** The core checks the file (version, code, duplicates); the diff with the list is made here. */
export async function parseImport(text: string, fileName: string) {
  const d = dlg.value!.data;
  d.text = text; d.fileName = fileName; d.parsed = null; d.err = '';
  const read = ++reads;
  try {
    const list = await backend.importRead(text);
    if (read !== reads) return;
    d.parsed = list.map(s => ({ ...s, group: s.group || tr('Без группы') }));
  } catch (e) {
    if (read === reads) d.err = isBackendError(e) ? e.title : String(e);
  }
}
/** A slower read of an earlier file must not land over a newer one. */
let reads = 0;

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
  // By code, not by the sidebar's sort: the same list gives the same file.
  const groups = allGroups.value.map(g => g.name).filter(n => d.data.exportGroups[n]).map(name => ({
    name,
    servers: servers.filter(s => s.group === name).sort((a, b) => collator.compare(a.code, b.code)).map(s => ({
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
export async function saveExport() {
  try {
    const path = await backend.exportWrite(exportJson.value);
    if (path) toast(tr('Сохранено: {p}', { p: path }), 'ok');
  } catch (e) { toast(isBackendError(e) ? e.title : String(e), 'bad'); }
}
export async function copyExport() { await copyText(exportJson.value); toast('JSON скопирован', 'ok'); }
export async function copyAgent() { await copyText(AGENT_PROMPT[lang.value]); toast('Задание скопировано', 'ok'); }
