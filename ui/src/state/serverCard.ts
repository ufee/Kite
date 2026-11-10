/* Server card: add, edit, test and delete a server. The card edits a copy; saving writes the whole list. */
import { nextTick } from 'vue';
import { backend, isBackendError, type Server, type ServerDraft, type ServerId } from '../backend';
import { tr } from '../i18n';
import { ask, closeDlg, confirmBox, dlg, toast } from './overlay';
import { groupOrder, newServerId, recent, saveServers, servers, sessions, srvBy } from './servers';
import { panes, setSource } from './panes';
import { terms } from './terms';
import { ui } from './prefs';

type Card = Record<string, any>;

export async function openServer(id: ServerId | null, focusNewUser = false) {
  const s = srvBy(id);
  const d: Card = s
    ? {
        id: s.id, code: s.code, group: s.group, desc: s.description, host: s.host, port: s.port,
        users: s.users.map(u => ({ ...u })), defUser: s.defaultUser, keyMode: s.keyMode, keyFile: s.keyFile,
        pass: '', install: !s.authPass, hasPass: s.authPass, busy: false, adv: false,
        keepalive: s.keepalive, startDir: s.startDir, fp: null, test: null, errs: {},
      }
    : {
        id: null, code: '', group: groupOrder[0] ?? tr('Без группы'), desc: '', host: '', port: 22,
        users: [{ name: 'root', key: 'auto' }], defUser: 'root', keyMode: 'auto', keyFile: '',
        pass: '', install: true, hasPass: false, busy: false, adv: false, keepalive: 30, startDir: '', fp: null, test: null, errs: {},
      };
  if (focusNewUser) d.users.push({ name: '', key: 'auto' });
  ask('server', d);
  const live = dlg.value!.data; // the reactive copy the dialog renders
  nextTick(() => document.querySelector<HTMLInputElement>(focusNewUser ? '.urow:last-child input' : '.dlg input')?.focus());
  // Linux without a Secret Service: no password-only servers, the key always goes on the server.
  backend.secretsAvailable().then(ok => { if (!ok) { live.noStore = true; live.install = true; } }, () => undefined);
  if (s) live.fp = await backend.hostkeyKnown(s.host, s.port).catch(() => null);
}

export function addUser(d: Card) {
  d.users.push({ name: '', key: 'auto' });
  nextTick(() => document.querySelector<HTMLInputElement>('.urow:last-child input')?.focus());
}
export function removeUser(d: Card, k: number) {
  const [u] = d.users.splice(k, 1);
  if (u.name === d.defUser) d.defUser = d.users[0]?.name || '';
}

const draftOf = (d: Card): ServerDraft => ({
  id: d.id, code: d.code.trim(), host: d.host.trim(), port: +d.port, user: d.defUser || 'root',
  keyMode: d.keyMode, keyFile: d.keyFile, password: d.pass, authPass: !!srvBy(d.id)?.authPass,
});

export async function testServer(d: Card) {
  d.test = { state: 'run' };
  const r = await backend.sshTest(draftOf(d));
  d.test = { state: r.ok ? 'ok' : 'bad', text: r.text };
}
export async function chooseKeyFile(d: Card) {
  const f = await backend.pickKeyFile();
  if (f) { d.keyMode = 'file'; d.keyFile = f; }
}
export function passHint(d: Card) {
  if (!d.pass && !d.hasPass) return tr('Если ключ уже на сервере, пароль не нужен');
  if (d.noStore) return tr('В системе нет хранилища паролей');
  if (!d.install) return tr('Вход только по паролю. Он хранится в системном хранилище');
  return tr(d.hasPass ? 'Войдём как {u} один раз, добавим ключ и удалим пароль' : 'Войдём как {u} один раз и добавим ключ. Пароль не сохраняем', { u: d.defUser });
}

/**
 * Logs in with the card's password before the server is saved, so a wrong one is caught right here.
 * A host key question lands above the card in the dialog stack. Returns false if the card must stay open.
 */
async function firstLogin(d: Card): Promise<boolean> {
  d.busy = true; d.errs = {};
  d.test = { state: 'run', text: tr(d.install ? 'Добавляем ключ…' : 'Проверяем пароль…') };
  try {
    await backend.sshFirstLogin(draftOf(d), d.install);
    return true;
  } catch (e) {
    const err = isBackendError(e) ? e : null;
    if (err?.code === 'AUTH_PASS') {
      d.errs = { pass: err.title }; d.pass = '';
      nextTick(() => document.querySelector<HTMLInputElement>('.pass-in')?.focus());
    } else if (err && err.code !== 'HOSTKEY_REJECTED') d.errs = { pass: err.title };
    return false;
  } finally {
    d.busy = false; d.test = null;
  }
}

