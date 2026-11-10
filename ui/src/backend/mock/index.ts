/*
 * MockBackend: the whole core simulated in memory. No network, no real SSH.
 * It plays the demo scenarios behind the same `Backend` interface the Rust core will implement:
 * hand-shakes with a limit of three at a time, host keys, permissions checked "on the server",
 * a transfer engine, external editing and pretend shells.
 */
import type {
  AgentStatus, Backend, BackendError, BatchReport, ConflictPolicy, Endpoint, Entry, HostKeyRequest, LogRow, PassphraseRequest,
  Server, ServerConfig, ServerDraft, ServerId, SessionState, Src, StateDoc, TestResult, TextFile, TransferItem, Unlisten,
} from '../types';
import type { Store } from '../store';
import { isBackendError } from '../types';
import { fmtSize, MB, objs, sleep } from '../../util/format';
import { baseName, childOf, homeOf, isLocal, LOCAL_WIN, parentOf, within } from '../../util/paths';
import { tr } from '../../i18n';
import { DEMO_AGENT_KEYS, DEMO_GROUPS, DEMO_KEYS, DEMO_SERVERS, LOCAL_HOME_WIN, ld, rd, SAMPLE_TEXT, type DemoServer } from './data';
import { fail, fingerprint, hash, MockFs } from './fs';
import { MockShell } from './shell';

type Listener = (...args: never[]) => void;

interface Job extends TransferItem {
  entry: Entry;
  row: LogRow | null;
  connecting: boolean;
  countTicks: number;
}

interface Pty {
  shell: MockShell;
  session: string | null;
  onData: (d: string) => void;
}

const EDITORS: Record<string, string> = { system: 'системном редакторе', npp: 'Notepad++', code: 'VS Code' };
const TEXT_RE = /\.(zip|gz|tgz|tar|7z|rar|png|jpe?g|gif|webp|ico|pdf|exe|dll|bin|iso|woff2?|mp4|dump|archive|xlsx)$/i;
const MAX_TEXT = 10 * MB;
const skey = (server: ServerId, user: string) => server + '|' + user;
const hostKey = (host: string, port: number) => `${host}:${port}`;
const actualFp = (host: string, port: number) => fingerprint(hostKey(host, port));
/** Plain copy without the demo-only fields. */
const copyServer = (s: Server): Server => ({
  id: s.id, code: s.code, group: s.group, description: s.description, host: s.host, port: s.port,
  users: s.users.map(u => ({ ...u })), defaultUser: s.defaultUser, keyMode: s.keyMode, keyFile: s.keyFile,
  authPass: s.authPass, startDir: s.startDir, keepalive: s.keepalive,
});

export class MockBackend implements Backend {
  readonly kind: 'mock' | 'tauri' = 'mock';
  readonly platform = LOCAL_WIN ? ('windows' as const) : ('linux' as const);

  private fs = new MockFs();
  private listeners = new Map<string, Set<Listener>>();
  protected servers = new Map<ServerId, Server>();
  /** Demo-only facts the real core learns from the network: offline hosts, demo accounts. */
  private demo = new Map<ServerId, DemoServer>();
  private known = new Map<string, string>();
  private strict = true;
  private sessions = new Map<string, SessionState>();
  private pending = new Map<string, Promise<void>>();
  private secrets = new Map<string, string>();
  private hostReqs = new Map<number, (trust: boolean) => void>();
  private reqId = 0;
  private slots = 0;
  private slotWait: (() => void)[] = [];
  private log: LogRow[] = [];
  private logId = 0;
  private jobs: Job[] = [];
  private jobId = 0;
  private batchId = 0;
  private reported = new Set<number>();
  private parallel = 3;
  private keepTime = true;
  private timer = 0;
  private ptys = new Map<number, Pty>();
  private ptyId = 0;

  /** Storage is real: the Tauri core in the window, localStorage in a browser tab. */
  constructor(protected store: Store) {
    for (const s of DEMO_SERVERS) {
      this.servers.set(s.id, s);
      this.demo.set(s.id, s);
      const k = hostKey(s.host, s.port);
      this.known.set(k, s.hostKeyChanged ? fingerprint('old|' + k) : actualFp(s.host, s.port));
    }
  }

