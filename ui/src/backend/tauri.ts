/*
 * TauriBackend: the Rust core takes over method by method. Stage 3 brought SSH, host keys, the keychain,
 * keys and the agent and the core's log; stage 4 the files of this PC and of servers; stage 5 terminals. Everything else
 * still runs on the mock underneath; operations the core does not have yet reject with NOT_YET.
 */
import { Channel, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { tr, trn } from '../i18n';
import { fmtSize } from '../util/format';
import { isLocal, LOCAL_WIN } from '../util/paths';
import { type ImportFail, importWords } from './importCheck';
import { MockBackend } from './mock';
import { fail } from './mock/fs';
import type { Store } from './store';
import type {
  AgentStatus, BackendError, BatchReport, ConflictPolicy, Endpoint, Entry, ErrCode, HostKeyRequest, ImportServer, LogRow, PassphraseRequest,
  PtyExit, Server, ServerConfig, ServerDraft, ServerId, SessionState, Src, TestResult, TextFile, TransferItem,
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
  // Sizes cross as bytes: the words for them are the UI's.
  if (vars?.size != null) vars.size = fmtSize(Number(vars.size));
  return vars?.n != null && m.text.includes('|') ? trn(Number(vars.n), m.text) : tr(m.text, vars);
}
const stamp = (ms: number) => {
  const d = new Date(ms), z = (n: number, w = 2) => String(n).padStart(w, '0');
  return `${z(d.getHours())}:${z(d.getMinutes())}:${z(d.getSeconds())}.${z(d.getMilliseconds(), 3)}`;
};
const row = (r: CoreRow): LogRow => ({ id: r.id, time: stamp(r.ts), src: r.src === 'local' ? tr('Этот ПК') : r.src, tag: r.tag, text: msg(r.text), note: msg(r.note), state: r.state });

/** Why a direct run failed, as the core words it; `{x}` parts carry names and addresses. */
const DIRECT_REASONS = [
  'агент OpenSSH не запущен', 'в агенте OpenSSH нет ключей', 'сервер-источник не разрешает проброс агента',
  'на сервере-источнике нельзя запускать команды', 'на сервере-источнике не создаётся временный файл', 'rsync не запустился',
  'имя пользователя {u} не подходит для ssh', 'адрес {h} не подходит для ssh', 'ключ хоста {c} не сохранён в known_hosts',
  'второе подключение к серверу-источнику не открылось: {e}', 'на {c} нельзя запускать команды', 'нет rsync на {c}',
  '{c} не ответил на проверку rsync', 'ключ хоста {t} не совпал с сохранённым', 'ключ из агента не принят на {t} для {u}',
  'сервер-источник не видит {t}', 'на {t} кончилось место',
].map(s => {
  const names = [...s.matchAll(/\{(\w+)\}/g)].map(m => m[1]);
  const re = new RegExp(`^${s.replace(/[.*+?^$()|[\]\\]/g, '\\$&').replace(/\{\w+\}/g, '(.+?)')}$`);
  return { s, names, re };
});
/** Known reasons in the UI's language; tool output (rsync, ssh) stays as it is. */
function directReason(r: string): string {
  for (const { s, names, re } of DIRECT_REASONS) {
    const m = re.exec(r);
    if (m) return tr(s, Object.fromEntries(names.map((n, i) => [n, m[i + 1]])));
  }
  return r;
}

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
    case 'SERVER_GONE': return [tr('Сервер удалён из списка'), ''];
    case 'BAD_NAME': return [tr('Имя не подходит для Windows: {p}', { p: e.path }), tr('Файл пропущен.')];
    case 'NAME_CLASH': return [tr('Имя отличается от соседнего только регистром: {p}', { p: e.path }), tr('В Windows это один файл, второй пропущен.')];
    case 'SOURCE_CHANGED': return [tr('Файл изменился во время передачи: {p}', { p: e.path }), tr('Источник оставлен на месте.')];
    case 'SERVER_CHANGED': return [tr('Файл на сервере изменился: {p}', { p: e.path }), tr('Правка не загружена: решите, чья версия важнее.')];
    case 'EDIT_WAITS': return [tr('Правка ждёт: {p}', { p: e.path }), tr('Загрузка в этот файл стоит на паузе в очереди; правка уйдёт после неё.')];
    case 'NOT_REGULAR': return [tr('Не обычный файл: {p}', { p: e.path }), tr('Каналы, сокеты и устройства не передаются.')];
    // The core's reason is a lowercase note, as in the log.
    case 'DIRECT_FAILED': return [tr('Напрямую не получилось'), directReason(e.reason ?? '').replace(/^./, c => c.toUpperCase())];
    default:
      if (e.code.startsWith('IMPORT_')) return [importWords(e as ImportFail).title, ''];
      return [tr('Ошибка SSH'), e.reason ?? ''];
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

/** Terminal input from this size on (a paste) keeps its place in line by waiting for the reply. */
const BIG_WRITE = 4096;
/** A file source as the core takes it. */
const where = (src: Src, user: string) => (isLocal(src) ? 'local' : { server: src, user });


/** A queue item as the core sends it: the error is a code with facts. */
type CoreItem = Omit<TransferItem, 'err'> & { err: CoreError | null };
const item = (i: CoreItem): TransferItem => {
  const [title, text] = i.err ? words(i.err) : ['', ''];
  const twin = i.err?.code === 'NAME_CLASH' && i.err.path === i.name;
  return { ...i, err: text ? `${title}. ${text}` : title, twin, code: i.err?.code as ErrCode | undefined };
};

export class TauriBackend extends MockBackend {
  override readonly kind = 'tauri';
  /** The queue as the core last sent it, in its order. */
  private mirror = new Map<number, TransferItem>();

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
    // The core sends the items that changed; the mirror keeps the order of the queue.
    void listen<{ up: CoreItem[]; del: number[]; reset: boolean }>('transfer://state', e => {
      // A reset is the whole list: rows deleted while the snapshot was on its way go too.
      if (e.payload.reset) this.mirror.clear();
      for (const id of e.payload.del) this.mirror.delete(id);
      for (const i of e.payload.up) this.mirror.set(i.id, item(i));
      this.emit('transfer', [...this.mirror.values()]);
    });
    void listen<BatchReport>('transfer://batch', e => this.emit('batch', e.payload));
    void listen<{ src: Src; path: string }>('fs://changed', e => this.emit('fs', e.payload.src, e.payload.path));
    type EditAt = { server: ServerId; user: string; path: string };
    void listen<EditAt & { uploaded: boolean }>('edit://changed', e => this.emit('edit', e.payload.server, e.payload.user, e.payload.path, e.payload.uploaded));
    void listen<EditAt>('edit://conflict', e => this.emit('editConflict', e.payload.server, e.payload.user, e.payload.path));
    void listen<EditAt & { err: CoreError }>('edit://failed', e => this.emit('editFailed', e.payload.server, e.payload.user, e.payload.path, uiError(e.payload.err)));
    void listen<{ id: number; groups: { path: string; entries: Entry[] }[] }>('drop://files', e => {
      this.dropWait.get(e.payload.id)?.(e.payload.groups.map(g => ({ from: { src: 'local', user: '', path: g.path }, entries: g.entries })));
    });
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

  /* ---------- Editing in an external program ---------- */
  override editOpen(src: Src, user: string, path: string, editor: string, autosave: boolean) {
    return call<void>('edit_open', { src: where(src, user), path, editor, autosave });
  }
  override editUpload(src: Src, user: string, path: string, force = false) { return call<void>('edit_upload', { src: where(src, user), path, force }); }
  override editReload(src: Src, user: string, path: string) { return call<void>('edit_reload', { src: where(src, user), path }); }
  override editEditors() { return call<string[]>('edit_editors'); }
  override editAutosave(on: boolean) { void invoke('edit_autosave', { on }); }
  override editUnsent() { return call<{ server: string; name: string }[]>('edit_unsent'); }

  /* ---------- Import and export ---------- */
  override importRead(text: string) { return call<ImportServer[]>('import_read', { text }); }
  override exportWrite(json: string) { return call<string | null>('export_write', { json }); }

  /* ---------- Transfers: the core owns the queue and keeps it on disk ---------- */
  /** WebView2 gives the core the real paths of the dropped files; the core answers with their rows. */
  override dropFiles(files: File[]) {
    const wv = (window as { chrome?: { webview?: { postMessageWithAdditionalObjects?: (msg: string, objs: File[]) => void } } }).chrome?.webview;
    if (!files.length) return Promise.resolve([]);
    if (!wv?.postMessageWithAdditionalObjects) return Promise.reject(fail('EINVAL', tr('Перетаскивание из файлового менеджера пока только в Windows')));
    const id = ++this.drops;
    return new Promise<{ from: Endpoint; entries: Entry[] }[]>((resolve, reject) => {
      const timer = setTimeout(() => { this.dropWait.delete(id); reject(fail('EINVAL', tr('Файлы не прочитались'))); }, 15000);
      this.dropWait.set(id, list => { clearTimeout(timer); this.dropWait.delete(id); resolve(list); });
      wv.postMessageWithAdditionalObjects!(JSON.stringify({ kite: 'drop', id }), files);
    });
  }
  private drops = 0;
  private dropWait = new Map<number, (list: { from: Endpoint; entries: Entry[] }[]) => void>();

  override transferEnqueue(from: Endpoint, entries: Entry[], to: Endpoint, move: boolean, policy: ConflictPolicy) {
    const picks = entries.map(e => ({ name: e.name, dir: e.dir, size: e.size }));
    return call<number>('transfer_enqueue', { from, picks, to, mv: move, policy });
  }
  override async transferList() {
    const list = (await call<CoreItem[]>('transfer_list')).map(item);
    this.mirror = new Map(list.map(i => [i.id, i]));
    // An update that crossed the reply was just overwritten: the core sends the whole list again.
    void invoke('transfer_resync');
    return list;
  }
  override transferPause(id: number) { void invoke('transfer_pause', { id }); }
  override transferResume(id: number) { void invoke('transfer_resume', { id }); }
  override transferCancel(id: number) { void invoke('transfer_cancel', { id }); }
  override transferPauseAll(pause: boolean) { void invoke('transfer_pause_all', { pause }); }
  override transferRetry(id: number, asRoot = false) { void invoke('transfer_retry', { id, asRoot }); }
  override transferClearDone() { void invoke('transfer_clear_done'); }
  override transferSetParallel(n: number) { void invoke('transfer_set_parallel', { n }); }
  override transferKeepTime(keep: boolean) { void invoke('transfer_keep_time', { keep }); }
  override transferDirect(on: boolean) { void invoke('transfer_direct', { on }); }
  override transferRelay(ids: number[]) { void invoke('transfer_relay', { ids }); }
  override transferDetach(server: ServerId, user: string | undefined, cancel: boolean) {
    return call<void>('transfer_detach', { server, user: user ?? null, cancel });
  }
  /** The core loads its own queue at start. */
  override transferRestore() {}

  /* ---------- Terminal ---------- */
  override async ptyOpen(src: Src, user: string, cwd: string, shell: string, cols: number, rows: number,
    onData: (data: string | Uint8Array) => void, onExit: (reason: PtyExit) => void) {
    if (!isLocal(src)) {
      const s = this.servers.get(src);
      if (!s) throw { code: 'ENOENT', title: tr('Сервер удалён из списка'), text: '' } satisfies BackendError;
      await this.sshConnect(s, user);
    }
    // One ordered channel: bytes as ArrayBuffers (xterm keeps UTF-8 split between chunks), then `{exit}`.
    // Messages can beat the reply with the id: they wait until the caller has it.
    let held: (() => void)[] | null = [];
    const deliver = (fn: () => void) => (held ? held.push(fn) : fn());
    const channel = new Channel<ArrayBuffer | { exit: PtyExit }>(m => {
      if (m instanceof ArrayBuffer) { const b = new Uint8Array(m); deliver(() => onData(b)); } else deliver(() => onExit(m.exit));
    });
    const id = await call<number>('pty_open', { src: where(src, user), cwd, shell, cols, rows, onData: channel });
    setTimeout(() => { const queue = held ?? []; held = null; queue.forEach(fn => fn()); }, 0);
    return id;
  }
  /** Per terminal: the last big write still on its way. */
  private bigWrite = new Map<number, Promise<unknown>>();
  /**
   * Keys go out at once: one sync command each keeps them in order. A big paste is a separate request
   * the webview may deliver later than the next key, so writes after it wait for its reply.
   */
  override ptyWrite(id: number, data: string) {
    const prev = this.bigWrite.get(id);
    const send = () => invoke('pty_write', { id, data });
    if (!prev && data.length < BIG_WRITE) { void send(); return; }
    const p = (prev ?? Promise.resolve()).then(send, send).catch(() => undefined);
    this.bigWrite.set(id, p);
    void p.then(() => { if (this.bigWrite.get(id) === p) this.bigWrite.delete(id); });
  }
  override ptyResize(id: number, cols: number, rows: number) { void invoke('pty_resize', { id, cols, rows }); }
  override ptyPause(id: number, paused: boolean) { void invoke('pty_pause', { id, paused }); }
  override ptyClose(id: number) { this.bigWrite.delete(id); void invoke('pty_close', { id }); }
  // A shell that fails to start here fails again, with its reason, in the tab that asks for it.
  override ptyPrewarm(cwd: string, shell: string) { invoke('pty_prewarm', { cwd, shell }).catch(() => undefined); }
}
