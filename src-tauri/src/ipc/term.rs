//! Terminals. Output travels as `InvokeResponseBody::Raw`: an `ArrayBuffer` in JS, no JSON or base64 step.
//! The exit goes as `{exit}` on the same channel, which keeps its order: it never overtakes the last output.

use std::sync::Arc;

use serde::Serialize;
use tauri::{
    State,
    ipc::{Channel, InvokeResponseBody},
};

use super::fs::Src;
use crate::{
    error::Result,
    log::Log,
    msg,
    ssh::Ssh,
    term::{self, Command, Exit, Terms},
};

type TermsState<'a> = State<'a, Arc<Terms>>;

#[derive(Serialize)]
struct ExitMsg {
    exit: Exit,
}

fn sink(channel: Channel<InvokeResponseBody>) -> term::Output {
    Arc::new(move |bytes| {
        let _ = channel.send(InvokeResponseBody::Raw(bytes));
    })
}

/// Tells the UI, unless the tab was closed first: then nobody is waiting.
fn on_exit(channel: Channel<InvokeResponseBody>, terms: Arc<Terms>, id: u32) -> term::OnExit {
    Box::new(move |reason| {
        if terms.ended(id)
            && let Ok(json) = serde_json::to_string(&ExitMsg { exit: reason })
        {
            let _ = channel.send(InvokeResponseBody::Json(json));
        }
    })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn pty_open(
    ssh: State<'_, Arc<Ssh>>,
    terms: TermsState<'_>,
    log: State<'_, Arc<Log>>,
    src: Src,
    cwd: String,
    shell: String,
    cols: u16,
    rows: u16,
    on_data: Channel<InvokeResponseBody>,
) -> Result<u32> {
    let terms = terms.inner().clone();
    let id = terms.allocate();
    let exit = on_exit(on_data.clone(), terms.clone(), id);
    let res = match src {
        Src::Local(_) => {
            let row = log.run("local", "PTY", msg!("Запуск терминала · {u}", u = shell));
            let (shell, t) = (shell.clone(), terms.clone());
            let opened = tauri::async_runtime::spawn_blocking(move || {
                term::open_local(&shell, &cwd, cols, rows, sink(on_data), exit).map(|(h, kill)| t.register_local(id, h, kill))
            })
            .await
            .map_err(crate::error::Error::ssh)
            .and_then(|r| r);
            log.done(row, opened.as_ref().map_or_else(|e| e.to_string(), |_| String::new()), state(&opened));
            opened
        }
        Src::Server { server, user } => {
            let session = ssh.session_live(&server, &user);
            match session {
                Err(e) => Err(e),
                Ok(session) => {
                    let row = log.run(&session.code, "PTY", msg!("Запуск терминала · {u}", u = user));
                    let opened = term::open_remote(session, &cwd, cols, rows, sink(on_data), exit).await.map(|h| terms.register_remote(id, h));
                    log.done(row, opened.as_ref().map_or_else(|e| e.to_string(), |_| String::new()), state(&opened));
                    opened
                }
            }
        }
    };
    match res {
        Ok(_) => Ok(id),
        Err(e) => {
            terms.forget(id);
            Err(e)
        }
    }
}

fn state<T>(r: &Result<T>) -> crate::log::State {
    if r.is_ok() { crate::log::State::Ok } else { crate::log::State::Err }
}

// Synchronous on purpose: keystrokes must reach the queue in the order they were typed,
// which independent async commands do not promise.
#[tauri::command]
pub fn pty_write(terms: TermsState<'_>, id: u32, data: String) {
    terms.send(id, Command::Input(data.into_bytes()));
}

#[tauri::command]
pub fn pty_resize(terms: TermsState<'_>, id: u32, cols: u16, rows: u16) {
    terms.send(id, Command::Resize(cols, rows));
}

#[tauri::command]
pub fn pty_close(terms: TermsState<'_>, id: u32) {
    terms.close(id);
}
