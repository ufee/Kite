//! Terminals: a local shell on a PTY (ConPTY on Windows) and a shell on an SSH channel.
//! Each has one ordered input queue; output goes to a sink as raw bytes.

use std::{
    collections::HashMap,
    io::{Read, Write},
    path::Path,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use portable_pty::{ChildKiller, CommandBuilder, PtySize, native_pty_system};
use russh::{Channel, ChannelMsg, client::Msg};
use serde::Serialize;
use tokio::{
    sync::{Notify, mpsc},
    time,
};

use crate::{
    error::{Error, Result},
    ssh::Session,
};

/// Sink for terminal output. Takes ownership so the buffer reaches the IPC layer without a copy.
pub type Output = Arc<dyn Fn(Vec<u8>) + Send + Sync>;
pub type OnExit = Box<dyn FnOnce(Exit) + Send>;
type Killer = Box<dyn ChildKiller + Send + Sync>;

/// Why a terminal ended. Only `Exited` closes the tab; the rest leave it with a red line and Enter reconnects:
/// the connection went away, the server refused the PTY or the command (`no-pty`, `PermitTTY no`),
/// or a signal killed the shell (OOM killer, `kill -9`): the tab must not just vanish then.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Exit {
    #[serde(rename = "exit")]
    Exited,
    Disconnected,
    Refused,
    Killed,
}

/// Each IPC message costs a round trip through the webview, so a flood is coalesced.
/// The first bytes after a pause still go out at once: keystroke echo never waits for a timer.
const COALESCE_WINDOW: Duration = Duration::from_millis(8);
const COALESCE_MAX: usize = 256 * 1024;
/// Output up to this many bytes per burst is never delayed, even when keys come inside
/// `COALESCE_WINDOW` (Windows rounds tokio timers up to its ~15.6 ms tick).
const INTERACTIVE_BUDGET: usize = 4 * 1024;
const LOCAL_READ_BUFFER: usize = 64 * 1024;
/// Output a paused remote terminal keeps in the core: a flood (`yes`) while the window is hidden and
/// xterm parses nothing. Past it the oldest half goes, so the reader never stops: a stalled channel
/// would stall the whole connection, the panes' SFTP with it.
const REMOTE_HOLD: usize = 8 * 1024 * 1024;
/// How long a finished local shell waits for the rest of its output before it reports the exit.
/// A background process that keeps the PTY open must not hold the exit forever.
const LOCAL_DRAIN: Duration = Duration::from_millis(500);

pub enum Command {
    Input(Vec<u8>),
    Resize(u16, u16),
    /// The local shell ended: release the PTY so the reader gets the rest and EOF.
    Close,
}

/// Input queue of a terminal. Commands stay ordered, unlike independent async IPC calls.
pub type Handle = mpsc::UnboundedSender<Command>;

/* ---------- Flow control ---------- */

/// The UI pauses output while its terminal is behind. A local reader stops, and the ConPTY pipe
/// holds the shell back. A remote reader never stops: russh hands channel data over from the session
/// loop, so a reader that stops stalls the whole connection (file panels, other tabs, keepalive),
/// not just this channel. It holds up to `REMOTE_HOLD` and drops the oldest output past it.
#[derive(Default)]
pub struct Flow {
    paused: Mutex<bool>,
    cv: Condvar,
    resumed: Notify,
}

impl Flow {
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.paused.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self, paused: bool) {
        *self.lock() = paused;
        if !paused {
            self.cv.notify_all();
            self.resumed.notify_waiters();
        }
    }

    fn is_paused(&self) -> bool {
        *self.lock()
    }

    fn wait_blocking(&self) {
        let mut paused = self.lock();
        while *paused {
            paused = self.cv.wait(paused).unwrap_or_else(|e| e.into_inner());
        }
    }

    async fn wait(&self) {
        loop {
            let resumed = self.resumed.notified();
            tokio::pin!(resumed);
            // Registered before the check: a resume in between is not lost.
            resumed.as_mut().enable();
            if !self.is_paused() {
                return;
            }
            resumed.await;
        }
    }
}

/* ---------- Registry ---------- */

/// A started terminal as the registry keeps it.
pub struct Open {
    handle: Handle,
    flow: Arc<Flow>,
    /// Ends the local shell process.
    kill: Option<Killer>,
    /// Ends the remote writer even while it waits for the channel window (a paste into a program
    /// that does not read stdin); dropping the queue alone is not seen then.
    stop: Option<Arc<Notify>>,
}

