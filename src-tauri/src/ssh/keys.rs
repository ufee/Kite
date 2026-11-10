//! Key files in `~/.ssh`, the OpenSSH agent and keys unlocked for this run.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use russh::keys::{
    PrivateKey, PublicKey,
    agent::{AgentIdentity, client::AgentClient},
    ssh_key,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{Error, Result};

pub fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn ssh_dir() -> PathBuf {
    home().join(".ssh")
}

/// Key paths are stored the way the user sees them (`~/.ssh/x` or `%USERPROFILE%\.ssh\x`)
/// so `servers.json` moves between machines. A bare name means a file in `~/.ssh`.
pub fn expand(path: &str) -> PathBuf {
    let path = path.trim();
    for prefix in ["~/", "~\\", "%USERPROFILE%\\", "%USERPROFILE%/"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            return home().join(rest);
        }
    }
    let p = Path::new(path);
    if p.is_absolute() || path.contains(['/', '\\']) { p.to_path_buf() } else { ssh_dir().join(path) }
}

/// The reverse of `expand` for a picked file.
pub fn contract(path: &Path) -> String {
    match path.strip_prefix(home()) {
        Ok(rest) if cfg!(windows) => format!("%USERPROFILE%\\{}", rest.display()),
        Ok(rest) => format!("~/{}", rest.display().to_string().replace('\\', "/")),
        Err(_) => path.display().to_string(),
    }
}

/// Private keys in `~/.ssh` that have a `.pub` next to them, by file name.
pub fn list() -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(ssh_dir()) else { return Vec::new() };
    let mut names: Vec<String> = dir
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.ends_with(".pub") && ssh_dir().join(format!("{n}.pub")).is_file())
        .collect();
    names.sort_by(|a, b| (a != "id_ed25519").cmp(&(b != "id_ed25519")).then(a.cmp(b)));
    names
}

/// A key file as read from disk. An OpenSSH key keeps its public half in the clear even when
/// encrypted, so we can ask the agent about it before asking for a passphrase.
pub struct KeyFile {
    pub path: PathBuf,
    text: zeroize::Zeroizing<String>,
    pub public: Option<PublicKey>,
    /// Ready to sign: the file was not encrypted.
    pub plain: Option<PrivateKey>,
}

impl KeyFile {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map(zeroize::Zeroizing::new)
            .map_err(|e| Error::KeyFile { path: path.display().to_string(), reason: e.kind().to_string() })?;
        let pub_file = || russh::keys::load_public_key(pub_path(path)).ok();
        if let Ok(key) = PrivateKey::from_openssh(text.as_bytes()) {
            let public = Some(key.public_key().clone());
            let plain = (!key.is_encrypted()).then_some(key);
            return Ok(Self { path: path.into(), text, public, plain });
        }
        // PEM, PKCS#8, PuTTY: no public half without decrypting, the `.pub` file stands in.
        match russh::keys::decode_secret_key(&text, None) {
            Ok(key) => Ok(Self { path: path.into(), public: Some(key.public_key().clone()), plain: Some(key), text }),
            Err(russh::keys::Error::KeyIsEncrypted) => Ok(Self { path: path.into(), public: pub_file(), plain: None, text }),
            Err(e) => Err(Error::KeyFile { path: path.display().to_string(), reason: e.to_string() }),
        }
    }

    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// `None` when the passphrase is wrong.
    pub fn decrypt(&self, passphrase: &str) -> Option<PrivateKey> {
        if let Ok(key) = PrivateKey::from_openssh(self.text.as_bytes()) {
            return key.decrypt(passphrase).ok();
        }
        russh::keys::decode_secret_key(&self.text, Some(passphrase)).ok()
    }
}

pub fn pub_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".pub");
    PathBuf::from(p)
}

pub fn algo_short(key: &PublicKey) -> String {
    match key.algorithm() {
        ssh_key::Algorithm::Ed25519 => "ed25519".into(),
        ssh_key::Algorithm::Rsa { .. } => "rsa".into(),
        ssh_key::Algorithm::Ecdsa { curve } => format!("ecdsa-{}", curve.as_str().trim_start_matches("nistp")),
        a => a.as_str().into(),
    }
}

