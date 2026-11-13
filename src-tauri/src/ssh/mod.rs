//! SSH sessions: one connection per server and user carries SFTP (and later the terminals).
//!
//! Order of a connection: TCP (10 s), host key against `~/.ssh/known_hosts`, login, the SFTP
//! subsystem. Concurrent requests for one pair wait for one connection; at most three handshakes
//! run at once, because the provider cuts bursts of connections and the app starts many servers.
//! A dropped session reconnects with pauses of 1, 3, 10 and 30 s, then reports the error.

pub mod agent_fwd;
pub mod auth;
pub mod keys;
pub mod known_hosts;
pub mod prompt;
pub mod sftp;

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use russh::{
    Disconnect, Preferred, cipher,
    client::{self, Handle},
    keys::{Algorithm, PublicKey, PublicKeyOrCertificate},
};
use russh_sftp::client::RawSftpSession;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::{
    net::TcpStream,
    sync::{OwnedSemaphorePermit, Semaphore, watch},
};
use zeroize::Zeroizing;

use self::{
    auth::{KeySource, Method, Who},
    keys::Unlocked,
    known_hosts::{Check, KnownHosts},
    prompt::{HostKeyRequest, Prompts},
};
use crate::{
    error::{Error, Result},
    log::{Log, State},
    msg,
    store::{KeyMode, Server},
};

/// A running log row that ends as cancelled when its connect is dropped midway (a paused transfer).
struct RunRow<'a> {
    log: &'a Log,
    row: Option<u64>,
}

impl<'a> RunRow<'a> {
    fn new(log: &'a Log, row: u64) -> Self {
        RunRow { log, row: Some(row) }
    }

    fn done(mut self, note: impl Into<crate::log::Msg>, state: State) {
        if let Some(row) = self.row.take() {
            self.log.done(row, note, state);
        }
    }
}

impl Drop for RunRow<'_> {
    fn drop(&mut self) {
        if let Some(row) = self.row.take() {
            self.log.done(row, "отменено", State::Ok);
        }
    }
}

/// Tokio workers. SSH traffic is a chain of tasks handing buffers to each other; on 16 cores they
/// keep migrating and uploads lose 15-25% (measured in the spike).
pub const ASYNC_WORKERS: usize = 4;

const TCP_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKES: usize = 3;
const RECONNECT: [u64; 4] = [1, 3, 10, 30];

/// Flow-control window of each channel; must exceed the bytes in flight of the SFTP pipeline.
const WINDOW_SIZE: u32 = 64 * 1024 * 1024;
const MAX_PACKET_SIZE: u32 = 64 * 1024;

/// AES-GCM first: with AES-NI it is several times faster than ChaCha20-Poly1305, the library default.
const CIPHERS: &[cipher::Name] =
    &[cipher::AES_128_GCM, cipher::AES_256_GCM, cipher::CHACHA20_POLY1305, cipher::AES_256_CTR, cipher::AES_128_CTR];

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    Wait,
    Live,
    Bad,
}

#[derive(Serialize, Clone)]
struct StatusEvent<'a> {
    server: &'a str,
    user: &'a str,
    state: Option<SessionState>,
}

/// What the card's draft or a saved server says about where and how to log in.
pub struct Target {
    pub server: String,
    pub code: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub keepalive: u32,
}

impl Target {
    fn addr(&self) -> String {
        if self.host.contains(':') { format!("[{}]:{}", self.host, self.port) } else { format!("{}:{}", self.host, self.port) }
    }

    fn who(&self) -> Who<'_> {
        Who { server: &self.server, code: &self.code, user: &self.user, permit: None, waited: None }
    }
}

/// The key a user of a saved server logs in with.
pub fn key_source(server: &Server, user: &str) -> KeySource {
    let named = server.users.iter().find(|u| u.name == user).map(|u| u.key.as_str()).unwrap_or("auto");
    if named != "auto" && !named.is_empty() {
        return KeySource::File(keys::expand(named));
    }
    match server.key_mode {
        KeyMode::File if !server.key_file.is_empty() => KeySource::File(keys::expand(&server.key_file)),
        _ => KeySource::Auto,
    }
}

/* ---------- Handshake ---------- */

