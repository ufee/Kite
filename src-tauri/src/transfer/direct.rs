//! Direct copy between two servers: rsync runs on the source server and logs in to the target with
//! the forwarded agent, so the bytes never pass this PC. Without rsync on both ends a single file
//! goes by scp; a folder cannot (scp writes straight into the target and does not merge).
//!
//! The source checks the target by the host keys on record here (a temporary known_hosts, strict),
//! never by its own list. The command runs under a PTY: when the channel closes (pause, cancel, a
//! dropped connection) the session's processes get SIGHUP, and rsync keeps what it has in
//! `.kite-rsync-part` for the next run. Whatever stops the copy before it starts — no agent, no
//! rsync, a refused key, a target out of reach — is `DIRECT_FAILED`: the queue asks the user to send
//! it through Kite instead.

use std::{sync::atomic::Ordering, time::Duration};

use russh::ChannelMsg;

use super::{
    io::{Kind, Side},
    job::{Run, unchanged},
};
use crate::{
    error::{Error, Result},
    fs::{sh_quote, valid_account},
    ssh::{Session, keys, known_hosts},
};

/// rsync's folder for what an interrupted run has of a file, next to that file.
pub const PART_DIR: &str = ".kite-rsync-part";
/// Lines of the command's output kept to explain a failure.
const TAIL: usize = 24;
/// A probe of the tools on a server.
const PROBE_WAIT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Rsync,
    Scp,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Rsync => "rsync",
            Tool::Scp => "scp",
        }
    }
}

fn failed(reason: impl Into<String>) -> Error {
    Error::DirectFailed { reason: reason.into() }
}

/// What the source needs to reach the target.
struct Reach {
    user: String,
    host: String,
    port: u16,
    /// known_hosts lines for the target, as the source's ssh looks it up.
    known: Vec<String>,
}