/// Terminals by id. An id is taken before the shell starts, so the exit callback knows it;
/// a shell that exits before it is registered leaves no entry behind.
#[derive(Default)]
pub struct Terms {
    next: AtomicU32,
    open: Mutex<HashMap<u32, Option<Open>>>,
    /// A local shell started ahead of the first local tab: see `Spare`.
    spare: Mutex<Option<Spare>>,
}

impl Terms {
    fn map(&self) -> MutexGuard<'_, HashMap<u32, Option<Open>>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn allocate(&self) -> u32 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.map().insert(id, None);
        id
    }

    /// False when the shell already ended or the tab was closed meanwhile: then it is closed here.
    pub fn register(&self, id: u32, open: Open) -> bool {
        let mut map = self.map();
        match map.get_mut(&id) {
            Some(slot) => {
                *slot = Some(open);
                true
            }
            None => {
                drop(map);
                close(open);
                false
            }
        }
    }

    /// The shell ended: forget it. True if it was still known (not closed by the UI).
    pub fn ended(&self, id: u32) -> bool {
        match self.map().remove(&id) {
            Some(open) => {
                if let Some(open) = open {
                    release(&open);
                }
                true
            }
            None => false,
        }
    }

    pub fn send(&self, id: u32, command: Command) {
        if let Some(Some(open)) = self.map().get(&id) {
            let _ = open.handle.send(command);
        }
    }

    pub fn pause(&self, id: u32, paused: bool) {
        if let Some(Some(open)) = self.map().get(&id) {
            open.flow.set(paused);
        }
    }

    pub fn close(&self, id: u32) {
        let open = self.map().remove(&id).flatten();
        if let Some(open) = open {
            close(open);
        }
    }

    /// The shell did not start: the id is free again.
    pub fn forget(&self, id: u32) {
        self.map().remove(&id);
    }

    fn spare_slot(&self) -> MutexGuard<'_, Option<Spare>> {
        self.spare.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Keeps a started spare shell, replacing (and ending) an older one.
    fn set_spare(&self, spare: Spare) {
        let old = self.spare_slot().replace(spare);
        if let Some(old) = old {
            close(old.open);
        }
    }

    /// The spare shell, if it runs `shell`, can go to `dir` and is still alive; any other spare is
    /// ended, so a shell never waits for a folder the panes have left.
    fn take_spare(&self, shell: &str, dir: &str) -> Option<(Open, Arc<Tap>)> {
        let spare = self.spare_slot().take()?;
        let fits = match &spare.dir {
            Some(d) => d == dir,
            // Before `WAIT_DIR` runs, keys would go to the profile (a `Read-Host` in it, say).
            None => spare.tap.ready(),
        };
        if spare.shell == shell && fits && !spare.tap.exited() {
            if spare.dir.is_none() {
                // Base64: one ASCII line whatever the folder holds (a CR, characters outside the
                // console code page).
                let line = format!("{}\r", data_encoding::BASE64.encode(dir.as_bytes()));
                let _ = spare.open.handle.send(Command::Input(line.into_bytes()));
            }
            Some((spare.open, spare.tap))
        } else {
            close(spare.open);
            None
        }
    }

    /// The spare shell ended before any tab took it.
    fn spare_ended(&self, tap: &Arc<Tap>) {
        let mut slot = self.spare_slot();
        if slot.as_ref().is_some_and(|s| Arc::ptr_eq(&s.tap, tap)) {
            slot.take();
        }
    }

    /// The spare shell printed too much to be handed out: it is ended, not kept running.
    fn drop_spare(&self, tap: &Arc<Tap>) {
        let spare = {
            let mut slot = self.spare_slot();
            if slot.as_ref().is_some_and(|s| Arc::ptr_eq(&s.tap, tap)) { slot.take() } else { None }
        };
        if let Some(spare) = spare {
            close(spare.open);
        }
    }

    /// The app is closing: the spare shell has no tab that would end it.
    pub fn close_spare(&self) {
        let spare = self.spare_slot().take();
        if let Some(spare) = spare {
            close(spare.open);
        }
    }
}

/* ---------- Spare local shell ---------- */

/// pwsh needs a few hundred ms to print its first prompt. The UI asks for one local shell once the
/// start has settled; the first local tab takes it with the profile already loaded. One spare at
/// most, and only until a tab takes it.
struct Spare {
    shell: String,
    /// The folder it started in; None when it waits for the tab's folder (`WAIT_DIR`).
    dir: Option<String>,
    open: Open,
    tap: Arc<Tap>,
}

/// Holds the spare shell's output and exit until a tab takes it, then passes them on.
struct Tap(Mutex<TapState>);