#[derive(Clone, Copy, PartialEq, Eq)]
enum HostMode {
    /// Ask the user about a new or changed key.
    Ask,
    /// "Test" in the card: never ask, never write; report what we saw.
    Report,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostSeen {
    None,
    Known,
    /// Trusted just now by the user.
    Trusted,
    /// Report mode: new key, accepted for this one connection only.
    Unverified(String),
    /// Report mode: another key is on record.
    Changed { old: String, new: String },
    Rejected,
    /// strictHost is off.
    Unchecked,
}

struct Shake {
    ssh: Weak<Ssh>,
    target_server: String,
    code: String,
    host: String,
    port: u16,
    mode: HostMode,
    sem: Arc<Semaphore>,
    permit: tokio::sync::Mutex<Option<OwnedSemaphorePermit>>,
    seen: Mutex<HostSeen>,
    /// The host key dialog was shown.
    asked: AtomicBool,
    /// The login waited for a passphrase dialog, its own or another connection's.
    waited: AtomicBool,
    first_kex: AtomicBool,
    dropped: watch::Sender<bool>,
    /// Direct copies that want the agent on this connection.
    fwd: agent_fwd::Forwards,
}

pub struct Client {
    shake: Arc<Shake>,
}

impl Shake {
    fn set_seen(&self, s: HostSeen) {
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> std::result::Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false); // host certificates are not offered, so not expected
        };
        let Some(ssh) = self.shake.ssh.upgrade() else { return Ok(false) };
        Ok(ssh.check_host(&self.shake, key).await)
    }

    async fn kex_done(
        &mut self,
        _secret: Option<&[u8]>,
        names: &russh::Names,
        session: &mut client::Session,
    ) -> std::result::Result<(), Self::Error> {
        if self.shake.first_kex.swap(false, Ordering::Relaxed)
            && let Some(ssh) = self.shake.ssh.upgrade()
        {
            let id = String::from_utf8_lossy(session.remote_sshid()).trim().to_owned();
            ssh.log.ok(&self.shake.code, "SSH", msg!("Сервер {v}", v = id));
            ssh.log.ok(&self.shake.code, "KEX", format!("{} · {}", names.kex.as_ref(), names.cipher.as_ref()));
        }
        Ok(())
    }

    /// Refused unless a direct copy runs on this connection; then filtered (see `agent_fwd`).
    async fn server_channel_open_agent_forward(
        &mut self,
        channel: russh::Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> std::result::Result<(), Self::Error> {
        agent_fwd::on_open(&self.shake.fwd, channel, reply).await;
        Ok(())
    }

    async fn disconnected(&mut self, reason: client::DisconnectReason<Self::Error>) -> std::result::Result<(), Self::Error> {
        let _ = self.shake.dropped.send(true);
        match reason {
            client::DisconnectReason::ReceivedDisconnect(_) => Ok(()),
            client::DisconnectReason::Error(e) => Err(e),
        }
    }
}

/// Host key types already on record go first, so a server with several keys shows the one we know
/// (OpenSSH does the same) and a new type is not mistaken for a changed key.
fn preferred_keys(known: &[PublicKey]) -> Cow<'static, [Algorithm]> {
    let base = Preferred::DEFAULT.key;
    if known.is_empty() {
        return base;
    }
    let fams: Vec<String> = known.iter().map(|k| known_hosts::family(&k.algorithm())).collect();
    let (mut first, rest): (Vec<_>, Vec<_>) = base.iter().cloned().partition(|a| fams.contains(&known_hosts::family(a)));
    first.extend(rest);
    Cow::Owned(first)
}

/// A connection after the handshake and login.
struct Shaken {
    handle: Handle<Client>,
    seen: HostSeen,
    /// Turns true when the connection drops.
    dropped: watch::Receiver<bool>,
    fwd: agent_fwd::Forwards,
}

/// A live connection after login with its SFTP channel.
pub struct Session {
    pub handle: Handle<Client>,
    /// Address as the saved server had it at connect time: a direct copy dials it from another server.
    pub host: String,
    pub port: u16,
    /// Agent channels are let through while one of these is held.
    pub fwd: agent_fwd::Forwards,
    pub sftp: RawSftpSession,
    pub home: String,
    /// The server's code at connect time: the log's source column.
    pub code: String,
    /// The server announced `posix-rename@openssh.com` at init.
    pub posix_rename: bool,
    closing: AtomicBool,
}

impl Session {
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// The SFTP channel died under a live connection (sftp-server killed): the connection is dropped
    /// on purpose, so the reconnect brings both back instead of every operation failing.
    pub async fn sftp_lost(&self) {
        if !self.is_closed() {
            let _ = self.handle.disconnect(Disconnect::ByApplication, "sftp channel lost", "en").await;
        }
    }

