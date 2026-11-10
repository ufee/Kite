//! Logging in: which key or password, in which order, with what in the log.
//!
//! Keys: a user with a named key uses that file from `~/.ssh`; `auto` follows the server's mode:
//! `auto` is the agent, then `~/.ssh/id_ed25519`; `file` is the chosen file. An encrypted key is
//! used through the agent when it is there, else from this run's unlocked keys, else the user is
//! asked for the passphrase once. Passwords: `password`, or `keyboard-interactive` with one prompt
//! when the server only offers that (Ubuntu with PAM).

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use russh::{
    MethodKind, MethodSet,
    client::{AuthResult, Handle, KeyboardInteractiveAuthResponse},
    keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey},
};
use zeroize::Zeroizing;

use super::{
    Client, Ssh,
    keys::{self, Agent, KeyFile},
    prompt::PassphraseRequest,
};
use crate::{
    error::{Error, Result},
    log::State,
    msg,
};

pub enum Method {
    Keys(KeySource),
    Password(Zeroizing<String>),
}

#[derive(Clone)]
pub enum KeySource {
    /// Agent, then `~/.ssh/id_ed25519`.
    Auto,
    File(PathBuf),
    /// One key the agent holds (the control login after installing it).
    AgentKey(PublicKey),
}

/// Who is logging in, for the log and the passphrase dialog.
pub struct Who<'a> {
    pub server: &'a str,
    pub code: &'a str,
    pub user: &'a str,
    /// The handshake slot, given back while the passphrase dialog waits for the user.
    pub permit: Option<&'a tokio::sync::Mutex<Option<tokio::sync::OwnedSemaphorePermit>>>,
    /// Set when this connection showed the passphrase dialog.
    pub prompted: Option<&'a AtomicBool>,
}

impl Who<'_> {
    /// Gives the handshake slot back while waiting for the user: other servers keep connecting.
    async fn pause(&self) {
        if let Some(p) = self.permit {
            p.lock().await.take();
        }
    }

    /// Takes a slot again before going on with the login.
    async fn resume(&self, ssh: &Ssh) {
        if let Some(p) = self.permit {
            let mut slot = p.lock().await;
            if slot.is_none()
                && let Ok(permit) = ssh.handshakes.clone().acquire_owned().await
            {
                *slot = Some(permit);
            }
        }
    }
}

fn methods_text(m: &MethodSet) -> String {
    m.iter().map(<&str>::from).collect::<Vec<_>>().join(", ")
}

/// Asks the server which methods it takes; a server with `none` lets us in right away.
async fn methods(h: &mut Handle<Client>, user: &str) -> Result<Option<MethodSet>> {
    match h.authenticate_none(user).await? {
        AuthResult::Success => Ok(None),
        AuthResult::Failure { remaining_methods, .. } => Ok(Some(remaining_methods)),
    }
}

pub async fn login(ssh: &Ssh, h: &mut Handle<Client>, who: &Who<'_>, method: &Method) -> Result<()> {
    let Some(offered) = methods(h, who.user).await? else {
        ssh.log.ok(who.code, "AUTH", msg!("Вход без пароля · {u}", u = who.user));
        return Ok(());
    };
    match method {
        Method::Password(p) => password(ssh, h, who, p, &offered).await,
        Method::Keys(src) => {
            if !offered.contains(&MethodKind::PublicKey) {
                ssh.log.err(who.code, "AUTH", format!("publickey · {}", who.user), msg!("сервер не принимает ключи: {m}", m = methods_text(&offered)));
                return Err(Error::AuthKey { user: who.user.into(), server: who.code.into() });
            }
            keys_login(ssh, h, who, src).await
        }
    }?;
    ssh.log.ok(who.code, "AUTH", "Вход выполнен");
    Ok(())
}

/* ---------- Password ---------- */