enum TapState {
    /// `ready`: a `WAIT_DIR` shell has reached its folder question.
    Held { output: Vec<u8>, exited: bool, input: Option<Handle>, answered: bool, ready: bool },
    Live { output: Output, on_exit: Option<OnExit> },
}

/// A prompt is a few hundred bytes; a spare that prints more is not idle and is not handed out.
const SPARE_HOLD: usize = 64 * 1024;
/// The window title `WAIT_DIR` sets while it waits: ConPTY passes a title on as OSC 0 and does not
/// repeat it in a redraw, so it marks the moment without leaving anything on the screen.
const READY_TITLE: &str = "kite-spare-ready";
/// ConPTY starts with the cursor inherited: it asks the terminal where the cursor is and renders
/// nothing until it hears back. A spare has no terminal yet, so the core answers for the empty one
/// a new tab has, and drops the question so that tab does not answer it a second time.
const CURSOR_QUERY: &[u8] = b"\x1b[6n";
const CURSOR_HOME: &[u8] = b"\x1b[1;1R";

impl TapState {
    fn answer_cursor(&mut self) {
        let TapState::Held { output, input: Some(input), answered: answered @ false, .. } = self else { return };
        if let Some(at) = output.windows(CURSOR_QUERY.len()).position(|w| w == CURSOR_QUERY) {
            output.drain(at..at + CURSOR_QUERY.len());
            let _ = input.send(Command::Input(CURSOR_HOME.to_vec()));
            *answered = true;
        }
    }

    /// Cuts the `READY_TITLE` sequence out of the held output and notes it.
    fn find_ready(&mut self) {
        let TapState::Held { output, ready: ready @ false, .. } = self else { return };
        let mark = READY_TITLE.as_bytes();
        let Some(at) = output.windows(mark.len()).position(|w| w == mark) else { return };
        let Some(start) = output[..at].windows(2).rposition(|w| w == b"\x1b]") else { return };
        // BEL or ST ends it; not here yet means the rest is still coming.
        let tail = &output[at + mark.len()..];
        let end = match (tail.iter().position(|&b| b == 0x07), tail.windows(2).position(|w| w == b"\x1b\\")) {
            (Some(b), Some(s)) if s < b => s + 2,
            (Some(b), _) => b + 1,
            (None, Some(s)) => s + 2,
            (None, None) => return,
        };
        output.drain(start..at + mark.len() + end);
        *ready = true;
    }
}

impl Tap {
    fn lock(&self) -> MutexGuard<'_, TapState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn exited(&self) -> bool {
        matches!(*self.lock(), TapState::Held { exited: true, .. })
    }

    fn ready(&self) -> bool {
        matches!(*self.lock(), TapState::Held { ready: true, .. })
    }

    /// The shell's input queue, known once it has started; the cursor question may already be held.
    fn set_input(&self, handle: Handle) {
        let mut state = self.lock();
        if let TapState::Held { input, .. } = &mut *state {
            *input = Some(handle);
        }
        state.answer_cursor();
    }

    /// True once the held output overflows: the shell is not idle, and the caller ends it.
    fn output(&self, bytes: Vec<u8>) -> bool {
        let mut state = self.lock();
        match &mut *state {
            TapState::Held { exited: true, .. } => false,
            TapState::Held { output, exited, .. } => {
                output.extend_from_slice(&bytes);
                if output.len() > SPARE_HOLD {
                    // Treated as gone: the next tab starts its own shell.
                    *exited = true;
                    output.clear();
                    return true;
                }
                state.answer_cursor();
                state.find_ready();
                false
            }
            TapState::Live { output, .. } => {
                output(bytes);
                false
            }
        }
    }

    /// Returns the exit callback when a tab owns the shell; None while it is still spare.
    fn exit(&self) -> Option<OnExit> {
        match &mut *self.lock() {
            TapState::Held { exited, .. } => {
                *exited = true;
                None
            }
            TapState::Live { on_exit, .. } => on_exit.take(),
        }
    }

    /// Hands the held output to the tab and switches to it. Gives `on_exit` back if the shell ended
    /// meanwhile: the tab then starts its own. Under the lock, so no output slips between the two.
    fn attach(&self, output: Output, on_exit: OnExit) -> std::result::Result<(), OnExit> {
        let mut state = self.lock();
        let held = match &mut *state {
            TapState::Held { exited: true, .. } | TapState::Live { .. } => return Err(on_exit),
            TapState::Held { output, .. } => std::mem::take(output),
        };
        if !held.is_empty() {
            output(held);
        }
        *state = TapState::Live { output, on_exit: Some(on_exit) };
        Ok(())
    }
}