    pub(crate) async fn close(&self) {
        self.closing.store(true, Ordering::Relaxed);
        let _ = self.handle.disconnect(Disconnect::ByApplication, "", "en").await;
    }
}

/* ---------- Manager ---------- */

type Pair = (String, String);
type OnLive = Box<dyn Fn(&str) + Send + Sync>;

enum Slot {
    Pending { epoch: u64, fut: Shared<BoxFuture<'static, Result<Arc<Session>>>> },
    Live { epoch: u64, session: Arc<Session> },
}

pub struct Ssh {
    pub app: AppHandle,
    pub log: Arc<Log>,
    pub prompts: Prompts,
    pub unlocked: Unlocked,
    strict: AtomicBool,
    handshakes: Arc<Semaphore>,
    known_lock: tokio::sync::Mutex<()>,
    slots: Mutex<HashMap<Pair, Slot>>,
    reconnecting: Mutex<HashSet<Pair>>,
    /// Told the server id whenever a session goes live, by hand or by a reconnect: the transfer
    /// queue resumes what a disconnect paused.
    on_live: std::sync::OnceLock<OnLive>,
    epoch: AtomicU64,
}

impl Ssh {
    pub fn new(app: AppHandle, log: Arc<Log>) -> Arc<Self> {
        Arc::new(Self {
            app,
            log,
            prompts: Prompts::default(),
            unlocked: Unlocked::default(),
            strict: AtomicBool::new(true),
            handshakes: Arc::new(Semaphore::new(HANDSHAKES)),
            known_lock: tokio::sync::Mutex::new(()),
            slots: Mutex::new(HashMap::new()),
            reconnecting: Mutex::new(HashSet::new()),
            on_live: std::sync::OnceLock::new(),
            epoch: AtomicU64::new(0),
        })
    }

    pub fn set_strict(&self, on: bool) {
        self.strict.store(on, Ordering::Relaxed);
    }

