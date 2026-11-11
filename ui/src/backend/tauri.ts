/*
 * TauriBackend: the Rust core takes over method by method. Stage 3 brought SSH, host keys, the keychain,
 * keys and the agent and the core's log; stage 4 the files of this PC and of servers. Everything else
 * still runs on the mock underneath; operations the core does not have yet reject with NOT_YET.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { tr, trn } from '../i18n';
import { isLocal, LOCAL_WIN } from '../util/paths';
import { MockBackend } from './mock';
import type { Store } from './store';
import type {
  AgentStatus, BackendError, Entry, ErrCode, HostKeyRequest, LogRow, PassphraseRequest,
  Server, ServerConfig, ServerDraft, ServerId, SessionState, Src, TestResult, TextFile,
} from './types';

/** Error as the core sends it: the code and its facts, no words. */
type CoreError = { code: string } & Record<string, string>;

interface CoreMsg { text: string; vars?: [string, string][] }
interface CoreRow { id: number; ts: number; src: string; tag: string; text: CoreMsg; note: CoreMsg; state: LogRow['state'] }
interface TestReport { login: boolean; password: boolean; ms: number; unverified: string | null }

/** `a|b|c` with a var `n` is a count in plural forms. */
function msg(m: CoreMsg) {
  if (!m.text) return '';
  const vars = m.vars ? Object.fromEntries(m.vars) : undefined;
  return vars?.n != null && m.text.includes('|') ? trn(Number(vars.n), m.text) : tr(m.text, vars);
}
const stamp = (ms: number) => {
  const d = new Date(ms), z = (n: number, w = 2) => String(n).padStart(w, '0');
  return `${z(d.getHours())}:${z(d.getMinutes())}:${z(d.getSeconds())}.${z(d.getMilliseconds(), 3)}`;
};
const row = (r: CoreRow): LogRow => ({ id: r.id, time: stamp(r.ts), src: r.src === 'local' ? tr('Этот ПК') : r.src, tag: r.tag, text: msg(r.text), note: msg(r.note), state: r.state });

/** Words for the core's error codes; the table of spec section 5 where it has one. */
function words(e: CoreError): [string, string] {
  switch (e.code) {
    case 'TIMEOUT': return [tr('Сервер не отвечает'), tr('{addr}: нет ответа за 10 с. Проверьте адрес или сеть.', { addr: e.addr })];
    case 'NET': return [tr('Нет связи с сервером'), `${e.addr}: ${e.reason}`];
    case 'DROPPED': return [tr('Сервер закрыл соединение'), tr('{addr}: сервер закрыл соединение до входа. Так бывает при блокировке после неудачных входов.', { addr: e.addr })];
    case 'HOSTKEY_REJECTED':return [tr('Подключение отменено'), tr('Ключ хоста не подтверждён.')];
    case 'HOSTKEY_CHANGED': return [tr('Ключ хоста изменился'), tr('{addr}: ключ не совпадает с known_hosts. Сохранённый {old}, новый {new}.', { addr: e.addr, old: e.old, new: e.new })];
    case 'AUTH_KEY': return [tr('Сервер не принял ключ'), tr('{u}@{c}. Если ключа там ещё нет, укажи пароль в карточке сервера.', { u: e.user, c: e.server })];
    case 'AUTH_PASS': return [tr('Пароль не подошёл'), tr('Укажи новый пароль в карточке сервера.')];
    case 'NO_SECRET': return [tr('Нет сохранённого пароля'), tr('Укажи пароль в карточке сервера.')];
    case 'AUTH_MULTI': return [tr('Сервер просит второй фактор'), tr('Кроме пароля сервер задаёт ещё вопросы. Такой вход Kite не поддерживает.')];
    case 'AUTH_METHODS': return [tr('Нет подходящего способа входа'), tr('Сервер предлагает: {m}.', { m: e.methods })];
    case 'KEY_FILE': return [tr('Ключ не прочитан'), `${e.path}: ${e.reason}`];
    case 'NO_PUBKEY': return [tr('Нет публичного ключа'), tr('Рядом с ключом нет файла .pub, а в агенте нет ключа ed25519.')];
    case 'KEY_CHECK': return [tr('Ключ добавлен, но вход по нему не прошёл'), e.reason];
    case 'PASSPHRASE': return [tr('Фраза не подошла'), ''];
    case 'CANCELLED': return [tr('Отменено'), ''];
    case 'DISCONNECTED': return [tr('Соединение потеряно'), ''];
    case 'KEYCHAIN': return [tr('Хранилище паролей недоступно'), e.reason];
    case 'ENOENT': return [tr('Не найдено: {p}', { p: e.path }), ''];
    case 'ENOTDIR': return [tr('Это файл: {p}', { p: e.path }), ''];
    case 'EACCES': return [tr('Нет доступа: {p}', { p: e.path }), ''];
    case 'EEXIST': return [tr('{v} уже есть в этой папке', { v: e.path.split(/[\\/]/).pop() ?? e.path }), ''];
    case 'BAD_PATH': return [tr(LOCAL_WIN ? 'Нужен путь вида D:\\папка' : 'Путь начинается с / или ~'), ''];
    case 'NO_EXEC': return [tr('Владелец не изменён'), tr('Сервер не выполняет команды: только SFTP.')];
    case 'EINVAL': return [tr('Операция не выполнена'), e.reason];
    default: return [tr('Ошибка SSH'), e.reason ?? ''];
  }
}

