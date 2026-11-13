//! Files of a pane: "This PC" (`local`) and server folders over the session's SFTP channel.
//! Rights are the file system's business: nothing here guesses from permission bits, the real
//! error (`EACCES`, `ENOENT`, ...) goes to the UI.

pub mod local;
pub mod remote;

use serde::Serialize;

use crate::error::Error;

/// Files bigger than this are not shown in the viewer: open them in an editor.
pub const MAX_TEXT: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    /// Follows a link's target; a broken link is a file.
    pub dir: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub link: bool,
    pub size: u64,
    /// Milliseconds, like the UI's Date.
    pub mtime: u64,
    /// `rwxr-xr-x`, without the type letter; empty locally.
    pub perm: String,
    pub owner: String,
    pub group: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// Puts a listing close to the pane's own order: folders first, then names compared without case
/// and with digit runs as numbers. The UI still sorts with its collator and decides the order; on an
/// almost sorted input that sort is several times cheaper (10k names: 27 against 2 ms).
pub fn presort(list: &mut [Entry]) {
    // One key per entry: comparing them is a plain byte compare, not a walk over both names.
    list.sort_by_cached_key(|e| (!e.dir, sort_key(&e.name)));
}

/// The name in lower case with each digit run zero-padded to one width, so `file-9` sorts before
/// `file-10`. Runs longer than the width keep their length: the hint is rougher there.
fn sort_key(name: &str) -> String {
    const WIDTH: usize = 20;
    let mut key = String::with_capacity(name.len() + WIDTH);
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            let mut run = String::from(c);
            while let Some(d) = chars.next_if(char::is_ascii_digit) {
                run.push(d);
            }
            let run = run.trim_start_matches('0');
            key.extend(std::iter::repeat_n('0', WIDTH.saturating_sub(run.len())));
            key.push_str(run);
        } else {
            key.extend(c.to_lowercase());
        }
    }
    key
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextFile {
    pub text: String,
    pub binary: bool,
    pub too_big: bool,
}

impl TextFile {
    pub fn too_big() -> Self {
        Self { text: String::new(), binary: false, too_big: true }
    }

    /// A NUL byte near the start means binary, like git decides it. UTF-8 (a BOM is dropped),
    /// anything else is read as cp1251: the old Windows files of a Russian admin.
    pub fn from_bytes(data: &[u8]) -> Self {
        if data[..data.len().min(8000)].contains(&0) {
            return Self { text: String::new(), binary: true, too_big: false };
        }
        let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
        let text = match std::str::from_utf8(data) {
            Ok(s) => s.to_owned(),
            Err(_) => cp1251(data),
        };
        Self { text, binary: false, too_big: false }
    }
}

/// Windows-1251 bytes 0x80..0xBF; 0xC0..0xFF are А..я in a row. 0x98 is unassigned and kept as is.
const CP1251_HIGH: &str = "ЂЃ‚ѓ„…†‡€‰Љ‹ЊЌЋЏђ‘’“”•–—\u{98}™љ›њќћџ\u{a0}ЎўЈ¤Ґ¦§Ё©Є«¬\u{ad}®Ї°±Ііґµ¶·ё№є»јЅѕї";

fn cp1251(data: &[u8]) -> String {
    let high: Vec<char> = CP1251_HIGH.chars().collect();
    data.iter()
        .map(|&b| match b {
            0..=0x7F => b as char,
            0x80..=0xBF => high[(b - 0x80) as usize],
            _ => char::from_u32(0x410 + (b - 0xC0) as u32).unwrap_or('\u{fffd}'),
        })
        .collect()
}

/// POSIX path from what the user typed: `~` is the home folder, a relative path hangs off `base`
/// (or the home folder), `.` and `..` are folded. Links are not resolved: the path stays as typed.
pub fn normalize(raw: &str, home: &str, base: Option<&str>) -> String {
    let raw = raw.trim();
    let full = if raw == "~" {
        home.to_owned()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else if raw.starts_with('/') {
        raw.to_owned()
    } else {
        format!("{}/{raw}", base.unwrap_or(home))
    };
    let mut parts: Vec<&str> = Vec::new();
    for p in full.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

pub fn perm_text(mode: u32) -> String {
    const BITS: [(u32, char); 9] =
        [(0o400, 'r'), (0o200, 'w'), (0o100, 'x'), (0o040, 'r'), (0o020, 'w'), (0o010, 'x'), (0o004, 'r'), (0o002, 'w'), (0o001, 'x')];
    let mut s: Vec<char> = BITS.iter().map(|&(b, c)| if mode & b != 0 { c } else { '-' }).collect();
    // setuid, setgid, sticky as ls shows them
    for (bit, at, on, off) in [(0o4000, 2, 's', 'S'), (0o2000, 5, 's', 'S'), (0o1000, 8, 't', 'T')] {
        if mode & bit != 0 {
            s[at] = if s[at] == 'x' { on } else { off };
        }
    }
    s.into_iter().collect()
}

/// One argument for a POSIX shell: single quotes, a quote inside becomes `'\''`.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A POSIX script as one command for any login shell (sh, bash, zsh, fish, tcsh). Their quoting
/// rules differ (fish reads `\'` inside single quotes, tcsh `!`), so the script goes in as printf
/// octal escapes: the login shell sees no quote, backslash, `$`, `!` or newline of it.
pub fn login_line(script: &str) -> String {
    use std::fmt::Write;
    let mut enc = String::with_capacity(login_len(script));
    for b in script.bytes() {
        if login_safe(b) {
            enc.push(b as char);
        } else {
            let _ = write!(enc, "\\{b:03o}");
        }
    }
    format!("exec sh -c 'eval \"$(printf \"{enc}\")\"'")
}

fn login_safe(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b" _-./,:=@+".contains(&b)
}

/// Length of a piece of script once in [`login_line`]: an escaped byte takes four characters.
pub fn login_len(s: &str) -> usize {
    s.bytes().map(|b| if login_safe(b) { 1 } else { 4 }).sum()
}

/// A user or group name as chown takes it: no colon, space or shell character can get through.
pub fn valid_account(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '$'))
}

