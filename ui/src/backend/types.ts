/**
 * The only door from the UI to the core. `MockBackend` simulates the core in memory,
 * `TauriBackend` (later stages) calls the Rust commands of the same names.
 */

/** Server id. Everything internal points to the id, never to the code, so renaming keeps all memory. */
export type ServerId = string;
/** A file source: this PC or a server (the user travels next to it). */
export type Src = 'local' | ServerId;

export interface ServerUser {
  name: string;
  /** 'auto' follows the server's key mode; otherwise a key file name from ~/.ssh */
  key: string;
}

export interface Server {
  id: ServerId;
  code: string;
  group: string;
  description: string;
  host: string;
  port: number;
  users: ServerUser[];
  defaultUser: string;
  keyMode: 'auto' | 'file';
  keyFile: string;
  /** Password-only server; the password itself lives in the OS keychain. */
  authPass: boolean;
  startDir: string;
  keepalive: number;
}

export interface Entry {
  name: string;
  dir: boolean;
  /** Symbolic link: shown with a mark, `dir` follows the target, a broken link is a file. */
  link?: boolean;
  size: number;
  mtime: number;
  /** rwx string without the type letter on servers, empty locally */
  perm: string;
  owner: string;
  group: string;
  hidden?: boolean;
}

export type ErrCode =
  | 'ENOENT' | 'ENOTDIR' | 'EACCES' | 'EEXIST' | 'EINVAL' | 'BAD_PATH' | 'NO_EXEC'
  | 'TIMEOUT' | 'NET' | 'DROPPED' |'AUTH_KEY' | 'AUTH_PASS' | 'NO_SECRET' | 'AUTH_MULTI' | 'AUTH_METHODS' | 'HOSTKEY_REJECTED' | 'HOSTKEY_CHANGED'
  | 'KEY_FILE' | 'NO_PUBKEY' | 'KEY_CHECK' | 'PASSPHRASE' | 'KEYCHAIN' | 'SSH' | 'CANCELLED' | 'DISCONNECTED'
  /** The operation arrives in a later stage. */
  | 'NOT_YET';

/** Every failure crossing the boundary. `title` and `text` are ready for the UI. */
export interface BackendError {
  code: ErrCode;
  title: string;
  text: string;
}
export const isBackendError = (e: unknown): e is BackendError =>
  !!e && typeof e === 'object' && 'code' in e && 'title' in e;

export type SessionState = 'wait' | 'live' | 'bad';
/** The shell exited (the tab closes), the connection went away, or the server refused the terminal. */
export type PtyExit = 'exit' | 'disconnected' | 'refused';

export interface HostKeyRequest {
  reqId: number;
  mode: 'tofu' | 'changed';
  server: ServerId;
  code: string;
  host: string;
  port: number;
  /** ED25519, ECDSA, RSA: the server's file to compare with is ssh_host_<lowercase>_key.pub. */
  algo: string;
  fp: string;
  old?: string;
  /** A new host whose address already has a key of another type on record. */
  others: boolean;
}

/** A key file with a passphrase and not in the agent. A wrong phrase rejects the reply; the dialog stays. */
export interface PassphraseRequest {
  reqId: number;
  server: ServerId;
  code: string;
  user: string;
  /** Key file as the user knows it: ~/.ssh/work_ed25519 */
  file: string;
  /** The OpenSSH agent runs: the "add to agent" box is available. */
  agent: boolean;
}

export interface LogRow {
  id: number;
  /** HH:MM:SS.mmm */
  time: string;
  src: string;
  tag: string;
  text: string;
  note: string;
  state: 'run' | 'ok' | 'err';
}

export interface AgentStatus {
  running: boolean;
  keys: string[];
}

export interface Endpoint {
  src: Src | 'ext';
  user: string;
  path: string;
}

export type ConflictPolicy = 'replace' | 'both' | 'skip';
export type TransferState = 'wait' | 'count' | 'run' | 'pause' | 'done' | 'err';

export interface TransferItem {
  id: number;
  batch: number;
  name: string;
  /** Target name, differs from `name` with "keep both" */
  tname: string;
  dir: boolean;
  size: number;
  done: number;
  speed: number;
  from: Endpoint;
  to: Endpoint;
  move: boolean;
  state: TransferState;
  err: string;
  /** Lack of rights on the target: the queue offers "as root" */
  denied?: boolean;
}

export interface BatchReport {
  batch: number;
  move: boolean;
  to: Src;
  ok: number;
  bad: number;
  total: number;
}

export interface TextFile {
  text: string;
  binary: boolean;
  tooBig: boolean;
}

export interface ServerDraft {
  id: ServerId | null;
  code: string;
  host: string;
  port: number;
  user: string;
  keyMode: 'auto' | 'file';
  keyFile: string;
  password: string;
  /** The saved server logs in by password: without a new one the core takes the stored password. */
  authPass: boolean;
}

export interface TestResult {
  ok: boolean;
  text: string;
}

export interface Unlisten { (): void }

export interface Backend {
  readonly kind: 'mock' | 'tauri';
  readonly platform: 'windows' | 'linux';

  // SSH
  sshConnect(server: Server, user: string): Promise<void>;
  sshDisconnect(server: ServerId, user?: string): Promise<void>;
  sshTest(draft: ServerDraft): Promise<TestResult>;
  hostkeyRespond(reqId: number, trust: boolean): void;
  /** `phrase` null cancels. Rejects with PASSPHRASE when the phrase does not open the key. */
  passphraseRespond(reqId: number, phrase: string | null, addToAgent: boolean): Promise<void>;
  /** Logs in with a password before the server is saved; with `installKey` adds our key and checks key login. */
  sshFirstLogin(draft: ServerDraft, installKey: boolean): Promise<void>;
  /** Host key state for the card: fingerprint if trusted, null otherwise. */
  hostkeyKnown(host: string, port: number): Promise<string | null>;
  hostkeyForget(host: string, port: number): Promise<void>;
  /** Setting strictHost: off skips the host key check, with a warning in the log. */
  setStrictHost(on: boolean): void;