/// A local tab takes the spare shell if it runs `shell` and can go to `dir`: the open terminal to register,
/// or `on_exit` back when there is none and the tab starts its own.
pub fn take_spare(terms: &Terms, shell: &str, dir: &str, cols: u16, rows: u16, output: Output, on_exit: OnExit) -> std::result::Result<Open, OnExit> {
    let Some((open, tap)) = terms.take_spare(shell, dir) else { return Err(on_exit) };
    // ConPTY redraws the screen at the new size; that output reaches the tab after the held prompt.
    let _ = open.handle.send(Command::Resize(cols, rows));
    match tap.attach(output, on_exit) {
        Ok(()) => Ok(open),
        Err(on_exit) => {
            close(open);
            Err(on_exit)
        }
    }
}

/// Starts the spare shell. Its output is held until a tab attaches; if it ends first,
/// it leaves the registry by itself.
pub fn open_spare(terms: Arc<Terms>, shell: &str, cwd: &str) -> Result<()> {
    let tap = Arc::new(Tap(Mutex::new(TapState::Held { output: Vec::new(), exited: false, input: None, answered: false, ready: false })));
    let (sink, owner) = (tap.clone(), Arc::downgrade(&terms));
    let output: Output = Arc::new(move |bytes| {
        if sink.output(bytes)
            && let Some(terms) = owner.upgrade()
        {
            terms.drop_spare(&sink);
        }
    });
    let (t, ended) = (terms.clone(), tap.clone());
    let on_exit: OnExit = Box::new(move |reason| match ended.exit() {
        Some(tab) => tab(reason),
        None => t.spare_ended(&ended),
    });
    let wait = waits_for_dir(shell);
    // A waiting shell starts in home: a process keeps the folder it started in open (Set-Location
    // does not move it), and the panel's folder could then be neither deleted nor renamed.
    let dir = if wait { local_dir("") } else { local_dir(cwd) };
    // The tab resizes it on attach; until then the size only shapes the first prompt.
    let open = spawn_local(shell, &dir, wait, 120, 30, Arc::new(Flow::default()), output, on_exit)?;
    tap.set_input(open.handle.clone());
    terms.set_spare(Spare { shell: shell.to_owned(), dir: (!wait).then_some(dir), open, tap });
    Ok(())
}

/// The folder a local shell starts in: the requested one if it exists, else home.
pub fn local_dir(cwd: &str) -> String {
    if !cwd.is_empty() && Path::new(cwd).is_dir() { cwd.to_owned() } else { crate::fs::local::home() }
}

/// Lets the reader and the writer reach their ends: a paused reader, and a writer waiting for the
/// channel window, which never learns that the channel or the connection is gone.
fn release(open: &Open) {
    open.flow.set(false);
    if let Some(stop) = &open.stop {
        stop.notify_one();
    }
}

fn close(open: Open) {
    release(&open);
    if let Some(mut kill) = open.kill {
        let _ = kill.kill();
    }
    // Dropping the queue ends the writer, which closes the channel or the PTY master.
}

/* ---------- Server ---------- */

/// `'…'` with one meaning in sh, bash, zsh, fish and tcsh: a quote inside becomes `'\''`,
/// and `!` becomes `'\!'` (csh expands history even inside quotes and in `-c`).
/// Text with a backslash never gets here: fish unescapes `\\` and `\'` inside quotes.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''").replace('!', r"'\!'"))
}

/// What the channel runs to start in `cwd`, in whatever login shell the user has.
/// None: the login shell in the home folder, also for a folder no quoting suits every shell
/// (a backslash or a control character).
/// `cd` failing (the folder is gone) leaves its message and the shell starts in home, in csh too;
/// no extra command lands in the shell's history because the shell itself starts after `exec`.
/// The path is absolute or under `~/`, so it never starts with `-` and needs no `--`.
pub fn start_command(cwd: &str) -> Option<String> {
    if cwd.chars().any(|c| c == '\\' || c.is_control()) {
        return None;
    }
    let target = match cwd {
        "" | "~" | "~/" => return None,
        _ if cwd.starts_with("~/") => format!("~/{}", quote(&cwd[2..])),
        _ if cwd.starts_with('/') => quote(cwd),
        _ => return None,
    };
    Some(format!("cd {target}; exec \"$SHELL\" -l"))
}

