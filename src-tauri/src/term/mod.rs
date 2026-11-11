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
/// Output a paused remote terminal keeps in the core. Only past this does its reader stop and the
/// connection stall: a flood (`yes`) while the window is hidden and xterm parses nothing.
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
/// holds the shell back. A remote reader keeps reading into its own buffer up to `REMOTE_HOLD`:
/// russh hands channel data over from the session loop, so a reader that stops stalls the whole
/// connection (file panels, other tabs, keepalive), not just this channel.
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
        // Resumed without the select branch seeing it (after the hold limit, or between two polls):
        // outside a pause a non-empty buffer always has a deadline, so this is held output.
        if !paused && !buffer.is_empty() && deadline.is_none() {
            flush(&mut buffer);
            last_flush = Instant::now();
        }
        if paused && buffer.len() >= REMOTE_HOLD {
            flow.wait().await;
            continue;
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

/// Windows shells by the setting; elsewhere the setting is a path (from /etc/shells) or empty for `$SHELL`.
fn shell_candidates(shell: &str) -> Vec<CommandBuilder> {
    let with = |prog: &str, args: &[&str]| {
        let mut c = CommandBuilder::new(prog);
        c.args(args);
        c
    };
    if cfg!(windows) {
        match shell {
            "cmd" => vec![with("cmd.exe", &[])],
            "powershell" => vec![with("powershell.exe", &["-NoLogo"])],
            // pwsh is not installed everywhere: Windows PowerShell is always there.
            _ => vec![with("pwsh.exe", &["-NoLogo"]), with("powershell.exe", &["-NoLogo"])],
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
    let pty = native_pty_system()
        .openpty(PtySize { cols, rows, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| Error::Einval { reason: e.to_string() })?;

    let dir = if !cwd.is_empty() && Path::new(cwd).is_dir() { cwd.to_owned() } else { crate::fs::local::home() };
    let mut child = None;
    let mut last = String::new();
    for mut command in shell_candidates(shell) {
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