  // Secrets and keys
  secretSet(server: ServerId, user: string, password: string): Promise<void>;
  secretDelete(server: ServerId, user: string): Promise<void>;
  /** False on Linux without a Secret Service: no password-only servers there. */
  secretsAvailable(): Promise<boolean>;
  agentStatus(): Promise<AgentStatus>;
  keysList(): Promise<string[]>;
  pickKeyFile(): Promise<string | null>;

  // Files
  fsList(src: Src, user: string, path: string): Promise<Entry[]>;
  /** Resolves a typed path; `base` enables relative paths. Rejects with ENOENT, ENOTDIR, EACCES. */
  fsResolve(src: Src, user: string, raw: string, base?: string): Promise<string>;
  fsHome(src: Src, user: string): Promise<string>;
  fsMkdir(src: Src, user: string, path: string): Promise<void>;
  fsRename(src: Src, user: string, from: string, to: string): Promise<void>;
  fsDelete(src: Src, user: string, paths: string[]): Promise<void>;
  fsTrash(paths: string[]): Promise<void>;
  /** `mode` null: only owner or group change. */
  fsChmod(src: Src, user: string, paths: string[], mode: number | null, owner: string, group: string, recursive: boolean): Promise<void>;
  fsReadText(src: Src, user: string, path: string): Promise<TextFile>;
  fsDrives(): Promise<string[]>;

  // Transfers
  transferEnqueue(from: Endpoint, names: Entry[], to: Endpoint, move: boolean, policy: ConflictPolicy): Promise<number>;
  transferPause(id: number): void;
  transferResume(id: number): void;
  transferCancel(id: number): void;
  transferPauseAll(pause: boolean): void;
  transferRetry(id: number, asRoot?: boolean): void;
  transferClearDone(): void;
  transferSetParallel(n: number): void;
  transferKeepTime(keep: boolean): void;

  // Editing in an external program
  editOpen(src: Src, user: string, path: string, editor: string, autosave: boolean): Promise<void>;
  editUpload(src: Src, user: string, path: string): Promise<void>;

  // Terminal: bytes go through `onData`, not through events
  /** `shell` is the setting (pwsh, powershell, cmd, or a path on Linux); servers run the user's login shell. */
  /** Output is UTF-8 bytes from the core (xterm parses them faster than strings) or text from the mock. */
  /** `onExit` comes after the last output and only for a terminal the UI did not close itself. */
  ptyOpen(src: Src, user: string, cwd: string, shell: string, cols: number, rows: number, onData: (data: string | Uint8Array) => void, onExit: (reason: PtyExit) => void): Promise<number>;
  ptyWrite(id: number, data: string): void;
  ptyResize(id: number, cols: number, rows: number): void;
  ptyClose(id: number): void;

  // State the core already holds (after a UI reload), fetched once after subscribing
  sshSessions(): Promise<{ server: ServerId; user: string; state: SessionState }[]>;
  transferList(): Promise<TransferItem[]>;
  logTail(): Promise<LogRow[]>;

  // Events
  onSshStatus(cb: (server: ServerId, user: string, state: SessionState | null) => void): Unlisten;
  onHostkey(cb: (req: HostKeyRequest) => void): Unlisten;
  onPassphrase(cb: (req: PassphraseRequest) => void): Unlisten;
  /** Host key or passphrase requests nobody will answer any more (server disconnected): close their dialogs. */
  onPromptGone(cb: (reqIds: number[]) => void): Unlisten;
  onTransferState(cb: (items: TransferItem[]) => void): Unlisten;
  onTransferBatch(cb: (r: BatchReport) => void): Unlisten;
  /** A watched file was saved in the editor; `uploaded` is true when autosave already sent it. */
  onEditChanged(cb: (src: Src, user: string, path: string, uploaded: boolean) => void): Unlisten;
  onLogRow(cb: (row: LogRow) => void): Unlisten;
  onLogUpdate(cb: (row: LogRow) => void): Unlisten;
  /** Folder contents changed by the core (transfer finished, move removed a source). */
  onFsChanged(cb: (src: Src, path: string) => void): Unlisten;

  // Server list: the core owns the config file, the UI edits a copy and saves it whole
  serversLoad(): Promise<ServerConfig>;
  serversSave(cfg: ServerConfig): Promise<void>;

  // Files of the app data folder: settings and session are the UI's own JSON, the core only stores them
  stateLoad(doc: StateDoc): Promise<unknown | null>;
  stateSave(doc: StateDoc, data: unknown): Promise<void>;
  /** Terminal history by tab id, ANSI from addon-serialize. */
  termLoad(id: number): Promise<string | null>;
  termSave(id: number, data: string): Promise<void>;
  /** Deletes the history of every tab not in `keep`. */
  termPrune(keep: number[]): Promise<void>;
  /** Unfinished transfers from the last run: they come back paused and wait for the user. */
  transferRestore(items: TransferItem[]): void;
  /** Mock only: the demo workspace (live sessions, a running download) on the very first launch. */
  seedDemo?(): void;
}

export type StateDoc = 'settings' | 'session';

export interface ServerConfig {
  servers: Server[];
  /** Group order in the sidebar */
  groups: string[];
}