pub async fn open_remote(session: Arc<Session>, cwd: &str, cols: u16, rows: u16, flow: Arc<Flow>, output: Output, on_exit: OnExit) -> Result<Open> {
    let channel = session.handle.channel_open_session().await?;
    channel.request_pty(true, "xterm-256color", cols.into(), rows.into(), 0, 0, &[]).await?;
    match start_command(cwd) {
        Some(cmd) => channel.exec(true, cmd).await?,
        None => channel.request_shell(true).await?,
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let stop = Arc::new(Notify::new());
    tauri::async_runtime::spawn(run_remote(session, channel, rx, stop.clone(), flow.clone(), output, on_exit));
    Ok(Open { handle: tx, flow, kill: None, stop: Some(stop) })
}

async fn run_remote(
    session: Arc<Session>,
    channel: Channel<Msg>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    stop: Arc<Notify>,
    flow: Arc<Flow>,
    output: Output,
    on_exit: OnExit,
) {
    let (mut reader, writer) = channel.split();

    // The queue drops (or `stop` fires) when the tab closes: the channel closes with it.
    tauri::async_runtime::spawn(async move {
        loop {
            let command = tokio::select! {
                c = commands.recv() => c,
                _ = stop.notified() => None,
            };
            let sent = match command {
                Some(Command::Input(bytes)) => tokio::select! {
                    r = writer.data_bytes(Bytes::from(bytes)) => r,
                    _ = stop.notified() => break,
                },
                Some(Command::Resize(cols, rows)) => writer.window_change(cols.into(), rows.into(), 0, 0).await,
                Some(Command::Close) | None => break,
            };
            if sent.is_err() {
                return;
            }
        }
        let _ = writer.eof().await;
        let _ = writer.close().await;
    });

    let mut buffer = Vec::new();
    let mut last_flush = Instant::now() - COALESCE_WINDOW;
    let mut deadline: Option<time::Instant> = None;
    let mut burst = 0usize;
    let mut exited = false;
    let mut killed = false;
    let mut refused = false;
    // Held output goes in coalescing-sized pieces: xterm parses one write in a single pass,
    // and megabytes in one piece would freeze input and drawing for a frame or more.
    let flush = |buffer: &mut Vec<u8>| {
        let data = std::mem::take(buffer);
        if data.len() <= COALESCE_MAX {
            if !data.is_empty() {
                output(data);
            }
        } else {
            data.chunks(COALESCE_MAX).for_each(|piece| output(piece.to_vec()));
        }
    };

    loop {
        let paused = flow.is_paused();
        // Resumed without the select branch seeing it (between two polls):
        // outside a pause a non-empty buffer always has a deadline, so this is held output.
        if !paused && !buffer.is_empty() && deadline.is_none() {
            flush(&mut buffer);
            last_flush = Instant::now();
        }
        if paused && buffer.len() >= REMOTE_HOLD {
            drop_oldest(&mut buffer);
        }
        tokio::select! {
            msg = reader.wait() => match msg {
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                    buffer.extend_from_slice(&data);
                    if paused {
                        deadline = None;
                        continue;
                    }
                    let idle = last_flush.elapsed() >= COALESCE_WINDOW;
                    if idle {
                        burst = 0;
                    }
                    if idle || buffer.len() >= COALESCE_MAX || burst + buffer.len() <= INTERACTIVE_BUDGET {
                        burst += buffer.len();
                        flush(&mut buffer);
                        last_flush = Instant::now();
                        deadline = None;
                    } else if deadline.is_none() {
                        deadline = Some(time::Instant::now() + COALESCE_WINDOW);
                    }
                }
                // OpenSSH sends EOF before the exit status: keep reading until the channel closes.
                Some(ChannelMsg::ExitStatus { .. }) => exited = true,
                Some(ChannelMsg::ExitSignal { .. }) => killed = true,
                // Only the PTY and exec/shell requests want a reply: a refusal of either leaves no usable terminal.
                // The UI forgets the id on exit; the dropped queue then closes the channel.
                Some(ChannelMsg::Failure) => {
                    refused = true;
                    break;
                }
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
            _ = time::sleep_until(deadline.unwrap_or_else(time::Instant::now)), if deadline.is_some() => {
                flush(&mut buffer);
                last_flush = Instant::now();
                deadline = None;
            }
            // Caught up: what was held goes out at once.
            _ = flow.wait(), if paused => {
                flush(&mut buffer);
                last_flush = Instant::now();
                deadline = None;
            }
        }
    }
    flush(&mut buffer);
    let reason = if refused {
        Exit::Refused
    } else if killed {
        Exit::Killed
    } else if exited {
        Exit::Exited
    // A channel that ends without an exit status on a dead connection was cut, not exited.
    } else if session.is_closed() || wait_closed(&session).await {
        Exit::Disconnected
    } else {
        Exit::Exited
    };
    on_exit(reason);
}

/// Keeps the newer half of held output, from a line start: a cut there rarely splits a character or
/// an escape sequence. Half, so the copy happens once per 4 MB, not on every packet.
fn drop_oldest(buffer: &mut Vec<u8>) {
    let cut = buffer.len() - REMOTE_HOLD / 2;
    let from = buffer[cut..].iter().position(|&b| b == b'\n').map_or(cut, |i| cut + i + 1);
    buffer.drain(..from);
}

/// A dropped connection may close its channels a moment before the handle reports it.
async fn wait_closed(session: &Session) -> bool {
    for _ in 0..10 {
        time::sleep(Duration::from_millis(20)).await;
        if session.is_closed() {
            return true;
        }
    }
    false
}

/* ---------- This PC ---------- */

/// After its profile, a spare PowerShell says it is ready (`READY_TITLE`), reads the tab's folder
/// in base64 from its input without echo, puts its title back and goes there, the process too;
/// the first prompt comes after that. A script block keeps its variables out of the session.
/// Under ConstrainedLanguage (AppLocker, WDAC) `ReadKey` and `Convert` are blocked: the shell
/// never says it is ready, so no tab takes it and its tab starts a shell of its own.
const WAIT_DIR: &str = "& { if ($ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') { return }; $t = $Host.UI.RawUI.WindowTitle; $Host.UI.RawUI.WindowTitle = 'kite-spare-ready'; \
$d = ''; while (($k = [Console]::ReadKey($true)).Key -ne 'Enter') { $d += $k.KeyChar }; $Host.UI.RawUI.WindowTitle = $t; \
$p = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($d)); Set-Location -LiteralPath $p; [Environment]::CurrentDirectory = (Get-Location -PSProvider FileSystem).ProviderPath }";

/// Shells that can start as a spare for any folder (`WAIT_DIR`); others start in the folder itself.
fn waits_for_dir(shell: &str) -> bool {
    cfg!(windows) && shell != "cmd"
}

/// Windows shells by the setting; elsewhere the setting is a path (from /etc/shells) or empty for `$SHELL`.
fn shell_candidates(shell: &str, wait_dir: bool) -> Vec<CommandBuilder> {
    let with = |prog: &str, args: &[&str]| {
        let mut c = CommandBuilder::new(prog);
        c.args(args);
        c
    };
    if cfg!(windows) {
        let ps: &[&str] = if wait_dir { &["-NoLogo", "-NoExit", "-Command", WAIT_DIR] } else { &["-NoLogo"] };
        match shell {
            "cmd" => vec![with("cmd.exe", &[])],
            "powershell" => vec![with("powershell.exe", ps)],
            // pwsh is not installed everywhere: Windows PowerShell is always there.
            _ => vec![with("pwsh.exe", ps), with("powershell.exe", ps)],
        }
    } else {
        let env = std::env::var("SHELL").ok().filter(|s| !s.is_empty());
        let mut list: Vec<String> = Vec::new();
        if shell.starts_with('/') {
            list.push(shell.to_owned());
        }
        list.extend(env);
        list.push("/bin/sh".into());
        list.iter().map(|p| with(p, &["-l"])).collect()
    }
}

pub fn open_local(shell: &str, cwd: &str, cols: u16, rows: u16, flow: Arc<Flow>, output: Output, on_exit: OnExit) -> Result<Open> {
    spawn_local(shell, cwd, false, cols, rows, flow, output, on_exit)
}

#[allow(clippy::too_many_arguments)]
fn spawn_local(shell: &str, cwd: &str, wait_dir: bool, cols: u16, rows: u16, flow: Arc<Flow>, output: Output, on_exit: OnExit) -> Result<Open> {
    let pty = native_pty_system()
        .openpty(PtySize { cols, rows, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| Error::Einval { reason: e.to_string() })?;

    let dir = local_dir(cwd);
    let mut child = None;
    let mut last = String::new();
    for mut command in shell_candidates(shell, wait_dir) {
        command.cwd(&dir);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        match pty.slave.spawn_command(command) {
            Ok(c) => {
                child = Some(c);
                break;
            }
            Err(e) => last = e.to_string(),
        }
    }
    let mut child = child.ok_or(Error::Einval { reason: last })?;
    // The child owns the slave now; keeping ours would block EOF on Unix.
    drop(pty.slave);

    let master = pty.master;
    let ends = master.try_clone_reader().and_then(|r| master.take_writer().map(|w| (r, w)));
    let (mut reader, mut writer) = match ends {
        Ok(ends) => ends,
        Err(e) => {
            // Nobody would own the shell otherwise.
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Einval { reason: e.to_string() });
        }
    };
    let killer = child.clone_killer();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (drained_tx, drained) = std::sync::mpsc::channel::<()>();

    let reader_flow = flow.clone();
    std::thread::spawn(move || {
        let mut buffer = vec![0; LOCAL_READ_BUFFER];
        loop {
            reader_flow.wait_blocking();
            match reader.read(&mut buffer) {
                Ok(n) if n > 0 => output(buffer[..n].to_vec()),
                _ => break,
            }
        }
        let _ = drained_tx.send(());
    });

    // Owns the master: when the queue drops (the tab closed) or the shell ended, the PTY closes.
    std::thread::spawn(move || {
        while let Some(command) = rx.blocking_recv() {
            match command {
                Command::Input(bytes) => {
                    if writer.write_all(&bytes).and_then(|_| writer.flush()).is_err() {
                        break;
                    }
                }
                Command::Resize(cols, rows) => {
                    let _ = master.resize(PtySize { cols, rows, pixel_width: 0, pixel_height: 0 });
                }
                Command::Close => break,
            }
        }
    });

    // The exit is reported after the output the shell left: ConPTY hands the rest over
    // only once the pseudoconsole closes, which the writer does when it lets the master go.
    let closer = tx.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        let _ = closer.send(Command::Close);
        drop(closer);
        let _ = drained.recv_timeout(LOCAL_DRAIN);
        on_exit(Exit::Exited);
    });

    Ok(Open { handle: tx, flow, kill: Some(killer), stop: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn spare_shell_goes_to_the_tab_with_its_prompt_and_exit() {
        let terms = Arc::new(Terms::default());
        let dir = local_dir(&std::env::temp_dir().to_string_lossy());
        let exit = |tx: std::sync::mpsc::Sender<Exit>| -> OnExit {
            Box::new(move |r| {
                let _ = tx.send(r);
            })
        };

        // A spare for another folder is ended, not handed out.
        open_spare(terms.clone(), "cmd", &dir).unwrap();
        let (etx, _erx) = std::sync::mpsc::channel();
        assert!(take_spare(&terms, "cmd", &local_dir(""), 80, 24, Arc::new(|_| {}), exit(etx)).is_err());
        assert!(terms.spare_slot().is_none());

        open_spare(terms.clone(), "cmd", &dir).unwrap();
        // Let the prompt arrive while nobody listens: it must be held, not lost.
        std::thread::sleep(Duration::from_millis(1500));
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (etx, erx) = std::sync::mpsc::channel();
        let output: Output = Arc::new(move |b| {
            let _ = tx.send(b);
        });
        let open = take_spare(&terms, "cmd", &dir, 100, 30, output, exit(etx)).ok().expect("the spare is taken");
        // The first chunk is what was held: the prompt is in it, the cursor question is not.
        let held = String::from_utf8_lossy(&rx.recv_timeout(Duration::from_secs(5)).expect("the held output")).into_owned();
        assert!(held.contains('>'), "no prompt in {held:?}");
        assert!(!held.contains("\x1b[6n"));
        assert!(terms.spare_slot().is_none());
        let _ = open.handle.send(Command::Input(b"exit\r".to_vec()));
        assert_eq!(erx.recv_timeout(Duration::from_secs(5)).unwrap(), Exit::Exited);
    }

    #[cfg(windows)]
    #[test]
    fn spare_powershell_goes_to_the_folder_of_the_tab() {
        let terms = Arc::new(Terms::default());
        let tmp = std::env::temp_dir();
        let start = tmp.join(format!("kite-spare-start-{}", std::process::id()));
        // A space and characters outside the console code page.
        let name = format!("kite spare №1 — тест {}", std::process::id());
        let target = tmp.join(&name);
        std::fs::create_dir_all(&start).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let home = target.to_string_lossy().into_owned();
        open_spare(terms.clone(), "powershell", &start.to_string_lossy()).unwrap();
        // Handed out only once it waits for the folder; a profile's own questions come before that.
        let until = std::time::Instant::now() + Duration::from_secs(20);
        while !terms.spare_slot().as_ref().is_some_and(|s| s.tap.ready()) {
            assert!(std::time::Instant::now() < until, "the spare never got ready");
            std::thread::sleep(Duration::from_millis(50));
        }
        // The waiting shell does not hold the panel's folder.
        std::fs::remove_dir(&start).expect("the panel's folder is free");
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (etx, erx) = std::sync::mpsc::channel();
        let output: Output = Arc::new(move |b| {
            let _ = tx.send(b);
        });
        let on_exit: OnExit = Box::new(move |r| {
            let _ = etx.send(r);
        });
        let open = take_spare(&terms, "powershell", &home, 100, 30, output, on_exit).ok().expect("the spare is taken");
        // The first prompt is in the tab's folder, and the folder was not echoed as typed text.
        // By name only: the temp folder may be given in its 8.3 form (`RUNNER~1`), the prompt
        // shows the long one.
        let mut seen = String::new();
        let until = std::time::Instant::now() + Duration::from_secs(20);
        while !seen.contains(&format!("\\{name}> ")) {
            let left = until.saturating_duration_since(std::time::Instant::now());
            let chunk = rx.recv_timeout(left).unwrap_or_else(|_| panic!("no prompt in {home}: {seen:?}"));
            // Taken this early, the cursor question reaches the tab: answer it as xterm does.
            if chunk.windows(CURSOR_QUERY.len()).any(|w| w == CURSOR_QUERY) {
                let _ = open.handle.send(Command::Input(CURSOR_HOME.to_vec()));
            }
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
        assert_eq!(seen.matches(name.as_str()).count(), 1, "{seen:?}");
        assert!(!seen.contains(READY_TITLE), "{seen:?}");
        let _ = open.handle.send(Command::Input(b"exit\r".to_vec()));
        assert_eq!(erx.recv_timeout(Duration::from_secs(10)).unwrap(), Exit::Exited);
        let _ = std::fs::remove_dir(&target);
    }

    #[test]
    fn ready_title_is_cut_out_of_the_held_output() {
        let mut state = TapState::Held { output: Vec::new(), exited: false, input: None, answered: false, ready: false };
        let feed = |s: &mut TapState, b: &[u8]| {
            if let TapState::Held { output, .. } = s {
                output.extend_from_slice(b);
            }
            s.find_ready();
        };
        feed(&mut state, b"banner\r\n\x1b]0;kite-spare-");
        assert!(matches!(state, TapState::Held { ready: false, .. }));
        feed(&mut state, b"ready");
        // The end of the sequence has not come yet.
        assert!(matches!(state, TapState::Held { ready: false, .. }));
        feed(&mut state, b"\x07\x1b[?25h");
        let TapState::Held { output, ready, .. } = &state else { unreachable!() };
        assert!(*ready);
        assert_eq!(output.as_slice(), b"banner\r\n\x1b[?25h");
    }

    #[test]
    fn held_output_keeps_the_newest_lines() {
        let line = b"0123456789abcdef\n";
        let mut buf: Vec<u8> = line.iter().copied().cycle().take(REMOTE_HOLD + 5).collect();
        let last = *buf.last().unwrap();
        drop_oldest(&mut buf);
        assert!(buf.len() <= REMOTE_HOLD / 2 && buf.len() > REMOTE_HOLD / 2 - line.len());
        assert_eq!(buf[0], b'0');
        assert_eq!(*buf.last().unwrap(), last);
    }

    #[test]
    fn start_command_quotes() {
        assert_eq!(start_command(""), None);
        assert_eq!(start_command("~"), None);
        assert_eq!(start_command("www"), None);
        assert_eq!(start_command("/var/www"), Some("cd '/var/www'; exec \"$SHELL\" -l".into()));
        assert_eq!(start_command("~/my site "), Some("cd ~/'my site '; exec \"$SHELL\" -l".into()));
        assert_eq!(start_command("/tmp/it's; rm -rf /"), Some("cd '/tmp/it'\\''s; rm -rf /'; exec \"$SHELL\" -l".into()));
        assert_eq!(start_command("/srv/a!b"), Some("cd '/srv/a'\\!'b'; exec \"$SHELL\" -l".into()));
        // fish would unescape these inside quotes: such folders start in home instead.
        assert_eq!(start_command(r"/tmp/x\'; rm -rf ~; echo \'"), None);
        assert_eq!(start_command("/tmp/a\nb"), None);
    }

    #[test]
    fn flow_resumes_a_waiting_reader() {
        let flow = Arc::new(Flow::default());
        flow.set(true);
        let waiting = flow.clone();
        let reader = std::thread::spawn(move || waiting.wait_blocking());
        std::thread::sleep(Duration::from_millis(20));
        assert!(!reader.is_finished());
        flow.set(false);
        reader.join().unwrap();
    }
}