  /* ---------- Events ---------- */
  private on(name: string, cb: Listener): Unlisten {
    let set = this.listeners.get(name);
    if (!set) this.listeners.set(name, (set = new Set()));
    set.add(cb);
    return () => set.delete(cb);
  }
  protected emit(name: string, ...args: unknown[]) {
    for (const cb of this.listeners.get(name) ?? []) (cb as (...a: unknown[]) => void)(...args);
  }
  onSshStatus(cb: (server: ServerId, user: string, state: SessionState | null) => void) { return this.on('ssh', cb); }
  onHostkey(cb: (req: HostKeyRequest) => void) { return this.on('hostkey', cb); }
  onPassphrase(cb: (req: PassphraseRequest) => void) { return this.on('passphrase', cb); }
  onPromptGone(cb: (reqIds: number[]) => void) { return this.on('promptGone', cb); }
  onTransferState(cb: (items: TransferItem[]) => void) { return this.on('transfer', cb); }
  onTransferBatch(cb: (r: BatchReport) => void) { return this.on('batch', cb); }
  onPtyExit(cb: (id: number, reason: string) => void) { return this.on('ptyExit', cb); }
  onEditChanged(cb: (src: Src, user: string, path: string, uploaded: boolean) => void) { return this.on('edit', cb); }
  onLogRow(cb: (row: LogRow) => void) { return this.on('log', cb); }
  onLogUpdate(cb: (row: LogRow) => void) { return this.on('logUpdate', cb); }
  onFsChanged(cb: (src: Src, path: string) => void) { return this.on('fs', cb); }

  async sshSessions() { return [...this.sessions].map(([k, state]) => { const [server, user] = k.split('|'); return { server, user, state }; }); }
  async transferList() { return this.jobs.map(j => this.view(j)); }
  async logTail() { return this.log.map(r => ({ ...r })); }

  protected groups = [...DEMO_GROUPS];
  /** The saved list wins; without one the demo servers stand in until the first save. */
  async serversLoad(): Promise<ServerConfig> {
    const saved = await this.store.serversLoad();
    if (saved) this.useServers(saved);
    return { servers: [...this.servers.values()].map(copyServer), groups: [...this.groups] };
  }
  async serversSave(cfg: ServerConfig) {
    this.useServers(cfg);
    await this.store.serversSave({ servers: [...this.servers.values()].map(copyServer), groups: [...this.groups] });
  }
  protected useServers(cfg: ServerConfig) {
    this.servers = new Map(cfg.servers.map(s => [s.id, copyServer(s)]));
    this.groups = [...cfg.groups];
    for (const id of [...this.demo.keys()]) if (!this.servers.has(id)) this.demo.delete(id);
  }

  /* ---------- App data files ---------- */
  stateLoad(doc: StateDoc) { return this.store.load(doc); }
  stateSave(doc: StateDoc, data: unknown) { return this.store.save(doc, data); }
  termLoad(id: number) { return this.store.termLoad(id); }
  termSave(id: number, data: string) { return this.store.termSave(id, data); }
  termPrune(keep: number[]) { return this.store.termPrune(keep); }

  /* ---------- Log ---------- */
  private label(src: Src | 'ext') {
    if (src === 'ext') return tr('Проводник');
    return isLocal(src) ? tr('Этот ПК') : this.servers.get(src)?.code ?? src;
  }
  private stamp() {
    const d = new Date(), z = (n: number, w = 2) => String(n).padStart(w, '0');
    return `${z(d.getHours())}:${z(d.getMinutes())}:${z(d.getSeconds())}.${z(d.getMilliseconds(), 3)}`;
  }
  private add(src: Src | 'ext' | string, tag: string, text: string, note = '', state: LogRow['state'] = 'ok', raw = false): LogRow {
    const row: LogRow = { id: ++this.logId, time: this.stamp(), src: raw ? src : this.label(src), tag, text, note, state };
    this.log.push(row);
    if (this.log.length > 1000) this.log.splice(0, this.log.length - 1000);
    this.emit('log', { ...row });
    return row;
  }
  private done(row: LogRow, note: string, state: LogRow['state'] = 'ok') {
    row.note = note; row.state = state;
    this.emit('logUpdate', { ...row });
  }
  /** A request whose reply arrives a moment later, like a real SFTP round trip. */
  private async op<T>(src: Src, tag: string, text: string, work: () => T, reply: (r: T) => string): Promise<T> {
    const row = this.add(src, tag, text, '', 'run'), ms = isLocal(src) ? 1 + Math.floor(Math.random() * 4) : 15 + Math.floor(Math.random() * 70);
    await sleep(ms);
    try {
      const r = work();
      this.done(row, `${reply(r)} · ${ms} ${tr('мс')}`);
      return r;
    } catch (e) {
      this.done(row, isBackendError(e) ? e.title : String(e), 'err');
      throw e;
    }
  }
  private tagOf(src: Src) { return isLocal(src) ? 'FS' : 'SFTP'; }

