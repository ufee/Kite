//! `~/.ssh/known_hosts` in the OpenSSH format, shared with the system `ssh`.
//!
//! Supported: plain and hashed (`|1|salt|hash`) host patterns, `[host]:port` for non-standard ports,
//! comma-separated pattern lists, `*`/`?` wildcards, `!` negation, the `@revoked` marker.
//! `@cert-authority` lines are skipped: Kite does not accept host certificates.
//! Kite only appends plain lines and removes the exact host it owns; other lines stay byte for byte.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use data_encoding::BASE64;
use ring::hmac;
use russh::keys::{Algorithm, HashAlg, PublicKey, PublicKeyBase64, parse_public_key_base64};

/// What `known_hosts` says about the key a server presented.
#[derive(Debug, PartialEq, Eq)]
pub enum Check {
    /// The same key is on record.
    Known,
    /// Nothing of this key type on record; `others` tells whether keys of other types are.
    Unknown { others: bool },
    /// A different key of the same type is on record: possible man in the middle.
    Changed { old: String },
    /// The key is explicitly marked `@revoked`.
    Revoked,
}

/// The host as OpenSSH writes it: `host` on port 22, `[host]:port` otherwise.
pub fn host_name(host: &str, port: u16) -> String {
    let host = host.to_ascii_lowercase();
    if port == 22 { host } else { format!("[{host}]:{port}") }
}

pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// RSA keys are one type whatever signature hash was negotiated.
pub fn family(alg: &Algorithm) -> String {
    match alg {
        Algorithm::Rsa { .. } => "ssh-rsa".into(),
        a => a.as_str().into(),
    }
}

struct Line<'a> {
    marker: Option<&'a str>,
    patterns: &'a str,
    key: Option<PublicKey>,
}

fn parse_line(raw: &str) -> Option<Line<'_>> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut fields = line.split_whitespace();
    let mut first = fields.next()?;
    let mut marker = None;
    if first.starts_with('@') {
        marker = Some(first);
        first = fields.next()?;
    }
    let _key_type = fields.next()?;
    let blob = fields.next()?;
    Some(Line { marker, patterns: first, key: parse_public_key_base64(blob).ok() })
}

/// `*` matches any run, `?` any one character; ASCII case-insensitive like OpenSSH.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p].eq_ignore_ascii_case(&text[t])) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

fn hashed_match(pattern: &str, name: &str) -> bool {
    let Some(rest) = pattern.strip_prefix("|1|") else { return false };
    let Some((salt, hash)) = rest.split_once('|') else { return false };
    let (Ok(salt), Ok(hash)) = (BASE64.decode(salt.as_bytes()), BASE64.decode(hash.as_bytes())) else {
        return false;
    };
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &salt);
    hmac::verify(&key, name.as_bytes(), &hash).is_ok()
}

/// A pattern list matches when some pattern matches and no negated one does.
fn patterns_match(patterns: &str, name: &str) -> bool {
    if patterns.starts_with("|1|") {
        return hashed_match(patterns, name);
    }
    let mut hit = false;
    for p in patterns.split(',') {
        if let Some(neg) = p.strip_prefix('!') {
            if glob(neg.as_bytes(), name.as_bytes()) {
                return false;
            }
        } else if glob(p.as_bytes(), name.as_bytes()) {
            hit = true;
        }
    }
    hit
}

fn same_key(a: &PublicKey, b: &PublicKey) -> bool {
    a.key_data() == b.key_data()
}

/// Keys on record for the host, revoked and CA lines excluded.
pub fn host_keys(text: &str, host: &str, port: u16) -> Vec<PublicKey> {
    let name = host_name(host, port);
    text.lines()
        .filter_map(parse_line)
        .filter(|l| l.marker.is_none() && patterns_match(l.patterns, &name))
        .filter_map(|l| l.key)
        .collect()
}

