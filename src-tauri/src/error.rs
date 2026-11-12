//! Errors that cross into the UI. Only the code and the facts travel; the UI words them in its language.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "code", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Error {
    /// No TCP answer within the connect timeout.
    Timeout { addr: String },
    /// The network said no: refused, unreachable, unknown host name, reset.
    Net { addr: String, reason: String },
    /// The server closed the connection before the login: a ban after failed logins
    /// (PerSourcePenalties, fail2ban), MaxStartups, a wrong port that speaks something else.
    Dropped { addr: String },
    /// The user declined the host key, or it is revoked.
    HostkeyRejected,
    /// "Test" in the card met another key than the one on record; the dialog comes on connect.
    HostkeyChanged { addr: String, old: String, new: String },
    /// No key was accepted.
    AuthKey { user: String, server: String },
    /// The password was not accepted.
    AuthPass,
    /// A password-only server without a password in the OS keychain.
    NoSecret,
    /// The server asks for more than a password (several prompts, a second factor).
    AuthMulti,
    /// The server offers no method Kite can use.
    AuthMethods { methods: String },
    /// A key file is missing or unreadable.
    KeyFile { path: String, reason: String },
    /// No public key to put on the server: no `.pub` next to the key and no ed25519 in the agent.
    NoPubkey,
    /// The key landed in authorized_keys, but logging in with it failed.
    KeyCheck { reason: String },
    /// The passphrase does not open the key; the dialog stays.
    Passphrase,
    Cancelled,
    /// The session is gone (closed, dropped, not connected yet).
    Disconnected,
    /// Anything else the SSH layer reports.
    Ssh { reason: String },
    Keychain { reason: String },
    #[serde(rename = "ENOENT")]
    Enoent { path: String },
    #[serde(rename = "ENOTDIR")]
    Enotdir { path: String },
    #[serde(rename = "EACCES")]
    Eacces { path: String },
    #[serde(rename = "EEXIST")]
    Eexist { path: String },
    /// The server runs no commands (`ForceCommand internal-sftp`): no chown there.
    NoExec,
    /// A typed path that is not a path of this source (`folder` on Windows without a drive).
    BadPath,
    #[serde(rename = "EINVAL")]
    Einval { reason: String },
    /// The server of a transfer is no longer in the list.
    ServerGone,
    /// A server name this PC cannot hold (`\`, `:`, `CON`, a trailing dot): skipped, never joined.
    BadName { path: String },
    /// Two server names that differ only in case land on one file here: the second is skipped.
    NameClash { path: String },
    /// The source changed while it was being sent; a move keeps it.
    SourceChanged { path: String },
    /// An edit going back met another version of the server file than the copy was checked against.
    ServerChanged { path: String },
    /// An edit waits for another upload to the same file, and that one is paused.
    EditWaits { path: String },
    /// A FIFO, socket or device: never transferred.
    NotRegular { path: String },
    /// A transfer was paused or cancelled; never reaches the UI.
    Stopped,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn ssh(e: impl std::fmt::Display) -> Self {
        Self::Ssh { reason: e.to_string() }
    }
}

impl From<russh::Error> for Error {
    fn from(e: russh::Error) -> Self {
        match e {
            russh::Error::Disconnect
            | russh::Error::SendError
            | russh::Error::HUP
            | russh::Error::KeepaliveTimeout
            | russh::Error::InactivityTimeout => Self::Disconnected,
            // The server reset or closed the socket rather than saying goodbye.
            russh::Error::IO(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::BrokenPipe
                ) =>
            {
                Self::Disconnected
            }
            e => Self::ssh(e),
        }
    }
}

impl From<keyring::Error> for Error {
    fn from(e: keyring::Error) -> Self {
        Self::Keychain { reason: e.to_string() }
    }
}

/// Short text for a log note.
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout { .. } => f.write_str("timeout"),
            // Notes become UI message ids, so these read as the log's own words.
            Self::AuthKey { .. } => f.write_str("ключ не принят"),
            Self::AuthPass => f.write_str("пароль не принят"),
            Self::Disconnected => f.write_str("соединение закрыто"),
            Self::Dropped { .. } => f.write_str("сервер закрыл соединение до входа"),
            Self::BadName { .. } => f.write_str("имя не подходит для Windows, пропущен"),
            Self::NameClash { .. } => f.write_str("имя отличается от соседнего только регистром, пропущен"),
            Self::SourceChanged { .. } => f.write_str("источник изменился во время передачи"),
            Self::ServerChanged { .. } => f.write_str("файл на сервере изменился, правка не загружена"),
            Self::EditWaits { .. } => f.write_str("правка ждёт загрузку на паузе"),
            Self::NotRegular { .. } => f.write_str("не обычный файл, пропущен"),
            Self::Net { reason, .. } | Self::Ssh { reason } | Self::Keychain { reason } | Self::Einval { reason } => {
                f.write_str(reason)
            }
            Self::KeyFile { path, reason } => write!(f, "{path}: {reason}"),
            Self::KeyCheck { reason } => write!(f, "key check: {reason}"),
            Self::AuthMethods { methods } => write!(f, "methods: {methods}"),
            Self::Enoent { path } => write!(f, "ENOENT {path}"),
            Self::Enotdir { path } => write!(f, "ENOTDIR {path}"),
            Self::Eacces { path } => write!(f, "EACCES {path}"),
            Self::Eexist { path } => write!(f, "EEXIST {path}"),
            e => write!(f, "{}", serde_json::to_value(e).ok().and_then(|v| v["code"].as_str().map(str::to_owned)).unwrap_or_default()),
        }
    }
}