impl Run<'_> {
    /// The whole item by the direct route. Its size is known first, as for the relay.
    pub async fn direct(&self, from: &Side, to: &Side) -> Result<()> {
        let (Some(fc), Some(tc)) = (from.conn(), to.conn()) else { return Err(failed("not server to server")) };
        let it = self.item;
        let src = from.join(&it.from.path, &it.name);
        let dst = to.join(&it.to.path, &it.tname);
        let meta = from.stat(&src).await?.ok_or_else(|| Error::Enoent { path: src.clone() })?;
        if meta.kind == Kind::Other {
            return Err(Error::NotRegular { path: src });
        }
        // The picked item may be a link: what it points to goes, as by the relay. Moving it deletes
        // the link afterwards, never its target's files.
        let is_link = from.lstat(&src).await?.is_some_and(|m| m.kind == Kind::Link);
        let link = it.mv && is_link;
        let dir = meta.kind == Kind::Dir;
        let mut dirs: Vec<String> = Vec::new();
        if dir {
            self.q.set_state(self.id, super::St::Count);
            let nodes = from.walk(&src).await?;
            let total = nodes.iter().filter(|n| n.meta.kind == Kind::File).map(|n| n.meta.size).sum();
            dirs = nodes.iter().filter(|n| n.meta.kind == Kind::Dir).map(|n| n.rel.clone()).collect();
            dirs.sort_by_key(|r| std::cmp::Reverse(r.matches('/').count()));
            self.q.set_size(self.id, total);
            self.q.set_state(self.id, super::St::Run);
        } else {
            self.q.set_size(self.id, meta.size);
        }
        // A resumed run keeps what it showed: rsync starts from its partial file, not from zero.
        if !self.resume.direct {
            self.done().store(0, Ordering::Relaxed);
        }

        let reach = self.reach(tc).await?;
        let tool = probe(&fc.session, &tc.session, dir).await?;
        self.q.direct_started(self.id, tool.name());

        match tool {
            Tool::Rsync => {
                let mv = it.mv && !link;
                let (s, d) = if dir { (format!("{src}/"), format!("{dst}/")) } else { (src.clone(), dst.clone()) };
                // A folder's link is followed by the trailing slash; a file's needs -L.
                let cmd = rsync_cmd(&reach, &s, &d, self.keep_time, mv, is_link && !dir);
                self.exec(&cmd, &reach, &dst).await?;
                if mv && dir {
                    // rsync deleted the files it sent; the emptied folders go, deepest first.
                    for rel in &dirs {
                        let _ = from.rmdir(&from.join(&src, rel)).await;
                    }
                    let _ = from.rmdir(&src).await;
                }
            }
            Tool::Scp => {
                let part = to.part_of(&dst);
                self.q.part_started(self.id, "", meta.size, meta.mtime);
                let cmd = scp_cmd(&reach, &src, &part, self.keep_time);
                self.exec(&cmd, &reach, &dst).await?;
                to.rename_over(&part, &dst).await?;
                self.q.part_done(self.id, "");
                if it.mv && !link {
                    unchanged(from, &src, &meta, false).await?;
                    from.remove(&src).await?;
                }
            }
        }
        self.done().store(self.item_size(), Ordering::Relaxed);
        if link {
            from.remove_link(&src).await?;
        }
        Ok(())
    }

    fn item_size(&self) -> u64 {
        self.q.size_of(self.id).unwrap_or(0)
    }

    /// The target as the source will dial it, and the keys it must show.
    async fn reach(&self, tc: &super::conn::Conn) -> Result<Reach> {
        let s = &tc.session;
        let user = self.item.to.user.clone();
        if !valid_account(&user) {
            return Err(failed(format!("имя пользователя {user} не подходит для ssh")));
        }
        if !host_ok(&s.host) {
            return Err(failed(format!("адрес {} не подходит для ssh", s.host)));
        }
        let mut agent = keys::agent().await.ok_or_else(|| failed("агент OpenSSH не запущен"))?;
        if keys::agent_keys(&mut agent).await.is_none_or(|k| k.is_empty()) {
            return Err(failed("в агенте OpenSSH нет ключей"));
        }
        let keys = self.q.ssh.known_keys(&s.host, s.port).await;
        if keys.is_empty() {
            return Err(failed(format!("ключ хоста {} не сохранён в known_hosts", s.code)));
        }
        let known = keys.iter().map(|k| known_hosts::entry(&s.host, s.port, k)).collect();
        Ok(Reach { user, host: s.host.clone(), port: s.port, known })
    }

    /// Runs the copy on the source, with the agent forwarded for as long as it runs.
    async fn exec(&self, cmd: &str, reach: &Reach, dst: &str) -> Result<()> {
        // sshd forwards the agent once per connection and keeps its socket until the connection
        // ends: each run gets a connection of its own, so the socket lives as long as the run.
        let (s, _) = self.q.ssh.open_xfer(&self.item.from.src, &self.item.from.user).await?;
        let _bye = Bye(s.clone());
        let _fwd = s.fwd.hold();
        let mut ch = Closing(Some(s.handle.channel_open_session().await?));
        ch.get()?.agent_forward(true).await?;
        if !reply(ch.get()?).await? {
            return Err(failed("сервер-источник не разрешает проброс агента"));
        }
        // Without a PTY a dropped channel leaves the copy running; it still finishes, only later.
        ch.get()?.request_pty(true, "dumb", 200, 24, 0, 0, &[]).await?;
        let _ = reply(ch.get()?).await?;
        ch.get()?.exec(true, cmd).await?;
        if !reply(ch.get()?).await? {
            return Err(failed("на сервере-источнике нельзя запускать команды"));
        }

        let size = self.item_size();
        let mut out = Output::default();
        let mut stop = self.stop.clone();
        let mut code: Option<u32> = None;
        loop {
            let c = ch.get()?;
            let msg = tokio::select! {
                biased;
                _ = super::io::stopped(&mut stop) => None,
                m = c.wait() => Some(m),
            };
            let Some(msg) = msg else {
                ch.close().await;
                return Err(Error::Stopped);
            };
            match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    for p in out.feed(&data) {
                        let now = progress(p, size);
                        if now > self.done().load(Ordering::Relaxed) {
                            self.done().store(now, Ordering::Relaxed);
                        }
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => code = Some(exit_status),
                Some(ChannelMsg::ExitSignal { .. }) => code = Some(255),
                Some(ChannelMsg::Close) | None => break,
                _ => {}
            }
        }
        match code {
            Some(0) => Ok(()),
            // The channel ended without a status: the connection went.
            None => Err(Error::Disconnected),
            Some(n) => Err(explain(n, &out.tail(), reach, dst)),
        }
    }
}