async fn password(ssh: &Ssh, h: &mut Handle<Client>, who: &Who<'_>, pass: &Zeroizing<String>, offered: &MethodSet) -> Result<()> {
    // One attempt only: retrying the same password through the other method doubles the failures
    // the server counts (fail2ban).
    if offered.contains(&MethodKind::Password) {
        let row = ssh.log.run(who.code, "AUTH", format!("password · {}", who.user));
        return match h.authenticate_password(who.user, pass.as_str()).await? {
            AuthResult::Success => {
                ssh.log.done(row, "", State::Ok);
                Ok(())
            }
            AuthResult::Failure { partial_success: true, .. } => {
                ssh.log.done(row, "нужен второй фактор", State::Err);
                Err(Error::AuthMulti)
            }
            AuthResult::Failure { .. } => {
                ssh.log.done(row, "пароль не принят", State::Err);
                Err(Error::AuthPass)
            }
        };
    }
    if offered.contains(&MethodKind::KeyboardInteractive) {
        return keyboard_interactive(ssh, h, who, pass).await;
    }
    ssh.log.err(who.code, "AUTH", format!("password · {}", who.user), msg!("сервер не принимает пароль: {m}", m = methods_text(offered)));
    Err(Error::AuthMethods { methods: methods_text(offered) })
}

async fn keyboard_interactive(ssh: &Ssh, h: &mut Handle<Client>, who: &Who<'_>, pass: &Zeroizing<String>) -> Result<()> {
    let row = ssh.log.run(who.code, "AUTH", format!("keyboard-interactive · {}", who.user));
    let mut answered = false;
    let mut reply = h.authenticate_keyboard_interactive_start(who.user, None).await?;
    // Servers may send empty info requests around the real one; a bound keeps a broken one from looping.
    for _ in 0..6 {
        match reply {
            KeyboardInteractiveAuthResponse::Success => {
                ssh.log.done(row, "", State::Ok);
                return Ok(());
            }
            KeyboardInteractiveAuthResponse::Failure { partial_success: true, .. } => {
                ssh.log.done(row, "нужен второй фактор", State::Err);
                return Err(Error::AuthMulti);
            }
            KeyboardInteractiveAuthResponse::Failure { .. } => {
                ssh.log.done(row, "пароль не принят", State::Err);
                return Err(Error::AuthPass);
            }
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                let answers = match prompts.len() {
                    0 => Vec::new(),
                    // A second question after the password is PAM asking again: the password was wrong,
                    // or it is a second factor. Either way we do not send it twice.
                    1 if !answered => {
                        answered = true;
                        vec![pass.to_string()]
                    }
                    1 => {
                        ssh.log.done(row, "пароль не принят", State::Err);
                        return Err(Error::AuthPass);
                    }
                    _ => {
                        ssh.log.done(row, "сервер задаёт несколько вопросов", State::Err);
                        return Err(Error::AuthMulti);
                    }
                };
                reply = h.authenticate_keyboard_interactive_respond(answers).await?;
            }
        }
    }
    ssh.log.done(row, "сервер не завершил вход", State::Err);
    Err(Error::AuthPass)
}

/* ---------- Keys ---------- */

struct Attempt {
    rsa: Option<HashAlg>,
    tried: Vec<PublicKey>,
}

impl Attempt {
    fn seen(&self, k: &PublicKey) -> bool {
        self.tried.iter().any(|t| t.key_data() == k.key_data())
    }
}

/// A server that drops us after too many keys (MaxAuthTries) means none of them was accepted.
fn key_error(e: russh::Error, who: &Who<'_>) -> Error {
    match Error::from(e) {
        Error::Disconnected => Error::AuthKey { user: who.user.into(), server: who.code.into() },
        e => e,
    }
}

async fn with_agent(ssh: &Ssh, h: &mut Handle<Client>, who: &Who<'_>, a: &mut Attempt, agent: &mut Agent, key: PublicKey, how: &str) -> Result<bool> {
    let row = ssh.log.run(who.code, "AUTH", format!("publickey · {} · {} · {how}", keys::algo_short(&key), keys::label(&key)));
    let hash = key.algorithm().is_rsa().then_some(a.rsa).flatten();
    a.tried.push(key.clone());
    let res = h.authenticate_publickey_with(who.user, key, hash, agent).await.map_err(|e| match e {
        russh::AgentAuthError::Send(_) => key_error(russh::Error::SendError, who),
        russh::AgentAuthError::Key(e) => Error::ssh(format!("agent: {e}")),
    });
    finish(ssh, row, res)
}

async fn with_key(ssh: &Ssh, h: &mut Handle<Client>, who: &Who<'_>, a: &mut Attempt, key: Arc<PrivateKey>, name: &str) -> Result<bool> {
    let public = key.public_key().clone();
    let row = ssh.log.run(who.code, "AUTH", format!("publickey · {} · {name}", keys::algo_short(&public)));
    let hash = public.algorithm().is_rsa().then_some(a.rsa).flatten();
    a.tried.push(public);
    let res = h.authenticate_publickey(who.user, PrivateKeyWithHashAlg::new(key, hash)).await;
    finish(ssh, row, res.map_err(|e| key_error(e, who)))
}

