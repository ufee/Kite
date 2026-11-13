//! Agent forwarding for a direct server-to-server copy, through a filter.
//!
//! The server opens an `auth-agent@openssh.com` channel whenever a program there talks to the
//! forwarded socket. Such a channel is accepted only while a direct copy holds the connection's
//! [`Forward`] guard, and it reaches the local OpenSSH agent through a filter: listing the keys and
//! signing go through, everything else (adding, removing, locking keys) is refused. So root on the
//! server can log in with the keys for as long as the copy runs, but can never change the agent.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use russh::{
    ChannelOpenFailure,
    client::{ChannelOpenHandle, Msg},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const REQUEST_IDENTITIES: u8 = 11;
const SIGN_REQUEST: u8 = 13;
const FAILURE: u8 = 5;
/// Agent messages are small; a bigger frame is not one.
const FRAME_MAX: usize = 256 * 1024;
/// An agent that asks the user to confirm a signature (`ssh-add -c`) may take a while.
const AGENT_WAIT: Duration = Duration::from_secs(60);

/// How many direct copies on a connection want the agent right now.
#[derive(Clone, Default)]
pub struct Forwards(Arc<AtomicUsize>);

/// Held by a direct copy: agent channels of its connection are let through while it lives.
pub struct Forward(Arc<AtomicUsize>);

impl Forwards {
    pub fn hold(&self) -> Forward {
        self.0.fetch_add(1, Ordering::AcqRel);
        Forward(self.0.clone())
    }

    fn open(&self) -> bool {
        self.0.load(Ordering::Acquire) > 0
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The handler side: accept and bridge, or refuse when no direct copy runs.
pub async fn on_open(fwd: &Forwards, channel: russh::Channel<Msg>, reply: ChannelOpenHandle) {
    if !fwd.open() {
        reply.reject(ChannelOpenFailure::AdministrativelyProhibited).await;
        return;
    }
    reply.accept().await;
    tokio::spawn(async move {
        let mut remote = channel.into_stream();
        let _ = bridge(&mut remote).await;
        let _ = remote.shutdown().await;
    });
}

async fn bridge<S: AsyncRead + AsyncWrite + Unpin>(remote: &mut S) -> std::io::Result<()> {
    // The local agent is opened on the first request that passes, not for a refused one.
    let mut agent: Option<Box<dyn Stream>> = None;
    loop {
        let Some(frame) = read_frame(remote).await? else { return Ok(()) };
        if !allowed(&frame) {
            write_frame(remote, &[FAILURE]).await?;
            continue;
        }
        if agent.is_none() {
            agent = Some(connect().await?);
        }
        let Some(a) = agent.as_mut() else { return Ok(()) };
        let reply = tokio::time::timeout(AGENT_WAIT, async {
            write_frame(a, &frame).await?;
            read_frame(a).await
        })
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "the agent did not answer"))??;
        match reply {
            Some(r) => write_frame(remote, &r).await?,
            None => return Ok(()),
        }
    }
}

/// Only what logging in needs: the list of keys and a signature.
fn allowed(frame: &[u8]) -> bool {
    matches!(frame.first(), Some(&REQUEST_IDENTITIES | &SIGN_REQUEST))
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

async fn connect() -> std::io::Result<Box<dyn Stream>> {
    // All instances of the agent's pipe busy (several signatures at once): wait for a free one.
    #[cfg(windows)]
    let s = {
        const PIPE_BUSY: i32 = 231;
        let mut tries = 0;
        loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(r"\\.\pipe\openssh-ssh-agent") {
                Err(e) if e.raw_os_error() == Some(PIPE_BUSY) && tries < 50 => {
                    tries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                r => break r?,
            }
        }
    };
    #[cfg(unix)]
    let s = tokio::net::UnixStream::connect(std::env::var("SSH_AUTH_SOCK").map_err(std::io::Error::other)?).await?;
    Ok(Box::new(s))
}

/// One message: length, then the body. None at a clean end of the stream.
async fn read_frame<S: AsyncRead + Unpin + ?Sized>(s: &mut S) -> std::io::Result<Option<Vec<u8>>> {
    let mut head = [0u8; 4];
    match s.read_exact(&mut head).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(head) as usize;
    if !(1..=FRAME_MAX).contains(&len) {
        return Err(std::io::Error::other("bad agent frame"));
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).await?;
    Ok(Some(body))
}

async fn write_frame<S: AsyncWrite + Unpin + ?Sized>(s: &mut S, body: &[u8]) -> std::io::Result<()> {
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    s.write_all(&out).await?;
    s.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_list_and_sign_pass() {
        assert!(allowed(&[REQUEST_IDENTITIES]));
        assert!(allowed(&[SIGN_REQUEST, 0, 0]));
        // add, remove, remove all, lock, unlock, add constrained, extension
        for t in [17u8, 18, 19, 22, 23, 25, 27] {
            assert!(!allowed(&[t]), "{t}");
        }
        assert!(!allowed(&[]));
    }

    #[tokio::test]
    async fn refused_requests_get_failure_without_an_agent() {
        // A refused request is answered here; the agent is never opened for it.
        let (mut a, mut b) = tokio::io::duplex(1024);
        let t = tokio::spawn(async move { bridge(&mut b).await });
        write_frame(&mut a, &[19]).await.unwrap();
        assert_eq!(read_frame(&mut a).await.unwrap(), Some(vec![FAILURE]));
        drop(a);
        t.await.unwrap().unwrap();
    }
}
