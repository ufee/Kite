//! Terminals: a local shell on a PTY (ConPTY on Windows) and a shell on an SSH channel.
//! Each has one ordered input queue; output goes to a sink as raw bytes.

use std::{
    collections::HashMap,
    io::{Read, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use russh::{Channel, ChannelMsg, client::Msg};
use serde::Serialize;
use tokio::{sync::mpsc, time};

use crate::{
    error::{Error, Result},
    ssh::Session,
};

/// Sink for terminal output. Takes ownership so the buffer reaches the IPC layer without a copy.
pub type Output = Arc<dyn Fn(Vec<u8>) + Send + Sync>;
pub type OnExit = Box<dyn FnOnce(Exit) + Send>;

/// Why a terminal ended: the shell exited (the tab closes) or the connection went away (Enter reconnects).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Exit {
    Exit,
    Disconnected,
}

/// Each IPC message costs a round trip through the webview, so a flood is coalesced.
/// The first bytes after a pause still go out at once: keystroke echo never waits for a timer.
const COALESCE_WINDOW: Duration = Duration::from_millis(8);
const COALESCE_MAX: usize = 256 * 1024;
/// Output up to this many bytes per burst is never delayed, even when keys come inside
/// `COALESCE_WINDOW` (Windows rounds tokio timers up to its ~15.6 ms tick).
const INTERACTIVE_BUDGET: usize = 4 * 1024;
const LOCAL_READ_BUFFER: usize = 64 * 1024;

pub enum Command {
    Input(Vec<u8>),
    Resize(u16, u16),
}

/// Input queue of a terminal. Commands stay ordered, unlike independent async IPC calls.
pub type Handle = mpsc::UnboundedSender<Command>;

/* ---------- Registry ---------- */

struct Open {
    handle: Handle,
    /// Ends the local shell process; remote shells end when the queue drops.
    kill: Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
}

/// Terminals by id. An id is taken before the shell starts, so the exit callback knows it;
/// a shell that exits before it is registered leaves no entry behind.
#[derive(Default)]
pub struct Terms {
    next: AtomicU32,
    open: Mutex<HashMap<u32, Option<Open>>>,
}

impl Terms {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<u32, Option<Open>>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn allocate(&self) -> u32 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.map().insert(id, None);
        id
    }

    /// False when the shell already ended or the tab was closed meanwhile: then it is closed here.
    fn set(&self, id: u32, open: Open) -> bool {
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
        self.map().remove(&id).is_some()
    }

    pub fn send(&self, id: u32, command: Command) {
        if let Some(Some(open)) = self.map().get(&id) {
            let _ = open.handle.send(command);
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

fn close(open: Open) {
    if let Some(mut kill) = open.kill {
        let _ = kill.kill();
    }
    // Dropping the queue ends the writer, which closes the channel or the PTY master.
}

/* ---------- Server ---------- */

/// `'…'` for a POSIX shell (also fish and tcsh): a quote inside becomes `'\''`.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// What the channel runs to start in `cwd`. None: the login shell in the home folder.
/// `cd` failing (the folder is gone) leaves its message and falls back to home; no extra
/// command lands in the shell's history because the shell itself starts after `exec`.
pub fn start_command(cwd: &str) -> Option<String> {
    let cwd = cwd.trim();
    let target = match cwd {
        "" | "~" | "~/" => return None,
        _ if cwd.starts_with("~/") => format!("~/{}", quote(&cwd[2..])),
        _ => quote(cwd),
    };
    Some(format!("cd -- {target} || cd; exec \"$SHELL\" -l"))
}

pub async fn open_remote(session: Arc<Session>, cwd: &str, cols: u16, rows: u16, output: Output, on_exit: OnExit) -> Result<Handle> {
    let channel = session.handle.channel_open_session().await?;
    channel.request_pty(true, "xterm-256color", cols.into(), rows.into(), 0, 0, &[]).await?;
    match start_command(cwd) {
        Some(cmd) => channel.exec(true, cmd).await?,
        None => channel.request_shell(true).await?,
    }
    let (tx, rx) = mpsc::unbounded_channel();
    tauri::async_runtime::spawn(run_remote(session, channel, rx, output, on_exit));
    Ok(tx)
}

async fn run_remote(session: Arc<Session>, channel: Channel<Msg>, mut commands: mpsc::UnboundedReceiver<Command>, output: Output, on_exit: OnExit) {
    let (mut reader, writer) = channel.split();

    // The queue drops when the tab closes: the channel closes with it.
    tauri::async_runtime::spawn(async move {
        while let Some(command) = commands.recv().await {
            let sent = match command {
                Command::Input(bytes) => writer.data_bytes(Bytes::from(bytes)).await,
                Command::Resize(cols, rows) => writer.window_change(cols.into(), rows.into(), 0, 0).await,
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
    let flush = |buffer: &mut Vec<u8>| {
        if !buffer.is_empty() {
            output(std::mem::take(buffer));
        }
    };

    loop {
        tokio::select! {
            msg = reader.wait() => match msg {
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                    buffer.extend_from_slice(&data);
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
                Some(ChannelMsg::ExitStatus { .. } | ChannelMsg::ExitSignal { .. }) => exited = true,
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
            _ = time::sleep_until(deadline.unwrap_or_else(time::Instant::now)), if deadline.is_some() => {
                flush(&mut buffer);
                last_flush = Instant::now();
                deadline = None;
            }
        }
    }
    flush(&mut buffer);
    // A channel that ends without an exit status on a dead connection was cut, not exited.
    let gone = !exited && (session.is_closed() || wait_closed(&session).await);
    on_exit(if gone { Exit::Disconnected } else { Exit::Exit });
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

pub fn open_local(shell: &str, cwd: &str, cols: u16, rows: u16, output: Output, on_exit: OnExit) -> Result<(Handle, Box<dyn portable_pty::ChildKiller + Send + Sync>)> {
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
    let mut reader = master.try_clone_reader().map_err(|e| Error::Einval { reason: e.to_string() })?;
    let mut writer = master.take_writer().map_err(|e| Error::Einval { reason: e.to_string() })?;
    let killer = child.clone_killer();
    let (tx, mut rx) = mpsc::unbounded_channel();

    std::thread::spawn(move || {
        let mut buffer = vec![0; LOCAL_READ_BUFFER];
        while let Ok(n) = reader.read(&mut buffer) {
            if n == 0 {
                break;
            }
            output(buffer[..n].to_vec());
        }
    });

    // Owns the master: when the queue drops (the tab closed or the shell ended), the PTY closes.
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
            }
        }
    });

    std::thread::spawn(move || {
        let _ = child.wait();
        on_exit(Exit::Exit);
    });

    Ok((tx, killer))
}

/* ---------- Wiring for the IPC layer ---------- */

impl Terms {
    pub fn register_local(&self, id: u32, handle: Handle, kill: Box<dyn portable_pty::ChildKiller + Send + Sync>) -> bool {
        self.set(id, Open { handle, kill: Some(kill) })
    }

    pub fn register_remote(&self, id: u32, handle: Handle) -> bool {
        self.set(id, Open { handle, kill: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_command_quotes() {
        assert_eq!(start_command(""), None);
        assert_eq!(start_command("~"), None);
        assert_eq!(start_command("/var/www"), Some("cd -- '/var/www' || cd; exec \"$SHELL\" -l".into()));
        assert_eq!(start_command("~/my site"), Some("cd -- ~/'my site' || cd; exec \"$SHELL\" -l".into()));
        assert_eq!(start_command("/tmp/it's; rm -rf /"), Some("cd -- '/tmp/it'\\''s; rm -rf /' || cd; exec \"$SHELL\" -l".into()));
    }
}