fn finish(ssh: &Ssh, row: u64, res: Result<AuthResult>) -> Result<bool> {
    match res {
        Ok(AuthResult::Success) => {
            ssh.log.done(row, "", State::Ok);
            Ok(true)
        }
        Ok(AuthResult::Failure { partial_success: true, .. }) => {
            ssh.log.done(row, "нужен второй фактор", State::Err);
            Err(Error::AuthMulti)
        }
        Ok(AuthResult::Failure { .. }) => {
            ssh.log.done(row, "ключ не принят", State::Err);
            Ok(false)
        }
        Err(e) => {
            ssh.log.done(row, e.to_string(), State::Err);
            Err(e)
        }
    }
}

async fn keys_login(ssh: &Ssh, h: &mut Handle<Client>, who: &Who<'_>, src: &KeySource) -> Result<()> {
    let rsa = h.best_supported_rsa_hash().await?.flatten();
    let mut a = Attempt { rsa, tried: Vec::new() };
    let mut agent = keys::agent().await;
    let in_agent = match agent.as_mut() {
        Some(ag) => keys::agent_keys(ag).await,
        None => Vec::new(),
    };
    let fail = || Error::AuthKey { user: who.user.into(), server: who.code.into() };

    match src {
        KeySource::Auto => {
            if let Some(ag) = agent.as_mut() {
                for key in in_agent.iter().cloned() {
                    if with_agent(ssh, h, who, &mut a, ag, key, "агент").await? {
                        return Ok(());
                    }
                }
            } else {
                ssh.log.ok(who.code, "KEY", "Агент OpenSSH не запущен");
            }
            let path = keys::ssh_dir().join("id_ed25519");
            if path.is_file() && file_login(ssh, h, who, &mut a, &mut agent, &in_agent, &path).await? {
                return Ok(());
            }
            Err(fail())
        }
        KeySource::File(path) => {
            if file_login(ssh, h, who, &mut a, &mut agent, &in_agent, path).await? {
                return Ok(());
            }
            Err(fail())
        }
        KeySource::AgentKey(key) => {
            if let Some(ag) = agent.as_mut()
                && with_agent(ssh, h, who, &mut a, ag, key.clone(), "агент").await?
            {
                return Ok(());
            }
            Err(fail())
        }
    }
}

/// One key file: through the agent if it holds the key, as is if not encrypted, else unlocked.
async fn file_login(
    ssh: &Ssh,
    h: &mut Handle<Client>,
    who: &Who<'_>,
    a: &mut Attempt,
    agent: &mut Option<Agent>,
    in_agent: &[PublicKey],
    path: &std::path::Path,
) -> Result<bool> {
    let kf = match KeyFile::load(path) {
        Ok(kf) => kf,
        Err(e) => {
            ssh.log.err(who.code, "KEY", msg!("Ключ {k} не прочитан", k = path.display()), e.to_string());
            return Err(e);
        }
    };
    let name = kf.name();
    if kf.public.clone().or_else(|| ssh.unlocked.public_of(&kf.path)).is_some_and(|p| a.seen(&p)) {
        return Ok(false);
    }
    let ready = match ready(ssh, &kf, in_agent) {
        Some(r) => r,
        None => {
            // One dialog per key file. A connection that waited here usually finds the key open:
            // in the agent (asked again, the list above is stale) or in this run's memory.
            who.pause().await;
            let gate = ssh.unlocked.gate(&kf.path);
            let turn = gate.lock().await;
            let now_in_agent = match agent.as_mut() {
                Some(ag) => keys::agent_keys(ag).await,
                None => Vec::new(),
            };
            let r = match ready(ssh, &kf, &now_in_agent) {
                Some(r) => Ok(r),
                None => unlock(ssh, who, &kf, agent).await.map(Ready::Key),
            };
            drop(turn);
            who.resume(ssh).await;
            r?
        }
    };
    match ready {
        Ready::Agent(public) => match agent.as_mut() {
            Some(ag) => with_agent(ssh, h, who, a, ag, public, &name).await,
            None => Ok(false),
        },
        Ready::Key(key) => with_key(ssh, h, who, a, key, &name).await,
    }
}

