//! The SFTP subsystem of a session, and the key install of the first login.

use russh::{
    client::Handle,
    keys::{PublicKey, PublicKeyBase64},
};
use russh_sftp::{
    client::{Config as SftpConfig, RawSftpSession, error::Error as SftpError},
    protocol::{FileAttributes, OpenFlags, StatusCode},
};

use super::Client;
use crate::error::{Error, Result};

/// SFTP frame limit; must fit a full `limits@openssh.com` read reply.
const SFTP_MAX_PACKET: u32 = 1024 * 1024;
/// Queueing delay of a deep pipeline on a slow link must not look like a timeout.
const SFTP_TIMEOUT_SECS: u64 = 120;

/// Maps an SFTP failure on `path` to the codes the panels know.
pub fn fs_error(e: SftpError, path: &str) -> Error {
    match e {
        SftpError::Status(s) => match s.status_code {
            StatusCode::NoSuchFile => Error::Enoent { path: path.into() },
            StatusCode::PermissionDenied => Error::Eacces { path: path.into() },
            StatusCode::Eof => Error::Einval { reason: "eof".into() },
            _ => Error::Einval { reason: format!("{path}: {}", s.error_message) },
        },
        SftpError::Timeout | SftpError::IO(_) => Error::Disconnected,
        e => Error::Einval { reason: format!("{path}: {e}") },
    }
}

/// Opens the subsystem; returns the session, its protocol version and the home folder.
pub async fn open(handle: &Handle<Client>) -> Result<(RawSftpSession, u32, String)> {
    let channel = handle.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let raw = RawSftpSession::new_with_config(
        channel.into_stream(),
        SftpConfig { max_packet_len: SFTP_MAX_PACKET, request_timeout_secs: SFTP_TIMEOUT_SECS, ..SftpConfig::default() },
    );
    let version = raw.init().await.map_err(Error::ssh)?;
    // Chunk sizes for transfers come from `limits@openssh.com` (stage 6); listing does not need them.
    let home = raw
        .realpath(".")
        .await
        .map_err(|e| fs_error(e, "."))?
        .files
        .into_iter()
        .next()
        .map(|f| f.filename)
        .unwrap_or_else(|| "/".into());
    Ok((raw, version.version, home))
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

/// Reads a small file whole; a missing file reads as empty.
async fn read_small(sftp: &RawSftpSession, path: &str) -> Result<Vec<u8>> {
    let handle = match sftp.open(path, OpenFlags::READ, FileAttributes::empty()).await {
        Ok(h) => h.handle,
        Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => return Ok(Vec::new()),
        Err(e) => return Err(fs_error(e, path)),
    };
    let mut out = Vec::new();
    loop {
        match sftp.read(handle.as_str(), out.len() as u64, 32 * 1024).await {
            Ok(d) if d.data.is_empty() => break,
            Ok(d) => out.extend(d.data),
            Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => break,
            Err(e) => {
                let _ = sftp.close(handle).await;
                return Err(fs_error(e, path));
            }
        }
        // authorized_keys is a few KB; anything huge is not one we should rewrite.
        if out.len() > 4 * 1024 * 1024 {
            let _ = sftp.close(handle).await;
            return Err(Error::Einval { reason: format!("{path}: too big") });
        }
    }
    let _ = sftp.close(handle).await;
    Ok(out)
}

/// Is this key (type and blob) on one of the lines? Options in front of the key are allowed.
pub fn has_key(authorized: &str, key: &PublicKey) -> bool {
    let blob = key.public_key_base64();
    authorized.lines().any(|l| {
        let l = l.trim();
        !l.starts_with('#') && l.split_whitespace().any(|f| f == blob)
    })
}

fn mode(bits: u32) -> FileAttributes {
    FileAttributes { permissions: Some(bits), ..FileAttributes::empty() }
}

/// `~/.ssh` (700, created if missing), our line appended to `authorized_keys` if absent, file 600.
/// Appends at the end of the file instead of rewriting it, so nothing already there can be lost.
/// Returns false when the key was already there.
pub async fn install_key(s: &super::Session, key: &PublicKey) -> Result<bool> {
    let sftp = &s.sftp;
    let dir = join(&s.home, ".ssh");
    match sftp.stat(dir.as_str()).await {
        Ok(a) if a.attrs.is_dir() => {}
        Ok(_) => return Err(Error::Enotdir { path: dir }),
        Err(SftpError::Status(st)) if st.status_code == StatusCode::NoSuchFile => {
            sftp.mkdir(dir.as_str(), mode(0o700)).await.map_err(|e| fs_error(e, &dir))?;
            // The server's umask may have trimmed the mode.
            sftp.setstat(dir.as_str(), mode(0o700)).await.map_err(|e| fs_error(e, &dir))?;
        }
        Err(e) => return Err(fs_error(e, &dir)),
    }
    let file = join(&dir, "authorized_keys");
    let current = read_small(sftp, &file).await?;
    let text = String::from_utf8_lossy(&current);
    let added = !has_key(&text, key);
    if added {
        let mut line = String::new();
        if !current.is_empty() && !current.ends_with(b"\n") {
            line.push('\n');
        }
        line.push_str(&key.to_openssh().map_err(Error::ssh)?);
        line.push('\n');
        let h = sftp
            .open(file.as_str(), OpenFlags::WRITE | OpenFlags::CREATE, mode(0o600))
            .await
            .map_err(|e| fs_error(e, &file))?
            .handle;
        let res = sftp.write(h.as_str(), current.len() as u64, line.into_bytes()).await;
        let _ = sftp.close(h).await;
        res.map_err(|e| fs_error(e, &file))?;
    }
    // sshd refuses keys from a file others can write (StrictModes).
    sftp.setstat(file.as_str(), mode(0o600)).await.map_err(|e| fs_error(e, &file))?;
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_key_with_options_and_comments() {
        let key = russh::keys::parse_public_key_base64(
            "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ",
        )
        .unwrap();
        let b = key.public_key_base64();
        assert!(has_key(&format!("ssh-ed25519 {b} me@pc\n"), &key));
        assert!(has_key(&format!("from=\"10.0.0.0/8\",no-pty ssh-ed25519 {b}\n"), &key));
        assert!(!has_key(&format!("# ssh-ed25519 {b}\n"), &key));
        assert!(!has_key("ssh-ed25519 AAAAother\n", &key));
    }
}