    fn status(&self, server: &str, user: &str, state: Option<SessionState>) {
        let _ = self.app.emit("ssh://status", StatusEvent { server, user, state });
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<Pair, Slot>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    /* ----- host keys ----- */

    async fn known_text(&self) -> String {
        let path = self.known_path();
        tokio::task::spawn_blocking(move || std::fs::read_to_string(path).unwrap_or_default()).await.unwrap_or_default()
    }

    fn known_path(&self) -> PathBuf {
        keys::ssh_dir().join("known_hosts")
    }

    /// Keys on record for a host: a direct copy hands them to the other server to check this one.
    pub async fn known_keys(&self, host: &str, port: u16) -> Vec<PublicKey> {
        known_hosts::host_keys(&self.known_text().await, host, port)
    }

    pub async fn hostkey_known(&self, host: &str, port: u16) -> Option<String> {
        let text = self.known_text().await;
        let keys = known_hosts::host_keys(&text, host, port);
        let best = keys.iter().find(|k| k.algorithm() == Algorithm::Ed25519).or(keys.first());
        best.map(known_hosts::fingerprint)
    }

    pub async fn hostkey_forget(&self, host: &str, port: u16) -> Result<()> {
        let _g = self.known_lock.lock().await;
        let (kh, h) = (KnownHosts::new(self.known_path()), host.to_owned());
        tokio::task::spawn_blocking(move || kh.forget(&h, port))
            .await
            .map_err(Error::ssh)?
            .map_err(|e| Error::ssh(format!("known_hosts: {e}")))
    }

    async fn check_host(&self, shake: &Shake, key: &PublicKey) -> bool {
        let fp = known_hosts::fingerprint(key);
        let src = shake.code.as_str();
        let algo = key.algorithm().as_str().to_owned();
        if !self.strict.load(Ordering::Relaxed) {
            self.log.add(src, "KEY", format!("{algo} {fp}"), "Ключ хоста не проверяется: проверка выключена", State::Err);
            shake.set_seen(HostSeen::Unchecked);
            return true;
        }
        let check = known_hosts::check(&self.known_text().await, &shake.host, shake.port, key);
        let (mode, old, others) = match check {
            Check::Known => {
                self.log.add(src, "KEY", format!("{algo} {fp}"), "известен", State::Ok);
                shake.set_seen(HostSeen::Known);
                return true;
            }
            Check::Revoked => {
                self.log.err(src, "KEY", format!("{algo} {fp}"), "ключ отозван в known_hosts");
                shake.set_seen(HostSeen::Rejected);
                return false;
            }
            Check::Unknown { others } => ("tofu", None, others),
            Check::Changed { old } => ("changed", Some(old), false),
        };
        if shake.mode == HostMode::Report {
            if let Some(old) = old {
                self.log.err(src, "KEY", format!("{algo} {fp}"), "ключ хоста изменился");
                shake.set_seen(HostSeen::Changed { old, new: fp });
                return false;
            }
            self.log.add(src, "KEY", format!("{algo} {fp}"), "ещё не подтверждён", State::Ok);
            shake.set_seen(HostSeen::Unverified(fp));
            return true;
        }
        let row = RunRow::new(&self.log, self.log.run(src, "KEY", format!("{algo} {fp}")));
        // The question does not hold a handshake slot: other servers keep connecting meanwhile.
        shake.permit.lock().await.take();
        shake.asked.store(true, Ordering::Relaxed);
        let req = HostKeyRequest {
            req_id: 0,
            mode,
            server: shake.target_server.clone(),
            code: shake.code.clone(),
            host: shake.host.clone(),
            port: shake.port,
            algo: keys::host_algo(key),
            fp: fp.clone(),
            old,
            others,
        };
        let trust = self.prompts.host_key(&self.app, req).await;
        if let Ok(p) = shake.sem.clone().acquire_owned().await {
            *shake.permit.lock().await = Some(p);
        }
        if !trust {
            row.done(if mode == "changed" { "ключ хоста изменился" } else { "не подтверждён" }, State::Err);
            shake.set_seen(HostSeen::Rejected);
            return false;
        }
        let _g = self.known_lock.lock().await;
        let (kh, host, port, k) = (KnownHosts::new(self.known_path()), shake.host.clone(), shake.port, key.clone());
        let changed = mode == "changed";
        let saved = tokio::task::spawn_blocking(move || if changed { kh.replace(&host, port, &k) } else { kh.add(&host, port, &k) }).await;
        match saved {
            Ok(Ok(())) => row.done("сохранён в known_hosts", State::Ok),
            Ok(Err(e)) => row.done(msg!("не записан в known_hosts: {e}", e = e), State::Err),
            Err(e) => row.done(e.to_string(), State::Err),
        }
        shake.set_seen(HostSeen::Trusted);
        true
    }

    /* ----- one connection ----- */

    /// TCP, host key, login. The handshake slot is held until the login is done.
    async fn handshake(self: &Arc<Self>, t: &Target, method: &Method, mode: HostMode) -> Result<Shaken> {
        // A long look at the host key dialog can outlast the server's LoginGraceTime; the key is
        // saved by then, so one more try goes through without asking.
        match self.handshake_once(t, method, mode).await {
            Err((Error::Disconnected, true)) => {
                self.log.ok(&t.code, "SSH", "Сервер закрыл вход, пока ждали ответа · повтор");
                self.handshake_once(t, method, mode).await.map_err(|(e, _)| e)
            }
            r => r.map_err(|(e, _)| e),
        }
    }

    async fn handshake_once(
        self: &Arc<Self>,
        t: &Target,
        method: &Method,
        mode: HostMode,
    ) -> std::result::Result<Shaken, (Error, bool)> {
        let permit = self.handshakes.clone().acquire_owned().await.map_err(|e| (Error::ssh(e), false))?;
        let addr = t.addr();
        let src = t.code.as_str();
        let row = RunRow { log: &self.log, row: Some(self.log.run(src, "TCP", msg!("Подключение к {addr}", addr = addr))) };
        let started = Instant::now();
        let stream = match tokio::time::timeout(TCP_TIMEOUT, TcpStream::connect((t.host.as_str(), t.port))).await {
            Err(_) => {
                row.done("нет ответа за 10 с", State::Err);
                return Err((Error::Timeout { addr }, false));
            }
            Ok(Err(e)) => {
                row.done(e.to_string(), State::Err);
                return Err((Error::Net { addr, reason: e.to_string() }, false));
            }
            Ok(Ok(s)) => s,
        };
        let _ = stream.set_nodelay(true);
        row.done(msg!("соединение установлено · {ms} мс", ms = started.elapsed().as_millis()), State::Ok);

        let known = known_hosts::host_keys(&self.known_text().await, &t.host, t.port);
        let config = Arc::new(client::Config {
            window_size: WINDOW_SIZE,
            maximum_packet_size: MAX_PACKET_SIZE,
            keepalive_interval: (t.keepalive > 0).then(|| Duration::from_secs(t.keepalive.into())),
            keepalive_max: 3,
            nodelay: true,
            preferred: Preferred {
                cipher: Cow::Borrowed(CIPHERS),
                key: preferred_keys(&known),
                ..Preferred::default()
            },
            ..client::Config::default()
        });
        let (dropped, _) = watch::channel(false);
        let shake = Arc::new(Shake {
            ssh: Arc::downgrade(self),
            target_server: t.server.clone(),
            code: t.code.clone(),
            host: t.host.clone(),
            port: t.port,
            mode,
            sem: self.handshakes.clone(),
            permit: tokio::sync::Mutex::new(Some(permit)),
            seen: Mutex::new(HostSeen::None),
            asked: AtomicBool::new(false),
            waited: AtomicBool::new(false),
            first_kex: AtomicBool::new(true),
            dropped,
            fwd: agent_fwd::Forwards::default(),
        });
        let connected = client::connect_stream(config, stream, Client { shake: shake.clone() }).await;
        let seen = shake.seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let asked = shake.asked.load(Ordering::Relaxed);
        let mut handle = match connected {
            Ok(h) => h,
            Err(e) => {
                let err = match (&seen, Error::from(e)) {
                    (HostSeen::Rejected, _) => Error::HostkeyRejected,
                    (HostSeen::Changed { old, new }, _) => Error::HostkeyChanged { addr: addr.clone(), old: old.clone(), new: new.clone() },
                    // Closed before any host key: a ban, MaxStartups, not an SSH server.
                    (HostSeen::None, Error::Disconnected) => Error::Dropped { addr: addr.clone() },
                    (_, e) => e,
                };
                if !matches!(err, Error::HostkeyRejected | Error::HostkeyChanged { .. }) {
                    self.log.err(src, "SSH", "Рукопожатие не удалось", err.to_string());
                }
                return Err((err, asked && seen == HostSeen::Trusted));
            }
        };
        let dropped = shake.dropped.subscribe();
        // Report mode never sends a password to a host it could not verify.
        if let (HostSeen::Unverified(_), Method::Password(_)) = (&seen, method) {
            return Ok(Shaken { handle, seen, dropped, fwd: shake.fwd.clone() });
        }
        let who = auth::Who { permit: Some(&shake.permit), waited: Some(&shake.waited), ..t.who() };
        let res = auth::login(self, &mut handle, &who, method).await;
        drop(shake.permit.lock().await.take());
        match res {
            Ok(()) => Ok(Shaken { handle, seen, dropped, fwd: shake.fwd.clone() }),
            Err(e) => {
                // A passphrase dialog took longer than LoginGraceTime: the key is open now,
                // try once more. A password is never sent twice. `is_closed` covers the moment
                // between russh closing its queue and reporting the disconnect. After MaxAuthTries
                // this resends the keys once: bounded, and only when the login waited for the user.
                let retry = shake.waited.load(Ordering::Relaxed)
                    && (*dropped.borrow() || handle.is_closed())
                    && matches!(method, Method::Keys(_))
                    && matches!(e, Error::Disconnected | Error::AuthKey { .. });
                let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
                Err(if retry { (Error::Disconnected, true) } else { (e, false) })
            }
        }
    }

    async fn open_session(self: &Arc<Self>, t: &Target, method: &Method, mode: HostMode) -> Result<(Arc<Session>, watch::Receiver<bool>)> {
        let Shaken { handle, dropped, fwd, .. } = self.handshake(t, method, mode).await?;
        let (sftp, version, home, posix_rename) = match sftp::open(&handle).await {
            Ok(v) => v,
            Err(e) => {
                self.log.err(&t.code, "SFTP", "Подсистема SFTP не запустилась", e.to_string());
                let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
                return Err(e);
            }
        };
        self.log.ok(&t.code, "SFTP", msg!("Подсистема запущена, протокол v{v}", v = version));
        let s = Session { handle, host: t.host.clone(), port: t.port, fwd, sftp, home, code: t.code.clone(), posix_rename, closing: AtomicBool::new(false) };
        Ok((Arc::new(s), dropped))
    }

    /* ----- saved servers ----- */

    fn target(server: &Server, user: &str) -> Target {
        Target {
            server: server.id.clone(),
            code: server.code.clone(),
            host: server.host.trim().to_owned(),
            port: server.port,
            user: user.to_owned(),
            keepalive: server.keepalive,
        }
    }

    /// The password of a password-only server, for its default user only; others log in with keys.
    async fn method_for(server: &Server, user: &str) -> Result<Method> {
        if server.auth_pass && user == server.default_user {
            let (id, u) = (server.id.clone(), user.to_owned());
            let pass = tokio::task::spawn_blocking(move || crate::secrets::get(&id, &u)).await.map_err(Error::ssh)??;
            return pass.map(Method::Password).ok_or(Error::NoSecret);
        }
        Ok(Method::Keys(key_source(server, user)))
    }

    /// A second connection of the pair for transfers, so a big file does not slow the terminal echo.
    /// Same login and host key check; no sidebar status and no reconnect of its own: the queue retries.
    pub async fn open_xfer(self: &Arc<Self>, server: &str, user: &str) -> Result<(Arc<Session>, watch::Receiver<bool>)> {
        let saved = self.saved(server).await.ok_or(Error::ServerGone)?;
        let t = Self::target(&saved, user);
        let method = Self::method_for(&saved, user).await?;
        self.open_session(&t, &method, HostMode::Ask).await
    }

    pub fn session_live(&self, server: &str, user: &str) -> Result<Arc<Session>> {
        match self.slots().get(&(server.to_owned(), user.to_owned())) {
            Some(Slot::Live { session, .. }) if !session.is_closed() => Ok(session.clone()),
            _ => Err(Error::Disconnected),
        }
    }

    /// Sets the one listener for sessions going live.
    pub fn on_live(&self, f: impl Fn(&str) + Send + Sync + 'static) {
        let _ = self.on_live.set(Box::new(f));
    }

    /// The server is connected, under any user.
    pub fn has_session(&self, server: &str) -> bool {
        self.slots().iter().any(|((s, _), slot)| s == server && matches!(slot, Slot::Live { session, .. } if !session.is_closed()))
    }

    /// The session of the pair, connecting if needed. Callers that come while it connects share the result.
    pub async fn connect(self: &Arc<Self>, server: &Server, user: &str) -> Result<Arc<Session>> {
        let pair: Pair = (server.id.clone(), user.to_owned());
        let fut = {
            let mut slots = self.slots();
            match slots.get(&pair) {
                Some(Slot::Live { session, .. }) if !session.is_closed() => return Ok(session.clone()),
                Some(Slot::Pending { fut, .. }) => fut.clone(),
                _ => {
                    let epoch = self.epoch.fetch_add(1, Ordering::Relaxed);
                    let (me, srv, u) = (self.clone(), server.clone(), user.to_owned());
                    let fut = async move { me.establish(srv, u, epoch).await }.boxed().shared();
                    slots.insert(pair.clone(), Slot::Pending { epoch, fut: fut.clone() });
                    self.status(&pair.0, &pair.1, Some(SessionState::Wait));
                    fut
                }
            }
        };
        fut.await
    }

    async fn establish(self: Arc<Self>, server: Server, user: String, epoch: u64) -> Result<Arc<Session>> {
        let t = Self::target(&server, &user);
        let res = match Self::method_for(&server, &user).await {
            Ok(method) => self.open_session(&t, &method, HostMode::Ask).await,
            Err(e) => {
                if matches!(e, Error::NoSecret) {
                    self.log.err(&t.code, "AUTH", format!("password · {user}"), "пароль не сохранён");
                }
                Err(e)
            }
        };
        let pair: Pair = (server.id.clone(), user.clone());
        // Decide under the lock, act after it: the guard must not live across an await.
        let ours = {
            let mut slots = self.slots();
            let ours = matches!(slots.get(&pair), Some(Slot::Pending { epoch: g, .. }) if *g == epoch);
            if ours {
                match &res {
                    Ok((session, _)) => {
                        slots.insert(pair.clone(), Slot::Live { epoch, session: session.clone() });
                    }
                    Err(_) => {
                        slots.remove(&pair);
                    }
                }
            }
            ours
        };
        match res {
            Ok((session, dropped)) if ours => {
                self.status(&pair.0, &pair.1, Some(SessionState::Live));
                self.watch_drop(server, user, epoch, session.clone(), dropped);
                if let Some(f) = self.on_live.get() {
                    f(&pair.0);
                }
                Ok(session)
            }
            Ok((session, _)) => {
                // Disconnected while connecting: the new session is not wanted.
                session.close().await;
                Err(Error::Cancelled)
            }
            Err(e) => {
                if ours {
                    let again = self.reconnecting.lock().unwrap_or_else(|e| e.into_inner()).contains(&pair);
                    self.status(&pair.0, &pair.1, Some(if again { SessionState::Wait } else { SessionState::Bad }));
                }
                Err(e)
            }
        }
    }

    fn watch_drop(self: &Arc<Self>, server: Server, user: String, epoch: u64, session: Arc<Session>, mut dropped: watch::Receiver<bool>) {
        let me = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            let _ = dropped.wait_for(|d| *d).await;
            if session.closing.load(Ordering::Relaxed) {
                return;
            }
            if let Some(me) = me.upgrade() {
                me.reconnect(server, user, epoch).await;
            }
        });
    }