  /* ---------- SSH ---------- */
  private setState(server: ServerId, user: string, state: SessionState | null) {
    const k = skey(server, user);
    if (state) this.sessions.set(k, state); else this.sessions.delete(k);
    this.emit('ssh', server, user, state);
  }
  private async slot() {
    while (this.slots >= 3) await new Promise<void>(r => this.slotWait.push(r));
    this.slots++;
  }
  private release() {
    this.slots--;
    this.slotWait.shift()?.();
  }
  private isOffline(host: string) {
    return [...this.demo.values()].some(d => d.offline && d.host === host);
  }
  private askHostKey(server: ServerId, code: string, host: string, port: number): Promise<boolean> {
    const k = hostKey(host, port), fp = actualFp(host, port), old = this.known.get(k);
    if (old === fp) return Promise.resolve(true);
    if (!this.strict) {
      this.add(code, 'KEY', tr('Ключ хоста не проверяется'), tr('проверка выключена'), 'ok', true);
      return Promise.resolve(true);
    }
    const req: HostKeyRequest = { reqId: ++this.reqId, mode: old ? 'changed' : 'tofu', server, code, host, port, algo: 'ED25519', fp, others: false, ...(old ? { old } : {}) };
    return new Promise(res => {
      this.hostReqs.set(req.reqId, trust => {
        if (trust) this.known.set(k, fp);
        res(trust);
      });
      this.emit('hostkey', req);
    });
  }
  hostkeyRespond(reqId: number, trust: boolean) {
    const r = this.hostReqs.get(reqId);
    this.hostReqs.delete(reqId);
    r?.(trust);
  }
  /** The demo keys have no passphrase: the mock never asks. */
  async passphraseRespond(_reqId: number, _phrase: string | null, _addToAgent: boolean) { /* nothing pending */ }

  sshConnect(server: Server, user: string): Promise<void> {
    const k = skey(server.id, user);
    const running = this.pending.get(k);
    if (running) return running;
    const p = this.connect(server, user).finally(() => this.pending.delete(k));
    this.pending.set(k, p);
    return p;
  }

  private async connect(server: Server, user: string) {
    const s = this.servers.get(server.id) ?? server, code = s.code, addr = `${s.host}:${s.port}`;
    if (this.sessions.get(skey(s.id, user)) === 'live') {
      const row = this.add(s.id, 'SSH', tr('Проверка соединения · {u}', { u: user }), '', 'run');
      await sleep(120);
      this.done(row, tr('активно'));
      return;
    }
    this.setState(s.id, user, 'wait');
    await this.slot();
    let row: LogRow;
    try {
      row = this.add(s.id, 'TCP', tr('Подключение к {addr}', { addr }), '', 'run');
      await sleep(140 + Math.random() * 120);
      if (this.isOffline(s.host)) {
        await sleep(2000);
        this.done(row, tr('нет ответа за 10 с'), 'err');
        this.setState(s.id, user, 'bad');
        throw fail('TIMEOUT', tr('Сервер не отвечает'), tr('{addr}: нет ответа за 10 с. Проверьте адрес или сеть.', { addr }));
      }
      this.done(row, tr('соединение установлено'));
      this.add(s.id, 'SSH', 'Сервер OpenSSH_9.2p1, протокол 2.0');
      await sleep(30 + Math.random() * 40);
      this.add(s.id, 'KEX', 'curve25519-sha256 · chacha20-poly1305@openssh.com');
      this.add(s.id, 'KEY', `ssh-ed25519 ${actualFp(s.host, s.port)}`);
    } finally {
      this.release();
    }
    // The prompt does not hold a hand-shake slot: other servers keep connecting meanwhile.
    if (!(await this.askHostKey(s.id, code, s.host, s.port))) {
      this.add(s.id, 'KEY', tr('Ключ хоста не подтверждён'), tr('отменено'), 'err');
      this.setState(s.id, user, 'bad');
      throw fail('HOSTKEY_REJECTED', tr('Подключение отменено'), tr('Ключ хоста не подтверждён.'));
    }
    await this.slot();
    try {
      await sleep(40 + Math.random() * 50);
      const demo = this.demo.get(s.id);
      if (s.authPass) {
        const pass = this.secrets.get(skey(s.id, user));
        if (!pass) {
          this.add(s.id, 'AUTH', `password · ${user}`, tr('пароль не сохранён'), 'err');
          this.setState(s.id, user, 'bad');
          throw fail('NO_SECRET', tr('Нет пароля для {u}', { u: user }), tr('Сервер входит только по паролю, а в системном хранилище его нет. Укажите пароль в карточке сервера.'));
        }
        if (pass === 'wrong') {
          this.add(s.id, 'AUTH', `password · ${user}`, tr('неверный пароль'), 'err');
          this.setState(s.id, user, 'bad');
          throw fail('AUTH_PASS', tr('Неверный пароль'), tr('Сервер {c} не принял пароль для {u}.', { c: code, u: user }));
        }
        this.add(s.id, 'AUTH', `password · ${user}`);
      } else {
        if (demo && !demo.users.some(u => u.name === user)) {
          this.add(s.id, 'AUTH', `publickey · ed25519 · ${user}`, tr('ключ не принят'), 'err');
          this.setState(s.id, user, 'bad');
          throw fail('AUTH_KEY', tr('Сервер не принял ключ'), tr('{u}@{c}: ни один ключ не подошёл. Добавьте ключ на сервер или войдите по паролю.', { u: user, c: code }));
        }
        this.add(s.id, 'AUTH', `publickey · ed25519 · ${user}`);
      }
      this.add(s.id, 'AUTH', tr('Вход выполнен'));
      await sleep(30 + Math.random() * 40);
      this.add(s.id, 'SFTP', tr('Подсистема запущена, протокол v3'));
      this.fs.ensureHome(s.id, user);
      this.setState(s.id, user, 'live');
    } finally {
      this.release();
    }
  }