pub fn check(text: &str, host: &str, port: u16, key: &PublicKey) -> Check {
    let name = host_name(host, port);
    let fam = family(&key.algorithm());
    let mut old = None;
    let mut others = false;
    for line in text.lines().filter_map(parse_line) {
        let Some(k) = &line.key else { continue };
        match line.marker {
            // A revoked key is refused whatever host it is listed for.
            Some("@revoked") if same_key(k, key) => return Check::Revoked,
            Some(_) => continue,
            None => {}
        }
        if !patterns_match(line.patterns, &name) {
            continue;
        }
        if same_key(k, key) {
            return Check::Known;
        }
        if family(&k.algorithm()) == fam {
            old.get_or_insert_with(|| fingerprint(k));
        } else {
            others = true;
        }
    }
    match old {
        Some(old) => Check::Changed { old },
        None => Check::Unknown { others },
    }
}

/// The line Kite appends: plain host name, key, no comment.
pub fn entry(host: &str, port: u16, key: &PublicKey) -> String {
    format!("{} {} {}", host_name(host, port), key.algorithm().as_str(), key.public_key_base64())
}

/// Drops the host from the text. A plain pattern list loses only this host; a hashed line that
/// matches goes whole. Wildcard and negated patterns are left alone: they are the user's rules.
pub fn remove(text: &str, host: &str, port: u16) -> String {
    let name = host_name(host, port);
    let mut out = String::with_capacity(text.len());
    for raw in text.split_inclusive('\n') {
        let Some(line) = parse_line(raw) else {
            out.push_str(raw);
            continue;
        };
        if line.marker.is_some() {
            out.push_str(raw);
            continue;
        }
        if line.patterns.starts_with("|1|") {
            if !hashed_match(line.patterns, &name) {
                out.push_str(raw);
            }
            continue;
        }
        let kept: Vec<&str> = line.patterns.split(',').filter(|p| !p.eq_ignore_ascii_case(&name)).collect();
        if kept.len() == line.patterns.split(',').count() {
            out.push_str(raw);
        } else if !kept.is_empty() {
            // Same line with the host taken out of the list.
            let start = raw.find(line.patterns).unwrap_or(0);
            out.push_str(&raw[..start]);
            out.push_str(&kept.join(","));
            out.push_str(&raw[start + line.patterns.len()..]);
        }
    }
    out
}

/// The file itself. Reads tolerate a missing file; writes create `~/.ssh` when needed.
pub struct KnownHosts {
    path: PathBuf,
}

impl KnownHosts {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn read(&self) -> String {
        fs::read_to_string(&self.path).unwrap_or_default()
    }

    /// Appends rather than rewrites: a line the system `ssh` adds meanwhile is not lost.
    pub fn add(&self, host: &str, port: u16, key: &PublicKey) -> std::io::Result<()> {
        ensure_dir(&self.path)?;
        let text = self.read();
        let lead = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{lead}{}", entry(host, port, key))?;
        f.sync_all()
    }

    pub fn forget(&self, host: &str, port: u16) -> std::io::Result<()> {
        let text = self.read();
        let next = remove(&text, host, port);
        if next == text {
            return Ok(());
        }
        crate::store::write_atomic(&self.path, next.as_bytes())
    }

    /// A changed key replaces every record of the host, like `ssh-keygen -R` followed by a new trust.
    pub fn replace(&self, host: &str, port: u16, key: &PublicKey) -> std::io::Result<()> {
        self.forget(host, port)?;
        self.add(host, port, key)
    }
}

