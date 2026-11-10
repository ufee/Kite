//! Passwords in the OS keychain: Credential Manager on Windows, Secret Service on Linux.
//! One entry per server and user; the server id survives renaming, so the entry does too.

use zeroize::Zeroizing;

use crate::error::Result;

const SERVICE: &str = "Kite";

fn entry(server: &str, user: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, &format!("{server}/{user}"))?)
}

/// False on a Linux box without a Secret Service: the card hides the password-only option.
pub fn available() -> bool {
    keyring::Entry::store_status().is_ok()
}

pub fn set(server: &str, user: &str, password: &str) -> Result<()> {
    entry(server, user)?.set_password(password)?;
    Ok(())
}

pub fn get(server: &str, user: &str) -> Result<Option<Zeroizing<String>>> {
    match entry(server, user)?.get_password() {
        Ok(p) => Ok(Some(Zeroizing::new(p))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn delete(server: &str, user: &str) -> Result<()> {
    match entry(server, user)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