  async sshDisconnect(server: ServerId, user?: string) {
    for (const k of [...this.sessions.keys()]) {
      const [id, u] = k.split('|');
      if (id !== server || (user !== undefined && u !== user)) continue;
      this.setState(id, u, null);
      this.add(id, 'SSH', tr('Соединение закрыто · {u}', { u }));
      for (const [pid, pty] of this.ptys) if (pty.session === k) { this.ptys.delete(pid); this.emit('ptyExit', pid, 'disconnected'); }
    }
  }

  async sshTest(d: ServerDraft): Promise<TestResult> {
    await sleep(700 + Math.random() * 400);
    if (!d.host) return { ok: false, text: tr('Не указан адрес') };
    if (this.isOffline(d.host)) { await sleep(1200); return { ok: false, text: tr('Нет ответа от {addr}', { addr: `${d.host}:${d.port}` }) }; }
    if (d.password === 'wrong') return { ok: false, text: tr('Неверный пароль') };
    const was = d.id ? this.servers.get(d.id) : null;
    if (d.password || was?.authPass) return { ok: true, text: tr('Сервер доступен · вход по паролю') };
    return { ok: true, text: tr('Подключение есть · {u} · {ms} мс', { u: d.user || 'root', ms: 30 + Math.floor(Math.random() * 60) }) };
  }

  async sshFirstLogin(d: ServerDraft, installKey: boolean) {
    if (!(await this.askHostKey(d.id ?? '', d.code, d.host, d.port))) throw fail('HOSTKEY_REJECTED', tr('Подключение отменено'), tr('Ключ хоста не подтверждён.'));
    await sleep(700);
    if (this.isOffline(d.host)) throw fail('TIMEOUT', tr('Нет ответа от {addr}', { addr: `${d.host}:${d.port}` }));
    if (d.password === 'wrong') throw fail('AUTH_PASS', tr('Неверный пароль'));
    this.add(d.code, 'AUTH', `password · ${d.user}`, '', 'ok', true);
    if (installKey) this.add(d.code, 'KEY', tr('Ключ {k} добавлен в authorized_keys', { k: d.keyFile || 'id_ed25519' }), '', 'ok', true);
  }

  async hostkeyKnown(host: string, port: number) { return this.known.get(hostKey(host, port)) ?? null; }
  async hostkeyForget(host: string, port: number) { this.known.delete(hostKey(host, port)); }
  setStrictHost(on: boolean) { this.strict = on; }

  /* ---------- Secrets and keys ---------- */
  async secretSet(server: ServerId, user: string, password: string) { this.secrets.set(skey(server, user), password); }
  async secretDelete(server: ServerId, user: string) { this.secrets.delete(skey(server, user)); }
  async secretsAvailable() { return true; }
  async agentStatus(): Promise<AgentStatus> { return { running: true, keys: [...DEMO_AGENT_KEYS] }; }
  async keysList() { return [...DEMO_KEYS]; }
  async pickKeyFile(): Promise<string | null> { await sleep(300); return LOCAL_WIN ? '%USERPROFILE%\\.ssh\\work_ed25519' : '~/.ssh/work_ed25519'; }

  /* ---------- Files ---------- */
  private live(src: Src, user: string) {
    if (isLocal(src)) return;
    if (this.sessions.get(skey(src, user)) !== 'live') throw fail('DISCONNECTED', tr('Нет соединения'), tr('{u}@{c} не подключён.', { u: user, c: this.label(src) }));
  }
  private writable(src: Src, user: string, dir: string, what: string) {
    if (!this.fs.canWrite(src, user, dir)) throw fail('EACCES', tr('Нет прав на запись в {p} для {u}', { p: dir, u: user }), what);
  }

  async fsList(src: Src, user: string, path: string): Promise<Entry[]> {
    this.live(src, user);
    return this.op(src, this.tagOf(src), `${isLocal(src) ? 'LIST' : 'READDIR'} ${path}`, () => {
      this.fs.resolve(src, user, path);
      return this.fs.dir(src, path).map(e => ({ ...e }));
    }, list => objs(list.length));
  }

  async fsResolve(src: Src, user: string, raw: string, base?: string) {
    this.live(src, user);
    try { return this.fs.resolve(src, user, raw, base); } catch (e) {
      this.add(src, this.tagOf(src), `STAT ${raw}`, isBackendError(e) ? e.title : String(e), 'err');
      throw e;
    }
  }

  async fsHome(src: Src, user: string) {
    if (isLocal(src)) return LOCAL_WIN ? LOCAL_HOME_WIN : '/home/user';
    return homeOf(user);
  }