/// Closes the channel when the copy is dropped midway (a lost connection on the other side), so
/// the source's processes get their SIGHUP now and not when the connection times out.
struct Closing(Option<russh::Channel<russh::client::Msg>>);

impl Closing {
    fn get(&mut self) -> Result<&mut russh::Channel<russh::client::Msg>> {
        self.0.as_mut().ok_or(Error::Disconnected)
    }

    async fn close(&mut self) {
        if let Some(c) = self.0.take() {
            let _ = c.close().await;
        }
    }
}

impl Drop for Closing {
    fn drop(&mut self) {
        if let Some(c) = self.0.take() {
            tokio::spawn(async move {
                let _ = c.close().await;
            });
        }
    }
}

/// Closes the run's own connection however the run ends.
struct Bye(std::sync::Arc<Session>);

impl Drop for Bye {
    fn drop(&mut self) {
        let s = self.0.clone();
        tokio::spawn(async move { s.close().await });
    }
}

/// The answer to the last request sent with `want_reply`.
async fn reply(c: &mut russh::Channel<russh::client::Msg>) -> Result<bool> {
    loop {
        match c.wait().await {
            Some(ChannelMsg::Success) => return Ok(true),
            Some(ChannelMsg::Failure) => return Ok(false),
            Some(ChannelMsg::Close) | None => return Err(Error::Disconnected),
            _ => {}
        }
    }
}

/// rsync on both ends: the folder copy; scp on the source: a single file.
async fn probe(src: &Session, dst: &Session, dir: bool) -> Result<Tool> {
    let (a, b) = futures::future::join(tools(src), tools(dst)).await;
    let a = a?.ok_or_else(|| failed(format!("на {} нельзя запускать команды", src.code)))?;
    let b = b?.ok_or_else(|| failed(format!("на {} нельзя запускать команды", dst.code)))?;
    let on = |out: &str, t: &str| out.lines().any(|l| l.trim() == t);
    if on(&a, "rsync") && on(&b, "rsync") {
        return Ok(Tool::Rsync);
    }
    if !dir && on(&a, "scp") {
        return Ok(Tool::Scp);
    }
    let missing = if on(&a, "rsync") { &dst.code } else { &src.code };
    Err(failed(format!("нет rsync на {missing}")))
}

/// Which of rsync and scp a server has, one per line.
async fn tools(s: &Session) -> Result<Option<String>> {
    let cmd = sh_line("command -v rsync >/dev/null 2>&1 && echo rsync; command -v scp >/dev/null 2>&1 && echo scp; echo kite-probe");
    tokio::time::timeout(PROBE_WAIT, capture(&s.handle, &cmd)).await.map_err(|_| failed(format!("{} не ответил на проверку rsync", s.code)))?
}

