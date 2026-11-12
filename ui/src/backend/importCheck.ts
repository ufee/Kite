/*
 * servers.json checks as the core makes them (src-tauri/src/store/import.rs), for the mock, and
 * the words for their codes, for both backends.
 */
import { tr } from '../i18n';
import type { BackendError, ErrCode, ImportServer } from './types';

export type ImportFail = { code: ErrCode } & Record<string, string>;

export function importWords(e: ImportFail): BackendError {
  const c = e.server || tr('(без code)');
  const title = ((): string => {
    switch (e.code) {
      case 'IMPORT_BIG': return tr('Файл больше 2 МБ');
      case 'IMPORT_JSON': return tr('Это не JSON: {e}', { e: e.reason });
      case 'IMPORT_VERSION': return tr('Версия {v} не поддерживается, нужна 1', { v: e.version });
      case 'IMPORT_NO_GROUPS': return tr('Нет списка groups');
      case 'IMPORT_CODE': return tr('Код {c} не подходит: латиница, цифры и дефис', { c });
      case 'IMPORT_HOST': return tr('У сервера {c} нет адреса', { c });
      case 'IMPORT_PORT': return tr('У сервера {c} порт не от 1 до 65535', { c });
      case 'IMPORT_USER': return tr('У сервера {c} неверный или повторный пользователь {u}', { c, u: e.user || '""' });
      case 'IMPORT_KEY': return tr('У сервера {c} ключ {k}: нужно только имя файла из ~/.ssh', { c, k: e.key });
      case 'IMPORT_DUP': return tr('Сервер {c} в файле дважды', { c });
      default: return tr('Ошибка формата');
    }
  })();
  return { code: e.code, title, text: '' };
}

const MAX = 2 * 1024 * 1024;
const CODE = /^[a-z0-9][a-z0-9-]*$/i;
const ACCOUNT = /^[A-Za-z0-9_.$][A-Za-z0-9_.$-]{0,63}$/;
const str = (v: unknown) => (typeof v === 'string' ? v.trim() : typeof v === 'number' ? String(v) : '');

/** Throws an ImportFail like the core's `import::parse`. */
export function parseServers(raw: string): ImportServer[] {
  const fail = (code: ErrCode, facts: Record<string, string> = {}): never => { throw { code, ...facts } satisfies ImportFail; };
  // Bytes, as the core counts them, not UTF-16 units.
  if (new TextEncoder().encode(raw).length > MAX) fail('IMPORT_BIG');
  let json: any;
  try { json = JSON.parse(raw.replace(/^﻿/, '')); } catch (e) { fail('IMPORT_JSON', { reason: (e as Error).message }); }
  if (!json || typeof json !== 'object' || Array.isArray(json)) fail('IMPORT_NO_GROUPS');
  if (json.version !== undefined && json.version !== 1) fail('IMPORT_VERSION', { version: str(json.version).slice(0, 20) });
  const groups: [string, unknown[]][] = Array.isArray(json.groups)
    ? json.groups.filter((g: any) => g && typeof g === 'object' && Array.isArray(g.servers)).map((g: any) => [str(g.name), g.servers])
    : Array.isArray(json.servers) ? [['', json.servers]] : fail('IMPORT_NO_GROUPS');
  const seen = new Set<string>(), out: ImportServer[] = [];
  for (const [group, list] of groups) for (const item of list) {
    const s: any = item && typeof item === 'object' ? item : {};
    const code = str(s.code);
    if (!CODE.test(code)) fail('IMPORT_CODE', { server: code });
    const host = str(s.host);
    if (!host || /\s/.test(host)) fail('IMPORT_HOST', { server: code });
    const p = str(s.port), port = s.port == null ? 22 : /^\d{1,5}$/.test(p) ? Number(p) : 0;
    if (port < 1 || port > 65535) fail('IMPORT_PORT', { server: code });
    const users: { name: string; key: string }[] = [];
    let def = '';
    for (const u of Array.isArray(s.users) ? s.users : []) {
      const name = str(u?.name), key = str(u?.key);
      if (!ACCOUNT.test(name) || users.some(x => x.name === name)) fail('IMPORT_USER', { server: code, user: name });
      if (key && key !== 'auto' && (key === '.' || key === '..' || /[/\\:\x00-\x1f]/.test(key))) fail('IMPORT_KEY', { server: code, key });
      if (!def && u?.default === true) def = name;
      users.push({ name, key: key || 'auto' });
    }
    if (!users.length) users.push({ name: 'root', key: 'auto' });
    if (seen.has(code)) fail('IMPORT_DUP', { server: code });
    seen.add(code);
    out.push({ code, desc: str(s.description), host, port, users, defUser: def || users[0].name, group: str(s.group) || group });
  }
  return out;
}