pub fn io_error(e: &std::io::Error, path: &str) -> Error {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => Error::Enoent { path: path.into() },
        K::PermissionDenied => Error::Eacces { path: path.into(), source: false },
        K::AlreadyExists => Error::Eexist { path: path.into() },
        K::NotADirectory => Error::Enotdir { path: path.into() },
        _ => Error::Einval { reason: format!("{path}: {e}") },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize("~", "/home/u", None), "/home/u");
        assert_eq!(normalize("~/www/../logs/", "/home/u", None), "/home/u/logs");
        assert_eq!(normalize("etc", "/root", Some("/")), "/etc");
        assert_eq!(normalize("conf", "/root", None), "/root/conf");
        assert_eq!(normalize("/var//www/./site", "/root", None), "/var/www/site");
        assert_eq!(normalize("/../..", "/root", None), "/");
    }

    #[test]
    fn permission_text() {
        assert_eq!(perm_text(0o755), "rwxr-xr-x");
        assert_eq!(perm_text(0o644), "rw-r--r--");
        assert_eq!(perm_text(0o4755), "rwsr-xr-x");
        assert_eq!(perm_text(0o1777), "rwxrwxrwt");
        assert_eq!(perm_text(0o2640), "rw-r-S---");
    }

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(sh_quote("/var/www"), "'/var/www'");
        assert_eq!(sh_quote("it's; rm -rf /"), r"'it'\''s; rm -rf /'");
        assert_eq!(sh_quote("$(id)`x`"), "'$(id)`x`'");
    }

    #[test]
    fn login_line_hides_shell_characters() {
        let script = "printf '%s\\n' \"it's\" $HOME `id` !x\nnext";
        let line = login_line(script);
        let body = line.strip_prefix("exec sh -c 'eval \"$(printf \"").and_then(|r| r.strip_suffix("\")\"'")).unwrap();
        // Only safe characters and octal escapes reach the login shell.
        assert!(body.bytes().all(|b| b.is_ascii_alphanumeric() || b" _-./,:=@+\\".contains(&b)), "{body}");
        let mut out = Vec::new();
        let mut it = body.bytes().peekable();
        while let Some(b) = it.next() {
            if b == b'\\' {
                let d: String = (0..3).map(|_| it.next().unwrap() as char).collect();
                out.push(u8::from_str_radix(&d, 8).unwrap());
            } else {
                out.push(b);
            }
        }
        assert_eq!(String::from_utf8(out).unwrap(), script);
        // The length the batches are measured by is the escaped one.
        assert_eq!(login_len(script), body.len());
        assert_eq!(login_len("/путь"), 1 + 4 * 8);
    }

    #[test]
    fn accounts() {
        assert!(valid_account("www-data"));
        assert!(valid_account("1000"));
        assert!(valid_account("svc$"));
        assert!(!valid_account(""));
        assert!(!valid_account("-R"));
        assert!(!valid_account("a:b"));
        assert!(!valid_account("a b"));
        assert!(!valid_account("a;id"));
    }

    #[test]
    fn text_or_binary() {
        assert!(TextFile::from_bytes(b"\x7fELF\0\0").binary);
        let t = TextFile::from_bytes("строка\n".as_bytes());
        assert!(!t.binary && t.text == "строка\n");
        assert_eq!(TextFile::from_bytes(b"\xEF\xBB\xBFok").text, "ok");
    }

    #[test]
    fn cp1251_fallback() {
        assert_eq!(super::CP1251_HIGH.chars().count(), 64);
        // "Ёжик №1 — ok" in cp1251
        let t = TextFile::from_bytes(b"\xA8\xE6\xE8\xEA \xB91 \x97 ok");
        assert_eq!(t.text, "Ёжик №1 — ok");
        assert_eq!(TextFile::from_bytes(b"\xC0\xFF").text, "Ая");
    }

    #[test]
    fn presort_puts_folders_first_then_names_like_the_pane() {
        let e = |name: &str, dir: bool| Entry { name: name.into(), dir, link: false, size: 0, mtime: 0, perm: String::new(), owner: String::new(), group: String::new(), hidden: false };
        let mut l = vec![e("file-11.log", false), e("Б", false), e("b", true), e("file-9.log", false), e("A", false), e("file-010.log", false), e("а", false)];
        presort(&mut l);
        let names: Vec<&str> = l.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["b", "A", "file-9.log", "file-010.log", "file-11.log", "а", "Б"]);
    }
}