  async fsMkdir(src: Src, user: string, path: string) {
    this.live(src, user);
    const dir = parentOf(src, path)!, name = baseName(src, path);
    await this.op(src, this.tagOf(src), `MKDIR ${path}`, () => {
      this.writable(src, user, dir, '');
      const list = this.fs.dir(src, dir);
      if (list.some(e => e.name === name)) throw fail('EEXIST', tr('{v} уже есть в этой папке', { v: name }));
      const e = isLocal(src) ? ld(name, 0) : rd(name, 0, user);
      e.mtime = Date.now();
      list.push(e);
      this.fs.set(src, path, []);
    }, () => tr('готово'));
  }

  async fsRename(src: Src, user: string, from: string, to: string) {
    this.live(src, user);
    const dir = parentOf(src, from)!, oldName = baseName(src, from), newName = baseName(src, to);
    await this.op(src, this.tagOf(src), `RENAME ${from} → ${newName}`, () => {
      this.writable(src, user, dir, '');
      const list = this.fs.dir(src, dir);
      if (list.some(e => e.name === newName)) throw fail('EEXIST', tr('{v} уже есть в этой папке', { v: newName }));
      const e = list.find(x => x.name === oldName);
      if (!e) throw fail('ENOENT', tr('Нет такого файла: {p}', { p: from }));
      e.name = newName;
      if (e.dir) this.fs.moveTree(src, from, to);
    }, () => tr('готово'));
  }

  async fsDelete(src: Src, user: string, paths: string[]) {
    this.live(src, user);
    for (const path of paths) {
      const dir = parentOf(src, path)!, name = baseName(src, path), list = this.fs.dir(src, dir), e = list.find(x => x.name === name);
      await this.op(src, this.tagOf(src), `${e?.dir ? 'RMDIR' : 'UNLINK'} ${path}`, () => {
        if (!this.fs.canWrite(src, user, dir)) throw fail('EACCES', tr('Нет прав на удаление в {p} для {u}', { p: dir, u: user }));
        const k = list.findIndex(x => x.name === name);
        if (k >= 0) list.splice(k, 1);
      }, () => tr('готово'));
    }
  }

  async fsTrash(paths: string[]) {
    for (const path of paths) {
      const dir = parentOf('local', path)!, name = baseName('local', path), list = this.fs.dir('local', dir);
      await this.op('local', 'FS', `TRASH ${path}`, () => {
        const k = list.findIndex(x => x.name === name);
        if (k >= 0) list.splice(k, 1);
      }, () => tr('готово'));
    }
  }

  async fsChmod(src: Src, user: string, paths: string[], mode: number, owner: string, group: string, recursive: boolean) {
    this.live(src, user);
    const oct = mode.toString(8).padStart(3, '0');
    const tail = [...oct].map(c => { const n = +c; return (n & 4 ? 'r' : '-') + (n & 2 ? 'w' : '-') + (n & 1 ? 'x' : '-'); }).join('');
    const entries = paths.map(p => ({ p, e: this.fs.find(src, p) }));
    if (user !== 'root' && entries.some(x => x.e && x.e.owner !== user)) {
      this.add(src, 'SFTP', `SETSTAT ${parentOf(src, paths[0])}`, tr('чужие файлы, нужен root (сейчас {u})', { u: user }), 'err');
      throw fail('EACCES', tr('Права чужих файлов меняет только root (сейчас {u})', { u: user }));
    }
    for (const { p, e } of entries) {
      await this.op(src, 'SFTP', `SETSTAT ${p} ${oct} ${owner}:${group}${recursive ? ' -R' : ''}`, () => {
        if (!e) throw fail('ENOENT', tr('Нет такого файла: {p}', { p }));
        e.perm = tail; e.owner = owner; e.group = group;
      }, () => tr('готово'));
    }
  }

  async fsReadText(src: Src, user: string, path: string): Promise<TextFile> {
    this.live(src, user);
    const e = this.fs.find(src, path), name = baseName(src, path);
    return this.op(src, this.tagOf(src), `READ ${path}`, () => {
      if (!e) throw fail('ENOENT', tr('Нет такого файла: {p}', { p: path }));
      if (!this.fs.canRead(src, user, e)) throw fail('EACCES', tr('Нет прав на чтение для {u}', { u: user }), path);
      if (TEXT_RE.test(name)) return { text: '', binary: true, tooBig: false };
      if (e.size > MAX_TEXT) return { text: '', binary: false, tooBig: true };
      return { text: SAMPLE_TEXT[name.split('.').pop()!.toLowerCase()] ?? `# ${name}\n`, binary: false, tooBig: false };
    }, () => fmtSize(e?.size ?? 0));
  }

  async fsDrives() { return LOCAL_WIN ? ['C:\\', 'D:\\'] : ['/']; }

  /* ---------- Transfers ---------- */
  private view(j: Job): TransferItem {
    const { entry: _e, row: _r, connecting: _c, countTicks: _t, ...item } = j;
    return { ...item, from: { ...item.from }, to: { ...item.to } };
  }
  private push() { this.emit('transfer', this.jobs.map(j => this.view(j))); }
  private wake() { if (!this.timer) this.timer = window.setInterval(() => this.tick(), 200); }

