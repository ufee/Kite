//! Editing in an external program: open, take a save up, take the server's version.

use std::sync::Arc;

use tauri::State;

use super::fs::Src;
use crate::{
    edit::{Edits, Unsent, editor},
    error::{Error, Result},
};

type EditState<'a> = State<'a, Arc<Edits>>;

#[tauri::command]
pub async fn edit_open(edits: EditState<'_>, src: Src, path: String, editor: String, autosave: bool) -> Result<()> {
    match src {
        Src::Local(_) => edits.open_local(path, editor).await,
        Src::Server { server, user } => edits.open(&server, &user, &path, &editor, autosave).await,
    }
}

/// `force`: the user chose his version over a change on the server.
#[tauri::command]
pub async fn edit_upload(edits: EditState<'_>, src: Src, path: String, force: bool) -> Result<()> {
    match src {
        Src::Local(_) => Err(Error::Einval { reason: "a local file is edited in place".into() }),
        Src::Server { server, user } => edits.upload(&server, &user, &path, force).await,
    }
}

#[tauri::command]
pub async fn edit_reload(edits: EditState<'_>, src: Src, path: String) -> Result<()> {
    match src {
        Src::Local(_) => Ok(()),
        Src::Server { server, user } => edits.reload(&server, &user, &path).await,
    }
}

/// The "upload at once" setting changed: it holds for the files already open too.
#[tauri::command]
pub fn edit_autosave(edits: EditState<'_>, on: bool) {
    edits.set_autosave(on);
}

/// Saves made in the editor that closing the app would lose.
#[tauri::command]
pub fn edit_unsent(edits: EditState<'_>) -> Vec<Unsent> {
    edits.unsent()
}

/// The editors of the settings list that are installed here.
#[tauri::command]
pub fn edit_editors() -> Vec<&'static str> {
    editor::available()
}