/// Output of a short command; None when the server runs no commands (no marker came back).
async fn capture(h: &russh::client::Handle<crate::ssh::Client>, cmd: &str) -> Result<Option<String>> {
    let mut ch = h.channel_open_session().await?;
    ch.exec(true, cmd).await?;
    let mut out = Vec::new();
    while let Some(m) = ch.wait().await {
        match m {
            ChannelMsg::Data { data } if out.len() < 16384 => out.extend_from_slice(&data),
            ChannelMsg::Failure => return Ok(None),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    let out = String::from_utf8_lossy(&out).into_owned();
    Ok(out.lines().any(|l| l.trim() == "kite-probe").then_some(out))
}

/// A host name or address as ssh takes it on a command line: nothing a shell or ssh reads as more.
fn host_ok(h: &str) -> bool {
    !h.is_empty() && !h.starts_with('-') && h.len() <= 255 && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_'))
}

/// `user@host:path` with an IPv6 address in brackets.
fn remote(r: &Reach, path: &str) -> String {
    let host = if r.host.contains(':') { format!("[{}]", r.host) } else { r.host.clone() };
    format!("{}@{}:{}", r.user, host, path)
}

/// The ssh that rsync or scp starts on the source: our known keys only, no prompts, no config of
/// the source user, nothing forwarded further.
fn ssh_opts() -> &'static str {
    "-F /dev/null -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=\"$k\" -o GlobalKnownHostsFile=/dev/null \
     -o PreferredAuthentications=publickey -o ForwardAgent=no -o ClearAllForwardings=yes -o ConnectTimeout=15 \
     -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o LogLevel=ERROR"
}

/// The script around a copy: a private temporary known_hosts that goes on any exit, the user's own
/// umask for the copy itself, then `body`. The shell runs a foreground command's trap only once that
/// command ends, so `body` runs in the background and a hangup passes on to it at once.
fn script(r: &Reach, body: &str) -> String {
    let lines: Vec<String> = r.known.iter().map(|l| sh_quote(l)).collect();
    format!(
        "p=; trap '[ -n \"$p\" ] && kill -TERM $p 2>/dev/null; exit 129' HUP; trap '[ -n \"$p\" ] && kill -TERM $p 2>/dev/null; exit 143' TERM; \
         u=$(umask); umask 077; k=$(mktemp) || exit 97; trap 'rm -f \"$k\"' EXIT; \
         printf '%s\\n' {} > \"$k\" || exit 97; umask \"$u\"; {body} & p=$!; wait $p",
        lines.join(" ")
    )
}

fn rsync_cmd(r: &Reach, src: &str, dst: &str, keep_time: bool, mv: bool, follow: bool) -> String {
    // -r -l: folders and links as links; -E: the x bit. No -p: a new file gets its source's mode
    // under the target's umask, a replaced one keeps its own, a merged folder keeps its own.
    let mut flags = String::from(if keep_time { "-rltE" } else { "-rlE" });
    flags.push_str(" -s --no-inc-recursive --info=progress2");
    flags.push_str(&format!(" --partial-dir={PART_DIR}"));
    if mv {
        flags.push_str(" --remove-source-files");
    }
    if follow {
        flags.push_str(" -L");
    }
    let body = format!(
        "env LC_ALL=C rsync {flags} -e \"ssh -p {} {}\" {} {}",
        r.port,
        ssh_opts(),
        sh_quote(src),
        sh_quote(&remote(r, dst))
    );
    sh_line(&script(r, &body))
}

fn scp_cmd(r: &Reach, src: &str, part: &str, keep_time: bool) -> String {
    // -s: the SFTP protocol, so the remote path is taken as it is, not by a shell there.
    let body = format!(
        "env LC_ALL=C scp -s{} -P {} {} {} {}",
        if keep_time { " -p" } else { "" },
        r.port,
        ssh_opts(),
        sh_quote(src),
        sh_quote(&remote(r, part))
    );
    sh_line(&script(r, &body))
}

/// Any login shell (bash, fish, tcsh) hands this to `sh` as one argument.
fn sh_line(script: &str) -> String {
    format!("exec sh -c {}", sh_quote(script))
}

/// The command's output, split at `\r` and `\n`: progress lines and the tail for an error.
#[derive(Default)]
struct Output {
    buf: Vec<u8>,
    tail: std::collections::VecDeque<String>,
}

impl Output {
    /// Feeds bytes; returns the progress lines completed by them.
    fn feed(&mut self, data: &[u8]) -> Vec<(u64, u8)> {
        let mut found = Vec::new();
        for &b in data {
            if b == b'\r' || b == b'\n' {
                let line = String::from_utf8_lossy(&self.buf).trim().to_owned();
                self.buf.clear();
                if line.is_empty() {
                    continue;
                }
                match parse_progress(&line) {
                    Some(p) => found.push(p),
                    None => {
                        if self.tail.len() == TAIL {
                            self.tail.pop_front();
                        }
                        self.tail.push_back(line);
                    }
                }
            } else if self.buf.len() < 4096 {
                self.buf.push(b);
            }
        }
        found
    }

    fn tail(&self) -> Vec<String> {
        let mut t: Vec<String> = self.tail.iter().cloned().collect();
        let rest = String::from_utf8_lossy(&self.buf).trim().to_owned();
        if !rest.is_empty() {
            t.push(rest);
        }
        t
    }
}

/// Bytes and percent of a progress line. rsync: `  1,234,567  12%  10.00MB/s  0:00:05 (xfr#1, …)`;
/// scp: `name  45%  450MB  10.0MB/s  00:05 ETA` (bytes unknown, 0).
fn parse_progress(line: &str) -> Option<(u64, u8)> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let pct_at = words.iter().position(|w| w.len() >= 2 && w.ends_with('%') && w[..w.len() - 1].bytes().all(|b| b.is_ascii_digit()))?;
    let pct: u8 = words[pct_at].trim_end_matches('%').parse().ok().filter(|p| *p <= 100)?;
    let rsync = pct_at == 1 && words[0].bytes().all(|b| b.is_ascii_digit() || b == b',');
    let scp = words.iter().skip(pct_at + 1).any(|w| w.ends_with("/s"));
    if !rsync && !scp {
        return None;
    }
    let bytes = if rsync { words[0].replace(',', "").parse().unwrap_or(0) } else { 0 };
    Some((bytes, pct))
}