/// Keys unlocked with a passphrase without going to the agent: kept until Kite closes.
/// `PrivateKey` wipes its secret half when dropped.
#[derive(Default)]
pub struct Unlocked(Mutex<Vec<Arc<PrivateKey>>>);

impl Unlocked {
    pub fn find(&self, public: &PublicKey) -> Option<Arc<PrivateKey>> {
        let keys = self.0.lock().unwrap_or_else(|e| e.into_inner());
        keys.iter().find(|k| k.public_key().key_data() == public.key_data()).cloned()
    }

    pub fn add(&self, key: Arc<PrivateKey>) {
        let mut keys = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !keys.iter().any(|k| k.public_key().key_data() == key.public_key().key_data()) {
            keys.push(key);
        }
    }
}

/* ---------- Agent ---------- */

pub type Agent = AgentClient<Box<dyn russh::keys::agent::client::AgentStream + Send + Unpin + 'static>>;

/// The OpenSSH agent: the Windows service pipe, `SSH_AUTH_SOCK` elsewhere. `None` when it is not running.
pub async fn agent() -> Option<Agent> {
    #[cfg(windows)]
    let client = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await.ok()?;
    #[cfg(unix)]
    let client = AgentClient::connect_env().await.ok()?;
    Some(client.dynamic())
}

/// Puts a private key into the agent with a comment, like `ssh-add` does (the file path), so the
/// agent lists it by name. russh's `add_identity` always sends an empty comment.
pub async fn agent_add(key: &PrivateKey, comment: &str) -> std::io::Result<()> {
    use ssh_key::encoding::Encode;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const ADD_IDENTITY: u8 = 17;
    const SUCCESS: u8 = 6;

    let mut body = zeroize::Zeroizing::new(Vec::with_capacity(4096));
    body.push(ADD_IDENTITY);
    key.key_data().encode(&mut *body).map_err(std::io::Error::other)?;
    comment.encode(&mut *body).map_err(std::io::Error::other)?;
    let len = u32::try_from(body.len()).map_err(std::io::Error::other)?;

    #[cfg(windows)]
    let mut stream = tokio::net::windows::named_pipe::ClientOptions::new().open(r"\\.\pipe\openssh-ssh-agent")?;
    #[cfg(unix)]
    let mut stream = tokio::net::UnixStream::connect(std::env::var("SSH_AUTH_SOCK").map_err(std::io::Error::other)?).await?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&body).await?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    let mut reply = vec![0u8; (u32::from_be_bytes(head) as usize).clamp(1, 1024)];
    stream.read_exact(&mut reply).await?;
    if reply[0] == SUCCESS { Ok(()) } else { Err(std::io::Error::other("the agent refused the key")) }
}

pub async fn agent_keys<S>(agent: &mut AgentClient<S>) -> Vec<PublicKey>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    agent
        .request_identities()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| match id {
            // The agent keeps the comment beside the key blob; `label` reads it from the key.
            AgentIdentity::PublicKey { mut key, comment } => {
                key.set_comment(comment);
                Some(key)
            }
            _ => None,
        })
        .collect()
}

/// How a key shows up in the settings: the file name from its comment, else the fingerprint.
pub fn label(key: &PublicKey) -> String {
    let comment = key.comment().as_str_lossy().trim();
    if comment.is_empty() {
        return key.fingerprint(ssh_key::HashAlg::Sha256).to_string();
    }
    // ssh-add on Windows stores the full path as the comment.
    comment.rsplit(['/', '\\']).next().unwrap_or(comment).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_and_contract() {
        let h = home();
        assert_eq!(expand("id_ed25519"), h.join(".ssh").join("id_ed25519"));
        assert_eq!(expand("~/.ssh/work"), h.join(".ssh/work"));
        assert_eq!(expand("%USERPROFILE%\\.ssh\\work"), h.join(".ssh\\work"));
        let picked = h.join(".ssh").join("work");
        assert_eq!(expand(&contract(&picked)), if cfg!(windows) { h.join(".ssh\\work") } else { picked });
    }
}