export async function saveServer(d: Card) {
  if (d.busy) return;
  const errs: Record<string, string> = {};
  d.code = d.code.trim(); d.host = d.host.trim();
  if (!/^[a-z0-9][a-z0-9-]*$/i.test(d.code)) errs.code = tr('Латиница, цифры и дефис');
  else if (servers.some(s => s.code === d.code && s.id !== d.id)) errs.code = tr('Такое имя уже есть');
  if (!d.host) errs.host = tr('Нужен адрес');
  if (!(d.port > 0 && d.port < 65536)) errs.port = tr('Порт от 1 до 65535');
  d.users = d.users.filter((u: { name: string }) => u.name.trim());
  if (!d.users.length) errs.users = tr('Нужен хотя бы один пользователь');
  d.errs = errs;
  if (Object.keys(errs).length) return;
  if (!d.users.some((u: { name: string }) => u.name === d.defUser)) d.defUser = d.users[0].name;

  const was = srvBy(d.id);
  // The keychain keeps the password of the default user: another default user needs its own.
  const wasUser = was?.defaultUser ?? '';
  if (was?.authPass && !d.pass && wasUser !== d.defUser) {
    d.errs = { pass: tr('Нужен пароль для {u}', { u: d.defUser }) };
    return;
  }
  const login = !!d.pass || (!!was?.authPass && d.install);
  if (login && !(await firstLogin(d))) return;
  if (dlg.value?.data !== d) return; // the card was closed while logging in
  const authPass = !d.install && (!!d.pass || !!was?.authPass);
  const fields: Omit<Server, 'id'> = {
    code: d.code, group: d.group.trim() || tr('Без группы'), description: d.desc.trim(), host: d.host, port: +d.port,
    users: d.users.map((u: { name: string; key: string }) => ({ name: u.name.trim(), key: u.key })),
    defaultUser: d.defUser, keyMode: d.keyMode, keyFile: d.keyFile, authPass,
    startDir: d.startDir.trim(), keepalive: Math.max(0, +d.keepalive || 0),
  };
  const id: ServerId = was?.id ?? newServerId();
  // A password-only server keeps its password in the OS keychain; key login drops it. The keychain
  // goes first: if it fails, the list stays as it was and the card stays open with the error.
  if (authPass && d.pass) {
    try {
      await backend.secretSet(id, d.defUser, d.pass);
    } catch (e) {
      d.errs = { pass: isBackendError(e) ? e.title : String(e) };
      return;
    }
  }
  if (was) {
    // The old address keeps its known_hosts record: the file is shared with ssh and git, and
    // other servers may point there. The new address is checked on its own.
    Object.assign(was, fields);
    toast(tr('{c} сохранён', { c: d.code }), 'ok');
  } else {
    servers.push({ id, ...fields });
    toast(tr('{c} добавлен в {g}', { c: d.code, g: fields.group }), 'ok', { label: 'Открыть', run: () => setSource(ui.focus, id) });
  }
  if (was?.authPass && (!authPass || wasUser !== d.defUser)) await backend.secretDelete(id, wasUser).catch(() => undefined);
  d.pass = '';
  await saveServers();
  closeDlg(true);
}

export async function deleteServer(id: ServerId) {
  const s = srvBy(id);
  if (!s) return;
  if (!(await confirmBox(tr('Удалить {c} из списка?', { c: s.code }), 'На самом сервере ничего не изменится.', 'Удалить', true))) return;
  await dropServer(id);
  await saveServers();
  toast(tr('{c} удалён из списка', { c: s.code }), 'ok');
}
/** Removes the server from the list and everything that points to it. The caller saves the list. */
export async function dropServer(id: ServerId) {
  const k = servers.findIndex(s => s.id === id);
  if (k < 0) return;
  await backend.sshDisconnect(id).catch(() => undefined);
  const s = servers[k];
  if (s.authPass) await backend.secretDelete(id, s.defaultUser).catch(() => undefined);
  servers.splice(k, 1);
  for (const key of Object.keys(sessions)) if (key.startsWith(id + '|')) delete sessions[key];
  panes.forEach((p, i) => { if (p.src === id) setSource(i, 'local'); });
  for (const t of terms) if (t.src === id) t.dead = true;
  const r = recent.indexOf(id);
  if (r >= 0) recent.splice(r, 1);
}
export function deleteFromCard(d: Card) {
  closeDlg(null);
  if (d.id) deleteServer(d.id);
}