export const uiError = (e: unknown): BackendError => {
  if (e && typeof e === 'object' && 'code' in e) {
    const [title, text] = words(e as CoreError);
    return { code: (e as CoreError).code as ErrCode, title, text };
  }
  return { code: 'SSH', title: tr('Ошибка SSH'), text: String(e) };
};

/** Every core call: errors cross as BackendError with ready words. */
const call = <T>(cmd: string, args?: Record<string, unknown>) => invoke<T>(cmd, args).catch((e: unknown) => { throw uiError(e); });

/** A file source as the core takes it. */
const where = (src: Src, user: string) => (isLocal(src) ? 'local' : { server: src, user });

const notYet = (stage: number): BackendError => ({ code: 'NOT_YET', title: tr('Будет на этапе {n}', { n: stage }), text: '' });

export class TauriBackend extends MockBackend {
  override readonly kind = 'tauri';

  constructor(store: Store) {
    super(store);
    void listen<{ server: ServerId; user: string; state: SessionState | null }>('ssh://status', e => this.emit('ssh', e.payload.server, e.payload.user, e.payload.state));
    void listen<HostKeyRequest>('ssh://hostkey', e => this.emit('hostkey', e.payload));
    void listen<PassphraseRequest>('ssh://passphrase', e => this.emit('passphrase', e.payload));
    void listen<number[]>('ssh://prompt-gone', e => this.emit('promptGone', e.payload));
    // A reloaded window lost the dialogs of the page before: their connections stop waiting.
    void invoke('prompts_reset').catch(() => undefined);
    void listen<CoreRow>('log://row', e => this.emit('log', row(e.payload)));
    void listen<CoreRow>('log://update', e => this.emit('logUpdate', row(e.payload)));
  }

  /** Real servers only: the demo list belongs to the mock. */
  override async serversLoad(): Promise<ServerConfig> {
    const cfg = (await this.store.serversLoad()) ?? { servers: [], groups: [] };
    this.useServers(cfg);
    return structuredClone(cfg);
  }

  /* ---------- SSH ---------- */
  override sshConnect(server: Server, user: string) { return call<void>('ssh_connect', { server, user }); }
  override sshDisconnect(server: ServerId, user?: string) { return call<void>('ssh_disconnect', { server, user: user ?? null }); }
  override sshSessions() { return call<{ server: ServerId; user: string; state: SessionState }[]>('ssh_sessions'); }
  override async logTail() {
    const core = await call<CoreRow[]>('log_tail').catch(() => []);
    return [...(await super.logTail()), ...core.map(row)];
  }