    /// The server as `servers.json` has it now.
    async fn saved(&self, id: &str) -> Option<Server> {
        let store = self.app.try_state::<Arc<crate::store::Store>>()?.inner().clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || store.load_servers()?.servers.into_iter().find(|s| s.id == id)).await.ok().flatten()
    }

    async fn reconnect(self: Arc<Self>, server: Server, user: String, epoch: u64) {
        let pair: Pair = (server.id.clone(), user.clone());
        {
            let mut slots = self.slots();
            if !matches!(slots.get(&pair), Some(Slot::Live { epoch: g, .. }) if *g == epoch) {
                return;
            }
            slots.remove(&pair);
        }
        self.log.err(&server.code, "SSH", msg!("Соединение потеряно · {u}", u = user), "");
        self.status(&pair.0, &pair.1, Some(SessionState::Wait));
        self.reconnecting.lock().unwrap_or_else(|e| e.into_inner()).insert(pair.clone());
        let still = |me: &Self| me.reconnecting.lock().unwrap_or_else(|e| e.into_inner()).contains(&pair);
        let mut ok = false;
        let mut server = server;
        for (k, pause) in RECONNECT.iter().enumerate() {
            tokio::time::sleep(Duration::from_secs(*pause)).await;
            if !still(&self) {
                return; // disconnected by the user meanwhile
            }
            // The card may have changed the address or the key since the session was opened.
            if let Some(saved) = self.saved(&server.id).await {
                server = saved;
            }
            self.log.ok(&server.code, "SSH", msg!("Повторное подключение · попытка {n}", n = k + 1));
            match self.connect(&server, &user).await {
                Ok(_) => {
                    ok = true;
                    break;
                }
                // A login or host key problem will not fix itself by waiting.
                Err(Error::Timeout { .. } | Error::Net { .. } | Error::Disconnected | Error::Dropped { .. } | Error::Ssh { .. }) => continue,
                Err(_) => break,
            }
        }
        let was = self.reconnecting.lock().unwrap_or_else(|e| e.into_inner()).remove(&pair);
        if !ok && was && !self.slots().contains_key(&pair) {
            self.status(&pair.0, &pair.1, Some(SessionState::Bad));
        }
    }

    /// Closes the pairs of a server (one user or all). A connection still being made is dropped when it lands.
    pub async fn disconnect(&self, server: &str, user: Option<&str>) {
        // A dialog still waiting for an answer would keep its connection hanging.
        if user.is_none() {
            self.prompts.cancel(&self.app, Some(server));
        }
        let mut gone = Vec::new();
        {
            let mut slots = self.slots();
            let keys: Vec<Pair> = slots.keys().filter(|(s, u)| s == server && user.is_none_or(|x| x == u)).cloned().collect();
            for k in keys {
                if let Some(slot) = slots.remove(&k) {
                    gone.push((k, slot));
                }
            }
        }
        let was_reconnecting: Vec<Pair> = {
            let mut rec = self.reconnecting.lock().unwrap_or_else(|e| e.into_inner());
            let hit: Vec<Pair> = rec.iter().filter(|(s, u)| s == server && user.is_none_or(|x| x == u)).cloned().collect();
            for p in &hit {
                rec.remove(p);
            }
            hit
        };
        for ((s, u), slot) in gone {
            if let Slot::Live { session, .. } = slot {
                session.close().await;
            }
            self.status(&s, &u, None);
        }
        for (s, u) in was_reconnecting {
            self.status(&s, &u, None);
        }
    }

    pub fn sessions(&self) -> Vec<(String, String, SessionState)> {
        self.slots()
            .iter()
            .map(|((s, u), slot)| {
                let st = match slot {
                    Slot::Pending { .. } => SessionState::Wait,
                    Slot::Live { session, .. } if session.is_closed() => SessionState::Wait,
                    Slot::Live { .. } => SessionState::Live,
                };
                (s.clone(), u.clone(), st)
            })
            .collect()
    }

    /* ----- the card ----- */

    /// "Test": connects with the draft, saves nothing, never asks about the host key.
    pub async fn test(self: &Arc<Self>, d: &Draft) -> Result<TestReport> {
        let t = d.target();
        let method = d.method().await?;
        let started = Instant::now();
        let Shaken { handle, seen, .. } = self.handshake(&t, &method, HostMode::Report).await?;
        let ms = started.elapsed().as_millis() as u64;
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        let password = matches!(method, Method::Password(_));
        Ok(match seen {
            HostSeen::Unverified(fp) => TestReport { login: !password, password, ms, unverified: Some(fp) },
            _ => TestReport { login: true, password, ms, unverified: None },
        })
    }

    /// Logs in with the card's password before the server is saved. With `install` puts our public
    /// key into `~/.ssh/authorized_keys` and proves it with a key login; success only after that.
    pub async fn first_login(self: &Arc<Self>, d: &Draft, install: bool) -> Result<()> {
        let t = d.target();
        let Method::Password(pass) = d.method().await? else { return Err(Error::AuthPass) };
        let source = d.key_source();
        let (pubkey, check_with) = if install { Some(auth::public_to_install(&source).await?) } else { None }.unzip();
        let method = Method::Password(pass);
        let (session, _) = self.open_session(&t, &method, HostMode::Ask).await?;
        drop(method);
        let Some(pubkey) = pubkey else {
            session.close().await;
            return Ok(());
        };
        let added = sftp::install_key(&session, &pubkey).await;
        session.close().await;
        match added {
            Ok(true) => _ = self.log.ok(&t.code, "KEY", msg!("Ключ {k} добавлен в authorized_keys", k = keys::label(&pubkey))),
            Ok(false) => _ = self.log.ok(&t.code, "KEY", msg!("Ключ {k} уже есть в authorized_keys", k = keys::label(&pubkey))),
            Err(e) => {
                self.log.err(&t.code, "KEY", "Ключ не добавлен в authorized_keys", e.to_string());
                return Err(e);
            }
        }
        let check = Method::Keys(check_with.unwrap_or(source));
        match self.handshake(&t, &check, HostMode::Ask).await {
            Ok(Shaken { handle: h, .. }) => {
                let _ = h.disconnect(Disconnect::ByApplication, "", "en").await;
                self.log.ok(&t.code, "KEY", "Контрольный вход по ключу прошёл");
                Ok(())
            }
            Err(e) => {
                self.log.err(&t.code, "KEY", "Контрольный вход по ключу не прошёл", e.to_string());
                Err(Error::KeyCheck { reason: e.to_string() })
            }
        }
    }
}