enum Ready {
    Agent(PublicKey),
    Key(Arc<PrivateKey>),
}

/// The key without asking anyone: through the agent, unlocked earlier in this run, or not encrypted.
fn ready(ssh: &Ssh, kf: &KeyFile, in_agent: &[PublicKey]) -> Option<Ready> {
    if let Some(public) = kf.public.clone().or_else(|| ssh.unlocked.public_of(&kf.path)) {
        if in_agent.iter().any(|k| k.key_data() == public.key_data()) {
            return Some(Ready::Agent(public));
        }
        if let Some(key) = ssh.unlocked.find(&public) {
            return Some(Ready::Key(key));
        }
    }
    kf.plain.as_ref().map(|k| Ready::Key(Arc::new(k.clone())))
}

/// Asks for the passphrase until it opens the key or the user cancels. The phrase is wiped as soon
/// as the key is decrypted; the key goes to the agent or stays in memory until Kite closes.
async fn unlock(ssh: &Ssh, who: &Who<'_>, kf: &KeyFile, agent: &mut Option<Agent>) -> Result<Arc<PrivateKey>> {
    let name = kf.name();
    let req = PassphraseRequest {
        req_id: 0,
        server: who.server.into(),
        code: who.code.into(),
        user: who.user.into(),
        file: keys::contract(&kf.path),
        agent: agent.is_some(),
    };
    ssh.log.ok(who.code, "KEY", msg!("Ключ {k} защищён паролем", k = name));
    let Some((id, mut rx)) = ssh.prompts.passphrase(&ssh.app, req) else { return Err(Error::Cancelled) };
    if let Some(p) = who.prompted {
        p.store(true, Ordering::Relaxed);
    }
    let result = loop {
        let Some(answer) = rx.recv().await else { break Err(Error::Cancelled) };
        let Some(phrase) = answer.phrase else {
            let _ = answer.reply.send(Ok(()));
            ssh.log.err(who.code, "KEY", msg!("Ключ {k} не открыт", k = name), "отменено");
            break Err(Error::Cancelled);
        };
        let Some(key) = kf.decrypt(&phrase) else {
            drop(phrase);
            ssh.log.err(who.code, "KEY", msg!("Ключ {k} не открыт", k = name), "неверная фраза");
            let _ = answer.reply.send(Err(Error::Passphrase));
            continue;
        };
        drop(phrase);
        let key = Arc::new(key);
        ssh.unlocked.learn(&kf.path, key.public_key().clone());
        let mut in_agent = false;
        if answer.add_to_agent && agent.is_some() {
            match keys::agent_add(&key, &kf.path.to_string_lossy()).await {
                Ok(()) => {
                    in_agent = true;
                    ssh.log.ok(who.code, "KEY", msg!("Ключ {k} добавлен в агент OpenSSH", k = name));
                }
                Err(e) => {
                    ssh.log.err(who.code, "KEY", msg!("Ключ {k} не добавлен в агент", k = name), e.to_string());
                }
            }
        }
        if !in_agent {
            ssh.unlocked.add(key.clone());
            ssh.log.ok(who.code, "KEY", msg!("Ключ {k} открыт до закрытия Kite", k = name));
        }
        let _ = answer.reply.send(Ok(()));
        break Ok(key);
    };
    ssh.prompts.passphrase_done(id);
    result
}

/* ---------- Public key to install ---------- */

/// The `.pub` next to the key in use, else the first ed25519 in the agent.
pub async fn public_to_install(src: &KeySource) -> Result<(PublicKey, KeySource)> {
    let file = match src {
        KeySource::File(p) => Some(p.clone()),
        KeySource::Auto => Some(keys::ssh_dir().join("id_ed25519")),
        KeySource::AgentKey(k) => return Ok((k.clone(), src.clone())),
    };
    if let Some(path) = file
        && let Ok(key) = russh::keys::load_public_key(keys::pub_path(&path))
    {
        return Ok((key, KeySource::File(path)));
    }
    if let Some(mut ag) = keys::agent().await
        && let Some(key) = keys::agent_keys(&mut ag).await.into_iter().find(|k| k.algorithm() == russh::keys::Algorithm::Ed25519)
    {
        return Ok((key.clone(), KeySource::AgentKey(key)));
    }
    Err(Error::NoPubkey)
}
