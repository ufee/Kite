//! The debug log shown in the log window: rows go to the UI as events and stay in a ring for a reload.
//!
//! Redaction is structural: secrets never reach `add`. Passwords and passphrases live in `Zeroizing`
//! buffers that are passed to the SSH layer only, and file contents are logged as sizes.

use std::{
    borrow::Cow,
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

const LIMIT: usize = 1000;

/// Text of a row: a UI message id (the Russian string, as everywhere in the UI) with its values,
/// or a plain technical string (algorithms, versions, server replies) that is shown as is.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Msg {
    pub text: Cow<'static, str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub vars: Vec<(&'static str, String)>,
}

impl From<&'static str> for Msg {
    fn from(s: &'static str) -> Self {
        Self { text: Cow::Borrowed(s), vars: Vec::new() }
    }
}

impl From<String> for Msg {
    fn from(s: String) -> Self {
        Self { text: Cow::Owned(s), vars: Vec::new() }
    }
}

/// `msg!("Подключение к {addr}", addr = x)`
#[macro_export]
macro_rules! msg {
    ($text:literal $(, $k:ident = $v:expr)* $(,)?) => {
        $crate::log::Msg { text: ::std::borrow::Cow::Borrowed($text), vars: vec![$((stringify!($k), ($v).to_string())),*] }
    };
}

/// Rows the UI mock still writes count from 1; the core's ids start far above them.
const FIRST_ID: u64 = 1 << 40;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Run,
    Ok,
    Err,
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub id: u64,
    /// Unix time in milliseconds; the UI formats it in local time.
    pub ts: u64,
    pub src: String,
    pub tag: &'static str,
    pub text: Msg,
    pub note: Msg,
    pub state: State,
}

pub struct Log {
    app: Option<AppHandle>,
    rows: Mutex<VecDeque<Row>>,
    next: AtomicU64,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Log {
    pub fn new(app: Option<AppHandle>) -> Self {
        Self { app, rows: Mutex::new(VecDeque::with_capacity(LIMIT)), next: AtomicU64::new(FIRST_ID) }
    }

    fn emit(&self, event: &str, row: &Row) {
        if let Some(app) = &self.app {
            let _ = app.emit(event, row);
        }
    }

    pub fn add(&self, src: &str, tag: &'static str, text: impl Into<Msg>, note: impl Into<Msg>, state: State) -> u64 {
        let row = Row {
            id: self.next.fetch_add(1, Ordering::Relaxed),
            ts: now_ms(),
            src: src.to_owned(),
            tag,
            text: text.into(),
            note: note.into(),
            state,
        };
        {
            let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
            if rows.len() == LIMIT {
                rows.pop_front();
            }
            rows.push_back(row.clone());
        }
        self.emit("log://row", &row);
        row.id
    }

    pub fn ok(&self, src: &str, tag: &'static str, text: impl Into<Msg>) -> u64 {
        self.add(src, tag, text, "", State::Ok)
    }

    pub fn run(&self, src: &str, tag: &'static str, text: impl Into<Msg>) -> u64 {
        self.add(src, tag, text, "", State::Run)
    }

    pub fn err(&self, src: &str, tag: &'static str, text: impl Into<Msg>, note: impl Into<Msg>) -> u64 {
        self.add(src, tag, text, note, State::Err)
    }

    /// Closes a `run` row with its outcome.
    pub fn done(&self, id: u64, note: impl Into<Msg>, state: State) {
        let row = {
            let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
            let Some(row) = rows.iter_mut().rev().find(|r| r.id == id) else { return };
            row.note = note.into();
            row.state = state;
            row.clone()
        };
        self.emit("log://update", &row);
    }

    pub fn tail(&self) -> Vec<Row> {
        self.rows.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
    }
}