/// Done bytes of the item from a progress line: the larger of what moved and the share done.
fn progress((bytes, pct): (u64, u8), size: u64) -> u64 {
    let share = size / 100 * pct as u64 + size % 100 * pct as u64 / 100;
    bytes.max(share).min(size)
}

/// A failed run in the user's words. Rights on the target are EACCES (the queue offers root);
/// everything else asks for the relay.
fn explain(code: u32, tail: &[String], r: &Reach, dst: &str) -> Error {
    let has = |s: &str| tail.iter().any(|l| l.contains(s));
    if has("Permission denied (13)") || has("mkstemp") && has("Permission denied") {
        return Error::Eacces { path: dst.into() };
    }
    let target = format!("{}:{}", r.host, r.port);
    let reason = if has("Host key verification failed") || has("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        format!("ключ хоста {target} не совпал с сохранённым")
    } else if has("Permission denied (publickey") || has("Permission denied, please try again") {
        format!("ключ из агента не принят на {target} для {}", r.user)
    } else if has("Could not resolve hostname") || has("Connection refused") || has("Connection timed out") || has("No route to host") || has("Network is unreachable") {
        format!("сервер-источник не видит {target}")
    } else if has("rsync: not found") || has("rsync: command not found") || code == 127 {
        "rsync не запустился".to_owned()
    } else if code == 97 {
        "на сервере-источнике не создаётся временный файл".to_owned()
    } else {
        tail.iter()
            .rev()
            .find(|l| l.starts_with("rsync:") || l.starts_with("scp:") || l.starts_with("ssh:") || l.contains("error"))
            .or(tail.last())
            .cloned()
            .unwrap_or_else(|| format!("код выхода {code}"))
    };
    failed(reason)
}