  private freeName(list: Entry[], name: string) {
    const dot = name.lastIndexOf('.'), stem = dot > 0 ? name.slice(0, dot) : name, ext = dot > 0 ? name.slice(dot) : '';
    for (let n = 2; ; n++) { const c = `${stem} (${n})${ext}`; if (!list.some(e => e.name === c)) return c; }
  }

  async transferEnqueue(from: Endpoint, entries: Entry[], to: Endpoint, move: boolean, policy: ConflictPolicy) {
    if (!entries.length) return 0;
    const toSrc = to.src as Src;
    if (from.src !== 'ext' && from.src === to.src && from.path === to.path) throw fail('EINVAL', tr('Это та же папка'));
    for (const e of entries) {
      if (!e.dir || from.src === 'ext' || from.src !== to.src) continue;
      if (within(from.src, childOf(from.src, from.path, e.name), to.path)) throw fail('EINVAL', tr('Нельзя поместить {n} в саму себя', { n: e.name }));
    }
    const target = this.fs.dir(toSrc, to.path), batch = ++this.batchId;
    for (const e of entries) {
      let tname = e.name;
      if (target.some(t => t.name === e.name)) {
        if (policy === 'skip') continue;
        if (policy === 'both') tname = this.freeName(target, e.name);
      }
      const size = e.dir ? (4 + (hash(e.name + from.path) % 176)) * MB : Math.max(e.size, 1);
      this.jobs.push({
        id: ++this.jobId, batch, name: e.name, tname, dir: e.dir, size, done: 0, speed: 0,
        from: { ...from }, to: { ...to }, move, state: 'wait', err: '', entry: { ...e }, row: null, connecting: false, countTicks: 0,
      });
    }
    if (!this.jobs.some(j => j.batch === batch)) return 0;
    this.push();
    this.wake();
    return batch;
  }

  private logTransfer(j: Job) {
    const fromLocal = j.from.src === 'ext' || isLocal(j.from.src), toLocal = isLocal(j.to.src as Src);
    const op = fromLocal && toLocal ? 'COPY' : fromLocal ? 'PUT' : toLocal ? 'GET' : 'RELAY';
    return this.add(toLocal && !fromLocal ? j.from.src : j.to.src, op, `${j.tname} · ${j.from.src === 'ext' ? tr('Проводник') : j.from.path} → ${j.to.path}`, '', 'run');
  }
  private speedOf(j: Job) {
    const fl = j.from.src === 'ext' || isLocal(j.from.src), tl = isLocal(j.to.src as Src);
    return fl && tl ? 420 * MB : fl ? 11.2 * MB : tl ? 86 * MB : 42 * MB;
  }
  /** A remote end without a live session gets connected, as the core would do. */
  private ensureLive(j: Job): boolean {
    for (const end of [j.from, j.to]) {
      if (end.src === 'ext' || isLocal(end.src)) continue;
      const st = this.sessions.get(skey(end.src, end.user));
      if (st === 'live') continue;
      const s = this.servers.get(end.src);
      if (!s) { this.failJob(j, tr('Сервер удалён из списка')); return false; }
      j.connecting = true;
      this.sshConnect(s, end.user).then(() => { j.connecting = false; }, (e: BackendError) => { j.connecting = false; this.failJob(j, e.title); });
      return false;
    }
    return true;
  }
  private failJob(j: Job, err: string, denied = false) {
    j.state = 'err'; j.err = err; j.speed = 0; j.denied = denied;
    j.row ??= this.logTransfer(j);
    this.done(j.row, err, 'err');
  }

  private finish(j: Job) {
    j.state = 'done'; j.done = j.size; j.speed = 0;
    if (j.row) this.done(j.row, fmtSize(j.size));
    const toSrc = j.to.src as Src, remote = !isLocal(toSrc), list = this.fs.dir(toSrc, j.to.path);
    const ne: Entry = {
      ...j.entry, name: j.tname, mtime: this.keepTime ? j.entry.mtime : Date.now(),
      perm: remote ? j.entry.perm || (j.dir ? 'rwxr-xr-x' : 'rw-r--r--') : '', owner: remote ? j.to.user : '', group: remote ? j.to.user : '',
    };
    const k = list.findIndex(x => x.name === j.tname);
    if (k >= 0) list.splice(k, 1, ne); else list.push(ne);
    if (j.dir && j.from.src !== 'ext') {
      const fromPath = childOf(j.from.src, j.from.path, j.name), toPath = childOf(toSrc, j.to.path, j.tname);
      if (j.move && j.from.src === toSrc) this.fs.moveTree(j.from.src, fromPath, toPath);
      else this.fs.copyTree(j.from.src, fromPath, toSrc, toPath, j.to.user);
    }
    this.emit('fs', toSrc, j.to.path);
    if (j.move && j.from.src !== 'ext') {
      const sl = this.fs.dir(j.from.src, j.from.path), i = sl.findIndex(x => x.name === j.name);
      if (i >= 0) sl.splice(i, 1);
      this.add(j.from.src, this.tagOf(j.from.src), `${j.dir ? 'RMDIR' : 'UNLINK'} ${childOf(j.from.src, j.from.path, j.name)}`, tr('готово'));
      this.emit('fs', j.from.src, j.from.path);
    }
  }

