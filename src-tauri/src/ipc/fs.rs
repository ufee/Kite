//! Files of a pane. A source is `"local"` or `{ server, user }`; every operation is a log row.

use std::{future::Future, sync::Arc};

use serde::Deserialize;
use tauri::State;

use crate::{
    error::{Error, Result},
    fs::{Entry, TextFile, local, remote},
    log::{Log, Msg, State as Row},
    msg,
    ssh::{Session, Ssh},
};

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Local {
    Local,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum Src {
    Server { server: String, user: String },
    Local(Local),
}

/// Log source of local rows; the UI shows it as "This PC".
const LOCAL: &str = "local";

enum Place {
    Local,
    Server(Arc<Session>),
}

fn place(ssh: &Ssh, src: Src) -> Result<Place> {
    match src {
        Src::Local(_) => Ok(Place::Local),
        Src::Server { server, user } => ssh.session_live(&server, &user).map(Place::Server),
    }
}

impl Place {
    fn src(&self) -> &str {
        match self {
            Self::Local => LOCAL,
            Self::Server(s) => &s.code,
        }
    }

    fn tag(&self) -> &'static str {
        match self {
            Self::Local => "FS",
            Self::Server(_) => "SFTP",
        }
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| Error::Einval { reason: e.to_string() })?
}

/// One log row for the operation: running, then its outcome.
async fn logged<T>(log: &Log, p: &Place, text: String, work: impl Future<Output = Result<T>>, note: impl FnOnce(&T) -> Msg) -> Result<T> {
    let id = log.run(p.src(), p.tag(), text);
    let res = work.await;
    match &res {
        Ok(v) => log.done(id, note(v), Row::Ok),
        Err(e) => log.done(id, e.to_string(), Row::Err),
    }
    // The channel is gone but the connection may look alive: drop it, the reconnect follows.
    if let (Err(Error::Disconnected), Place::Server(s)) = (&res, p) {
        s.sftp_lost().await;
    }
    res
}

fn objs(n: usize) -> Msg {
    msg!("объект|объекта|объектов", n = n)
}

fn done<T>(_: &T) -> Msg {
    "готово".into()
}

type SshState<'a> = State<'a, Arc<Ssh>>;
type LogState<'a> = State<'a, Arc<Log>>;

#[tauri::command]
pub async fn fs_home(ssh: SshState<'_>, src: Src) -> Result<String> {
    match place(&ssh, src)? {
        Place::Local => Ok(local::home()),
        Place::Server(s) => Ok(s.home.clone()),
    }
}

#[tauri::command]
pub async fn fs_resolve(ssh: SshState<'_>, src: Src, raw: String, base: Option<String>) -> Result<String> {
    match place(&ssh, src)? {
        Place::Local => blocking(move || local::resolve(&raw, base.as_deref())).await,
        Place::Server(s) => remote::resolve(&s, &raw, base.as_deref()).await,
    }
}

#[tauri::command]
pub async fn fs_list(ssh: SshState<'_>, log: LogState<'_>, src: Src, path: String) -> Result<Vec<Entry>> {
    let p = place(&ssh, src)?;
    let text = format!("{} {path}", if matches!(p, Place::Local) { "LIST" } else { "READDIR" });
    let work = async {
        match &p {
            Place::Local => {
                let path = path.clone();
                blocking(move || local::list(&path)).await
            }
            Place::Server(s) => remote::list(s, &path).await,
        }
    };
    logged(&log, &p, text, work, |l| objs(l.len())).await
}

#[tauri::command]
pub async fn fs_mkdir(ssh: SshState<'_>, log: LogState<'_>, src: Src, path: String) -> Result<()> {
    let p = place(&ssh, src)?;
    let work = async {
        match &p {
            Place::Local => {
                let path = path.clone();
                blocking(move || local::mkdir(&path)).await
            }
            Place::Server(s) => remote::mkdir(s, &path).await,
        }
    };
    logged(&log, &p, format!("MKDIR {path}"), work, done).await
}

#[tauri::command]
pub async fn fs_rename(ssh: SshState<'_>, log: LogState<'_>, src: Src, from: String, to: String) -> Result<()> {
    let p = place(&ssh, src)?;
    let work = async {
        match &p {
            Place::Local => {
                let (from, to) = (from.clone(), to.clone());
                blocking(move || local::rename(&from, &to)).await
            }
            Place::Server(s) => remote::rename(s, &from, &to).await,
        }
    };
    logged(&log, &p, format!("RENAME {from} → {to}"), work, done).await
}

/// Server only: local files go to the trash. Stops at the first failure.
#[tauri::command]
pub async fn fs_delete(ssh: SshState<'_>, log: LogState<'_>, src: Src, paths: Vec<String>) -> Result<()> {
    let p = place(&ssh, src)?;
    let Place::Server(s) = &p else { return Err(Error::Einval { reason: "local files go to the trash".into() }) };
    for path in &paths {
        logged(&log, &p, format!("DELETE {path}"), remote::delete(s, path), |n| objs(*n)).await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn fs_trash(log: LogState<'_>, paths: Vec<String>) -> Result<()> {
    let text = match paths.as_slice() {
        [one] => format!("TRASH {one}"),
        _ => format!("TRASH {} …", paths.first().cloned().unwrap_or_default()),
    };
    let n = paths.len();
    logged(&log, &Place::Local, text, blocking(move || local::trash(&paths)), |_| objs(n)).await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn fs_chmod(
    ssh: SshState<'_>,
    log: LogState<'_>,
    src: Src,
    paths: Vec<String>,
    mode: u32,
    owner: String,
    group: String,
    recursive: bool,
) -> Result<()> {
    let p = place(&ssh, src)?;
    let Place::Server(s) = &p else { return Err(Error::Einval { reason: "no permissions on this PC".into() }) };
    let mut text = format!("SETSTAT {:03o}", mode & 0o777);
    if !owner.is_empty() || !group.is_empty() {
        text.push_str(&format!(" {owner}:{group}"));
    }
    if recursive {
        text.push_str(" -R");
    }
    text.push(' ');
    text.push_str(&paths.join(" "));
    logged(&log, &p, text, remote::chmod(s, &paths, mode, &owner, &group, recursive), |n| objs(*n)).await.map(|_| ())
}

#[tauri::command]
pub async fn fs_read_text(ssh: SshState<'_>, log: LogState<'_>, src: Src, path: String) -> Result<TextFile> {
    let p = place(&ssh, src)?;
    let work = async {
        match &p {
            Place::Local => {
                let path = path.clone();
                blocking(move || local::read_text(&path)).await
            }
            Place::Server(s) => remote::read_text(s, &path).await,
        }
    };
    let note = |t: &TextFile| -> Msg {
        if t.too_big {
            "больше 10 МБ".into()
        } else if t.binary {
            "двоичный".into()
        } else {
            msg!("{n} байт", n = t.text.len())
        }
    };
    logged(&log, &p, format!("READ {path}"), work, note).await
}

#[tauri::command]
pub async fn fs_drives() -> Result<Vec<String>> {
    blocking(|| Ok(local::drives())).await
}