/// Removes what interrupted direct runs left on the target: rsync's part folders under the item,
/// or the one next to a single file.
pub async fn clean(s: &Session, root: &str, name: &str, dir: bool) {
    let cmd = if dir {
        format!("find {} -type d -name {PART_DIR} -prune -exec rm -rf -- {{}} + 2>/dev/null; echo kite-probe", sh_quote(root))
    } else {
        let (parent, _) = root.rsplit_once('/').unwrap_or(("", root));
        let pd = format!("{}/{PART_DIR}", if parent.is_empty() { "" } else { parent });
        format!("rm -f -- {} 2>/dev/null; rmdir -- {} 2>/dev/null; echo kite-probe", sh_quote(&format!("{pd}/{name}")), sh_quote(&pd))
    };
    let _ = tokio::time::timeout(PROBE_WAIT, capture(&s.handle, &sh_line(&cmd))).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reach() -> Reach {
        Reach { user: "key".into(), host: "10.0.0.2".into(), port: 2222, known: vec!["[10.0.0.2]:2222 ssh-ed25519 AAAA".into()] }
    }

    #[test]
    fn rsync_progress_lines() {
        assert_eq!(parse_progress("1,234,567  12%   10.00MB/s    0:00:05 (xfr#1, to-chk=0/1)"), Some((1_234_567, 12)));
        assert_eq!(parse_progress("0   0%    0.00kB/s    0:00:00"), Some((0, 0)));
        assert_eq!(parse_progress("big.iso  45%  450MB  10.0MB/s   00:05 ETA"), Some((0, 45)));
        assert_eq!(parse_progress("rsync error: some files could not be transferred (code 23)"), None);
        assert_eq!(parse_progress("100% sure"), None);
    }

    #[test]
    fn output_splits_and_keeps_tail() {
        let mut o = Output::default();
        let p = o.feed(b"    100  1%  1kB/s 0:00:01\r    200  2%  1kB/s 0:00:01\r\nrsync: oops\r\nrsync error: x (code 11)");
        assert_eq!(p, vec![(100, 1), (200, 2)]);
        assert_eq!(o.tail(), vec!["rsync: oops".to_owned(), "rsync error: x (code 11)".to_owned()]);
    }

    #[test]
    fn progress_is_clamped() {
        assert_eq!(progress((50, 10), 1000), 100);
        assert_eq!(progress((900, 10), 1000), 900);
        assert_eq!(progress((5000, 100), 1000), 1000);
    }

    #[test]
    fn errors_are_worded() {
        let r = reach();
        let e = |lines: &[&str], code| match explain(code, &lines.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &r, "/d/x") {
            Error::DirectFailed { reason } => reason,
            Error::Eacces { path } => format!("EACCES {path}"),
            other => format!("{other:?}"),
        };
        assert!(e(&["Host key verification failed."], 255).contains("не совпал"));
        assert!(e(&["key@10.0.0.2: Permission denied (publickey)."], 255).contains("не принят"));
        assert!(e(&["ssh: connect to host 10.0.0.2 port 2222: Connection refused"], 255).contains("не видит"));
        assert_eq!(e(&["rsync: mkstemp \"/d/.x.Ab\" failed: Permission denied (13)"], 23), "EACCES /d/x");
        assert_eq!(e(&[], 5), "код выхода 5");
    }

    #[test]
    fn commands_quote_paths() {
        let r = reach();
        let c = rsync_cmd(&r, "/src/it's here/", "/dst/a b/", true, false, false);
        assert!(c.starts_with("exec sh -c '"));
        // The path is quoted for the inner sh, and that again for the login shell.
        assert!(c.contains(r"it'\''\'\'''\''s here"), "{c}");
        assert!(c.contains("--partial-dir=.kite-rsync-part"));
        assert!(!c.contains("--remove-source-files"));
        assert!(rsync_cmd(&r, "/a", "/b", false, true, false).contains("-rlE -s"));
        let s = scp_cmd(&r, "/a", "/b/.x.kite-part", false);
        assert!(s.contains("scp -s -P 2222"));
        assert!(remote(&Reach { host: "::1".into(), ..reach() }, "/p").starts_with("key@[::1]:"));
    }

    #[test]
    fn hosts_are_checked() {
        assert!(host_ok("srv-1.example.com"));
        assert!(host_ok("fe80::1"));
        assert!(!host_ok("-oProxyCommand=x"));
        assert!(!host_ok("a b"));
        assert!(!host_ok("a;b"));
    }
}