fn ensure_dir(path: &Path) -> std::io::Result<()> {
    match path.parent() {
        Some(dir) if !dir.exists() => fs::create_dir_all(dir),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED_A: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    const ED_B: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G1sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";

    fn key(b64: &str) -> PublicKey {
        parse_public_key_base64(b64).unwrap()
    }

    #[test]
    fn host_names() {
        assert_eq!(host_name("Example.COM", 22), "example.com");
        assert_eq!(host_name("10.0.0.1", 2222), "[10.0.0.1]:2222");
    }

    #[test]
    fn known_unknown_changed() {
        let text = format!("# comment\n\nweb1,10.0.0.1 ssh-ed25519 {ED_A}\n[db1]:2222 ssh-ed25519 {ED_B} note\n");
        assert_eq!(check(&text, "web1", 22, &key(ED_A)), Check::Known);
        assert_eq!(check(&text, "10.0.0.1", 22, &key(ED_A)), Check::Known);
        assert_eq!(check(&text, "db1", 2222, &key(ED_B)), Check::Known);
        // Same host on another port is another host.
        assert_eq!(check(&text, "db1", 22, &key(ED_B)), Check::Unknown { others: false });
        match check(&text, "web1", 22, &key(ED_B)) {
            Check::Changed { old } => assert_eq!(old, fingerprint(&key(ED_A))),
            c => panic!("{c:?}"),
        }
    }

    #[test]
    fn hashed_hosts() {
        // `ssh-keygen -H` style entry for "web1", salt fixed for the test.
        let salt = [7u8; 20];
        let k = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &salt);
        let tag = hmac::sign(&k, b"web1");
        let line = format!("|1|{}|{} ssh-ed25519 {ED_A}\n", BASE64.encode(&salt), BASE64.encode(tag.as_ref()));
        assert_eq!(check(&line, "web1", 22, &key(ED_A)), Check::Known);
        assert_eq!(check(&line, "web2", 22, &key(ED_A)), Check::Unknown { others: false });
        assert_eq!(remove(&line, "web2", 22), line);
        assert_eq!(remove(&line, "web1", 22), "");
    }

    #[test]
    fn wildcards_and_negation() {
        let text = format!("*.prod,!db.prod ssh-ed25519 {ED_A}\n");
        assert_eq!(check(&text, "web.prod", 22, &key(ED_A)), Check::Known);
        assert_eq!(check(&text, "db.prod", 22, &key(ED_A)), Check::Unknown { others: false });
        assert!(glob(b"w?b*", b"web1"));
        assert!(!glob(b"w?b", b"web1"));
        // A user's wildcard rule is never rewritten.
        assert_eq!(remove(&text, "web.prod", 22), text);
    }

    #[test]
    fn revoked_and_markers() {
        let text = format!("@revoked * ssh-ed25519 {ED_A}\n@cert-authority * ssh-ed25519 {ED_B}\n");
        assert_eq!(check(&text, "any", 22, &key(ED_A)), Check::Revoked);
        // CA lines are not host keys.
        assert_eq!(check(&text, "any", 22, &key(ED_B)), Check::Unknown { others: false });
        assert!(host_keys(&text, "any", 22).is_empty());
    }

    #[test]
    fn remove_keeps_other_lines() {
        let text = format!("web1,10.0.0.1 ssh-ed25519 {ED_A} c\r\n# keep\nweb2 ssh-ed25519 {ED_B}\nweb1 ssh-ed25519 {ED_B}");
        let out = remove(&text, "web1", 22);
        assert_eq!(out, format!("10.0.0.1 ssh-ed25519 {ED_A} c\r\n# keep\nweb2 ssh-ed25519 {ED_B}\n"));
    }

    #[test]
    fn entry_round_trip() {
        let line = entry("db1", 2222, &key(ED_B));
        assert_eq!(line, format!("[db1]:2222 ssh-ed25519 {ED_B}"));
        assert_eq!(check(&line, "db1", 2222, &key(ED_B)), Check::Known);
    }

    #[test]
    fn file_add_forget_replace() {
        let dir = std::env::temp_dir().join(format!("kite-kh-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let kh = KnownHosts::new(dir.join(".ssh").join("known_hosts"));
        kh.add("web1", 22, &key(ED_A)).unwrap();
        fs::write(&kh.path, kh.read().trim_end()).unwrap(); // no newline at the end, like a hand edit
        kh.add("db1", 2222, &key(ED_B)).unwrap();
        assert_eq!(kh.read(), format!("web1 ssh-ed25519 {ED_A}\n[db1]:2222 ssh-ed25519 {ED_B}\n"));
        kh.replace("web1", 22, &key(ED_B)).unwrap();
        assert_eq!(check(&kh.read(), "web1", 22, &key(ED_B)), Check::Known);
        kh.forget("db1", 2222).unwrap();
        assert_eq!(kh.read(), format!("web1 ssh-ed25519 {ED_B}\n"));
        let _ = fs::remove_dir_all(&dir);
    }
}