/// The card's unsaved fields.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub id: Option<String>,
    pub code: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key_mode: KeyMode,
    pub key_file: String,
    #[serde(default)]
    pub password: Zeroizing<String>,
    /// The saved server logs in by password: a test without a new password uses the stored one.
    #[serde(default)]
    pub auth_pass: bool,
}

impl Draft {
    fn target(&self) -> Target {
        Target {
            server: self.id.clone().unwrap_or_default(),
            code: if self.code.is_empty() { self.host.clone() } else { self.code.clone() },
            host: self.host.trim().to_owned(),
            port: self.port,
            user: self.user.clone(),
            keepalive: 0,
        }
    }

    fn key_source(&self) -> KeySource {
        match self.key_mode {
            KeyMode::File if !self.key_file.is_empty() => KeySource::File(keys::expand(&self.key_file)),
            _ => KeySource::Auto,
        }
    }

    /// A typed password wins; else the stored one of a password-only server; else keys.
    async fn method(&self) -> Result<Method> {
        if !self.password.is_empty() {
            return Ok(Method::Password(self.password.clone()));
        }
        if self.auth_pass
            && let Some(id) = self.id.clone()
        {
            let u = self.user.clone();
            let pass = tokio::task::spawn_blocking(move || crate::secrets::get(&id, &u)).await.map_err(Error::ssh)??;
            return pass.map(Method::Password).ok_or(Error::NoSecret);
        }
        Ok(Method::Keys(self.key_source()))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestReport {
    /// Logged in (false: the password was not sent to an unverified host).
    pub login: bool,
    pub password: bool,
    pub ms: u64,
    /// Fingerprint of a host key not yet trusted.
    pub unverified: Option<String>,
}