  override async sshTest(draft: ServerDraft): Promise<TestResult> {
    if (!draft.host) return { ok: false, text: tr('Не указан адрес') };
    try {
      const r = await call<TestReport>('ssh_test', { draft });
      const u = draft.user || 'root';
      if (!r.login) return { ok: true, text: tr('Сервер доступен. Пароль проверим при сохранении, после ключа хоста') };
      const base = tr(r.password ? 'Пароль подошёл · {u} · {ms} мс' : 'Подключение есть · {u} · {ms} мс', { u, ms: r.ms });
      return { ok: true, text: r.unverified ? `${base}. ${tr('Ключ хоста ещё не подтверждён')}` : base };
    } catch (e) {
      const err = e as BackendError;
      return { ok: false, text: err.text && err.code !== 'AUTH_PASS' && err.code !== 'NO_SECRET' ? `${err.title}. ${err.text}` : err.title };
    }
  }
  override sshFirstLogin(draft: ServerDraft, installKey: boolean) { return call<void>('ssh_first_login', { draft, installKey }); }
  override hostkeyRespond(reqId: number, trust: boolean) { void invoke('hostkey_respond', { reqId, trust }); }
  override passphraseRespond(reqId: number, phrase: string | null, addToAgent: boolean) {
    return call<void>('passphrase_respond', { reqId, phrase, addToAgent });
  }
  override hostkeyKnown(host: string, port: number) { return call<string | null>('hostkey_known', { host, port }); }
  override hostkeyForget(host: string, port: number) { return call<void>('hostkey_forget', { host, port }); }
  override setStrictHost(on: boolean) { void invoke('set_strict_host', { on }); }

  /* ---------- Secrets and keys ---------- */
  override secretSet(server: ServerId, user: string, password: string) { return call<void>('secret_set', { server, user, password }); }
  override secretDelete(server: ServerId, user: string) { return call<void>('secret_delete', { server, user }); }
  override secretsAvailable() { return call<boolean>('secrets_available'); }
  override agentStatus() { return call<AgentStatus>('agent_status'); }
  override keysList() { return call<string[]>('keys_list'); }
  override pickKeyFile() { return call<string | null>('pick_key_file'); }

  /* ---------- Files: this PC and servers ---------- */
  override fsList(src: Src, user: string, path: string) { return call<Entry[]>('fs_list', { src: where(src, user), path }); }
  override fsResolve(src: Src, user: string, raw: string, base?: string) {
    return invoke<string>('fs_resolve', { src: where(src, user), raw, base: base ?? null }).catch((e: unknown) => {
      // A typed path that is not there is a folder the user looked for.
      const c = e as CoreError;
      if (c?.code === 'ENOENT') throw { code: 'ENOENT', title: tr('Нет такой папки: {p}', { p: c.path }), text: '' } satisfies BackendError;
      throw uiError(e);
    });
  }
  override fsHome(src: Src, user: string) { return call<string>('fs_home', { src: where(src, user) }); }
  override fsMkdir(src: Src, user: string, path: string) { return call<void>('fs_mkdir', { src: where(src, user), path }); }
  override fsRename(src: Src, user: string, from: string, to: string) { return call<void>('fs_rename', { src: where(src, user), from, to }); }
  override fsDelete(src: Src, user: string, paths: string[]) { return call<void>('fs_delete', { src: where(src, user), paths }); }
  override fsTrash(paths: string[]) { return call<void>('fs_trash', { paths }); }
  override fsChmod(src: Src, user: string, paths: string[], mode: number | null, owner: string, group: string, recursive: boolean) {
    return call<void>('fs_chmod', { src: where(src, user), paths, mode, owner, group, recursive });
  }
  override fsReadText(src: Src, user: string, path: string) { return call<TextFile>('fs_read_text', { src: where(src, user), path }); }
  override fsDrives() { return call<string[]>('fs_drives'); }

  // The mock's editor and queue work on its own files, not on the real ones the panes now show.
  override async editOpen() { throw notYet(7); }
  override async transferEnqueue(): Promise<number> { throw notYet(6); }
  override async ptyOpen(src: Src, user: string, cwd: string, cols: number, rows: number, onData: (data: string) => void) {
    if (!isLocal(src)) throw notYet(5);
    return super.ptyOpen(src, user, cwd, cols, rows, onData);
  }
}