  private tick() {
    let running = this.jobs.filter(j => j.state === 'run' || j.state === 'count').length;
    for (const j of this.jobs) {
      if (running >= this.parallel) break;
      if (j.state !== 'wait' || j.connecting || !this.ensureLive(j)) continue;
      j.row ??= this.logTransfer(j);
      if (!this.fs.canWrite(j.to.src as Src, j.to.user, j.to.path)) {
        this.failJob(j, tr('Нет прав на запись: {u} в {p}', { u: j.to.user, p: j.to.path }), true);
        continue;
      }
      // Folders are counted first, so the progress bar knows the total.
      j.state = j.dir ? 'count' : 'run'; j.countTicks = 0;
      running++;
    }
    for (const j of this.jobs) {
      if (j.state === 'count') { if (++j.countTicks >= 3) j.state = 'run'; continue; }
      if (j.state !== 'run') continue;
      const eff = Math.max(this.speedOf(j), j.size / 20);
      j.speed = eff * (0.92 + Math.random() * 0.16);
      j.done = Math.min(j.size, j.done + j.speed * 0.2);
      if (j.done >= j.size) this.finish(j);
    }
    for (const batch of new Set(this.jobs.map(j => j.batch))) {
      if (this.reported.has(batch)) continue;
      const items = this.jobs.filter(j => j.batch === batch);
      if (items.some(j => ['wait', 'count', 'run', 'pause'].includes(j.state))) continue;
      this.reported.add(batch);
      this.emit('batch', {
        batch, move: items[0].move, to: items[0].to.src as Src, total: items.length,
        ok: items.filter(j => j.state === 'done').length, bad: items.filter(j => j.state === 'err').length,
      } satisfies BatchReport);
    }
    this.push();
    if (!this.jobs.some(j => ['wait', 'count', 'run'].includes(j.state))) { clearInterval(this.timer); this.timer = 0; }
  }

  private job(id: number) { return this.jobs.find(j => j.id === id); }
  transferRestore(items: TransferItem[]) {
    for (const q of items) {
      if (q.state === 'done' || this.job(q.id)) continue;
      const found = q.from.src === 'ext' ? undefined : this.fs.find(q.from.src, childOf(q.from.src, q.from.path, q.name));
      const entry: Entry = found ? { ...found } : { name: q.name, dir: q.dir, size: q.size, mtime: Date.now(), perm: '', owner: '', group: '' };
      // Nothing heavy restarts on its own: running and waiting items come back paused.
      const state = q.state === 'err' ? 'err' : 'pause';
      this.jobs.push({ ...q, from: { ...q.from }, to: { ...q.to }, state, speed: 0, entry, row: null, connecting: false, countTicks: 0 });
      this.jobId = Math.max(this.jobId, q.id);
      this.batchId = Math.max(this.batchId, q.batch);
    }
    // A batch with only errors left was reported before the restart.
    for (const b of new Set(this.jobs.map(j => j.batch))) if (this.jobs.every(j => j.batch !== b || j.state === 'err')) this.reported.add(b);
    this.push();
  }
  transferPause(id: number) {
    const j = this.job(id);
    if (j && ['wait', 'count', 'run'].includes(j.state)) { j.state = 'pause'; j.speed = 0; this.push(); }
  }
  transferResume(id: number) {
    const j = this.job(id);
    if (j?.state === 'pause') { j.state = 'wait'; this.reported.delete(j.batch); this.push(); this.wake(); }
  }
  transferCancel(id: number) {
    const k = this.jobs.findIndex(j => j.id === id);
    if (k < 0) return;
    const [j] = this.jobs.splice(k, 1);
    if (j.row?.state === 'run') this.done(j.row, tr('отменено'));
    this.push();
  }
  transferPauseAll(pause: boolean) {
    for (const j of this.jobs) {
      if (pause && ['wait', 'count', 'run'].includes(j.state)) { j.state = 'pause'; j.speed = 0; }
      else if (!pause && j.state === 'pause') { j.state = 'wait'; this.reported.delete(j.batch); }
    }
    this.push();
    if (!pause) this.wake();
  }
  transferRetry(id: number, asRoot = false) {
    const j = this.job(id);
    if (!j || j.state !== 'err') return;
    if (asRoot) j.to.user = 'root';
    Object.assign(j, { state: 'wait', err: '', done: 0, row: null, denied: false });
    this.reported.delete(j.batch);
    this.push();
    this.wake();
  }
  transferClearDone() {
    this.jobs = this.jobs.filter(j => !['done', 'err'].includes(j.state));
    this.push();
  }
  transferSetParallel(n: number) { this.parallel = Math.max(1, Math.min(8, n)); }
  transferKeepTime(keep: boolean) { this.keepTime = keep; }

  /* ---------- Editing in an external program ---------- */
  async editOpen(src: Src, user: string, path: string, editor: string, autosave: boolean) {
    this.live(src, user);
    const dir = parentOf(src, path)!, name = baseName(src, path), app = tr(EDITORS[editor] ?? EDITORS.system);
    const e = this.fs.find(src, path);
    if (!this.fs.canWrite(src, user, dir) || (e && !isLocal(src) && user !== 'root' && !(e.owner === user ? e.perm[1] === 'w' : e.perm[7] === 'w'))) {
      this.add(src, this.tagOf(src), `OPEN ${path}`, tr('нет прав на запись для {u}', { u: user }), 'err');
      throw fail('EACCES', tr('Нет прав на запись для {u}', { u: user }), path);
    }
    if (isLocal(src)) { this.add(src, 'EDIT', `${path} · ${app}`); return; }
    await this.op(src, 'SFTP', `READ ${path}`, () => null, () => tr('временная копия'));
    this.add(src, 'EDIT', tr('Слежу за {n} · {app}', { n: name, app }));
    // Mock: pretends the user saved the file in the editor five seconds later.
    setTimeout(async () => {
      this.add(src, 'EDIT', tr('{n} изменён в редакторе', { n: name }));
      if (autosave) await this.editUpload(src, user, path).catch(() => undefined);
      this.emit('edit', src, user, path, autosave);
    }, 5000);
  }

  async editUpload(src: Src, user: string, path: string) {
    this.live(src, user);
    const e = this.fs.find(src, path);
    await this.op(src, 'SFTP', `WRITE ${path}`, () => { if (e) e.mtime = Date.now(); }, () => (e ? fmtSize(e.size) : tr('готово')));
    this.emit('fs', src, parentOf(src, path)!);
  }

  /* ---------- Terminal ---------- */
  async ptyOpen(src: Src, user: string, cwd: string, _cols: number, _rows: number, onData: (d: string) => void) {
    let session: string | null = null, code = '';
    if (!isLocal(src)) {
      const s = this.servers.get(src);
      if (!s) throw fail('ENOENT', tr('Сервер удалён из списка'));
      await this.sshConnect(s, user);
      session = skey(src, user); code = s.code;
      try { cwd = this.fs.resolve(src, user, cwd || homeOf(user)); } catch { cwd = homeOf(user); }
    } else {
      try { cwd = this.fs.resolve('local', '', cwd || (await this.fsHome('local', ''))); } catch { cwd = await this.fsHome('local', ''); }
    }
    this.add(src, 'PTY', tr('Запуск терминала') + (user ? ' · ' + user : ''));
    const id = ++this.ptyId;
    const shell = new MockShell({
      fs: this.fs,
      out: d => this.ptys.get(id)?.onData(d),
      exit: reason => { if (this.ptys.delete(id)) this.emit('ptyExit', id, reason); },
      server: name => {
        const s = [...this.servers.values()].find(x => x.code === name);
        return s ? { src: s.id, code: s.code, defaultUser: s.defaultUser } : null;
      },
      later: (ms, fn) => { setTimeout(fn, ms); },
    }, { src, user, code, cwd });
    this.ptys.set(id, { shell, session, onData });
    shell.start(true);
    return id;
  }
  ptyWrite(id: number, data: string) { this.ptys.get(id)?.shell.write(data); }
  ptyResize() { /* the mock shell does not wrap lines itself */ }
  ptyClose(id: number) { this.ptys.delete(id); }

  /* ---------- Demo state on the first launch ---------- */
  seedDemo() {
    const web = DEMO_SERVERS[0], db = DEMO_SERVERS[4];
    for (const s of [web, db]) {
      this.sessions.set(skey(s.id, 'root'), 'live');
      this.add(s.id, 'SSH', 'Сервер OpenSSH_9.2p1, протокол 2.0');
      this.add(s.id, 'AUTH', 'publickey · ed25519 · ' + tr('вход выполнен'));
      this.add(s.id, 'SFTP', tr('Подсистема запущена, протокол v3'));
    }
    // A backup is already downloading when the app opens.
    const from: Endpoint = { src: db.id, user: 'root', path: '/home/admin/backups' };
    const e = this.fs.dir(db.id, from.path).find(x => x.name === 'shop-2026-11-06.sql.gz')!;
    const j: Job = {
      id: ++this.jobId, batch: ++this.batchId, name: e.name, tname: e.name, dir: false, size: e.size, done: e.size * 0.42, speed: 0,
      from, to: { src: 'local', user: '', path: LOCAL_WIN ? 'D:\\Backups' : '/home/user/Downloads' }, move: false, state: 'run', err: '',
      entry: { ...e }, row: null, connecting: false, countTicks: 0,
    };
    j.row = this.logTransfer(j);
    this.jobs.push(j);
    this.wake();
  }
}
