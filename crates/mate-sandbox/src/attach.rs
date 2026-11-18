//! Interactive attach: hand the host terminal to Claude Code running inside
//! the container's tmux session, and take it back on detach.
//!
//! The caller (the TUI) must stop reading stdin and leave the alternate
//! screen before calling [`run`], and restore both afterwards. Raw mode stays
//! on throughout.

use uuid::Uuid;

use crate::claude::{SessionArg, choose_session_arg};
use crate::docker::DockerApi;
use crate::error::SandboxError;
use crate::headless::{interactive_alive, transcript_exists};
use crate::session::SandboxSession;
use crate::spec::CLAUDE_CONFIG_DIR_ENV;

/// Key that detaches while attached: `Ctrl-\`.
pub const DETACH_BYTE: u8 = 0x1c;

/// How an attach ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachOutcome {
    /// The tmux session is still alive: Claude Code keeps running.
    Detached,
    /// Claude Code exited.
    Ended { exit_code: Option<i64> },
}

/// Split a chunk of stdin at the first detach byte. Returns the bytes to
/// forward and whether a detach was requested. Bytes after the detach byte
/// are dropped.
pub fn split_detach(buf: &[u8]) -> (&[u8], bool) {
    match buf.iter().position(|b| *b == DETACH_BYTE) {
        Some(i) => (&buf[..i], true),
        None => (buf, false),
    }
}

/// Decide the Claude session argument and record it on the session. The
/// caller must persist the store **before** calling [`run`], so a fresh id
/// survives a crash.
pub async fn prepare_attach(
    docker: &dyn DockerApi,
    session: &mut SandboxSession,
) -> Result<SessionArg, SandboxError> {
    let inspect = crate::docker::verify_ownership(docker, &session.container_id, &session.id).await?;
    let stored = session.claude_session_id;
    // The transcript probe matters only when tmux will start Claude Code;
    // if tmux is alive, `-A` ignores the command and the argument is unused.
    let exists = match stored {
        Some(id) if !interactive_alive(docker, session).await? => {
            let prefix = format!("{CLAUDE_CONFIG_DIR_ENV}=");
            let dir = inspect
                .config
                .as_ref()
                .and_then(|c| c.env.as_ref())
                .and_then(|env| env.iter().find_map(|e| e.strip_prefix(&prefix)));
            match dir {
                Some(dir) => transcript_exists(docker, session, id, dir).await?,
                None => false,
            }
        }
        Some(_) => true,
        None => false,
    };
    let arg = choose_session_arg(stored, exists, Uuid::new_v4);
    session.claude_session_id = Some(arg.id());
    Ok(arg)
}

#[cfg(unix)]
pub use unix::{NonBlockingFd, install_panic_hook, run};

#[cfg(not(unix))]
pub async fn run(
    _docker: &dyn DockerApi,
    _session: &SandboxSession,
    _arg: SessionArg,
) -> Result<AttachOutcome, SandboxError> {
    Err(SandboxError::Unsupported(
        "interactive attach needs a Unix terminal".to_owned(),
    ))
}

#[cfg(unix)]
mod unix {
    use std::io::{self, Write};
    use std::os::fd::{AsRawFd, RawFd};
    use std::sync::Once;
    use std::sync::atomic::{AtomicI32, Ordering};

    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;
    use tokio::io::unix::AsyncFd;
    use tokio::signal::unix::{SignalKind, signal};

    use bollard::container::LogOutput;

    use super::{AttachOutcome, split_detach};
    use crate::claude::{SessionArg, interactive_command, tmux_detach_command};
    use crate::docker::{DockerApi, ExecSpec, create_exec_owned, exec_collect};
    use crate::error::SandboxError;
    use crate::headless::interactive_alive;
    use crate::session::SandboxSession;

    const STDIN_FD: RawFd = 0;
    const STDOUT_FD: RawFd = 1;

    /// Flags to restore if we panic while stdin is non-blocking; -1 = none.
    static SAVED_FLAGS: AtomicI32 = AtomicI32::new(-1);
    static SAVED_FD: AtomicI32 = AtomicI32::new(-1);
    static HOOK: Once = Once::new();

    /// Chain a panic hook that restores stdin's flags before the previous
    /// hook (ratatui's terminal restore) runs. Installed once per process;
    /// inert when no [`NonBlockingFd`] is alive.
    pub fn install_panic_hook() {
        HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let flags = SAVED_FLAGS.load(Ordering::SeqCst);
                if flags >= 0 {
                    // SAFETY: fcntl on a descriptor number with a flags value
                    // previously read from that same descriptor.
                    unsafe {
                        libc::fcntl(SAVED_FD.load(Ordering::SeqCst), libc::F_SETFL, flags);
                    }
                }
                previous(info);
            }));
        });
    }

    /// Sets `O_NONBLOCK` on a descriptor and restores the original flags on
    /// drop. A non-blocking stdin left behind breaks the user's shell.
    pub struct NonBlockingFd {
        fd: RawFd,
        original: libc::c_int,
    }

    impl NonBlockingFd {
        pub fn new(fd: RawFd) -> io::Result<Self> {
            // SAFETY: fcntl F_GETFL/F_SETFL take only integers and do not
            // touch memory; an invalid fd is reported through the return value.
            let original = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if original < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: as above.
            if unsafe { libc::fcntl(fd, libc::F_SETFL, original | libc::O_NONBLOCK) } < 0 {
                return Err(io::Error::last_os_error());
            }
            SAVED_FD.store(fd, Ordering::SeqCst);
            SAVED_FLAGS.store(original, Ordering::SeqCst);
            Ok(Self { fd, original })
        }
    }

    impl Drop for NonBlockingFd {
        fn drop(&mut self) {
            // SAFETY: restores flags previously read from this descriptor.
            unsafe {
                libc::fcntl(self.fd, libc::F_SETFL, self.original);
            }
            SAVED_FLAGS.store(-1, Ordering::SeqCst);
        }
    }

    struct Fd(RawFd);

    impl AsRawFd for Fd {
        fn as_raw_fd(&self) -> RawFd {
            self.0
        }
    }

    /// Terminal size as (columns, rows), falling back to 80x24.
    fn terminal_size() -> (u16, u16) {
        let mut ws = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCGWINSZ writes one `winsize` into the pointed-to struct,
        // which lives for the call.
        let rc = unsafe { libc::ioctl(STDOUT_FD, libc::TIOCGWINSZ, &mut ws) };
        if rc == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            (ws.ws_col, ws.ws_row)
        } else {
            (80, 24)
        }
    }

    async fn read_stdin(afd: &AsyncFd<Fd>, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut guard = afd.readable().await?;
            let result = guard.try_io(|inner| {
                // SAFETY: `buf` is valid for `buf.len()` writable bytes for
                // the duration of the call.
                let n = unsafe {
                    libc::read(inner.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match result {
                Ok(r) => return r,
                Err(_would_block) => continue,
            }
        }
    }

    fn io_err(e: io::Error) -> SandboxError {
        SandboxError::Store(format!("terminal i/o: {e}"))
    }

    /// Attach the host terminal to the sandbox's tmux session until it ends
    /// or the user presses `Ctrl-\`.
    pub async fn run(
        docker: &dyn DockerApi,
        session: &SandboxSession,
        arg: SessionArg,
    ) -> Result<AttachOutcome, SandboxError> {
        install_panic_hook();
        let exec = create_exec_owned(
            docker,
            &session.container_id,
            &session.id,
            ExecSpec {
                cmd: interactive_command(arg),
                tty: true,
                attach_stdin: true,
                env: vec!["TERM=xterm-256color".to_owned()],
            },
        )
        .await?;
        let mut stream = docker.start_exec(&exec, true).await?;
        let (cols, rows) = terminal_size();
        docker.resize_exec(&exec, cols, rows).await?;

        let guard = NonBlockingFd::new(STDIN_FD).map_err(io_err)?;
        let afd = AsyncFd::new(Fd(STDIN_FD)).map_err(io_err)?;
        let mut winch = signal(SignalKind::window_change()).map_err(io_err)?;
        let mut stdout = io::stdout();
        let mut buf = [0u8; 4096];
        let mut stdin_open = true;

        loop {
            tokio::select! {
                chunk = stream.output.next() => match chunk {
                    Some(Ok(LogOutput::StdOut { message } | LogOutput::Console { message })) => {
                        stdout.write_all(&message).map_err(io_err)?;
                        stdout.flush().map_err(io_err)?;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e),
                    None => break,
                },
                read = read_stdin(&afd, &mut buf), if stdin_open => {
                    let n = read.map_err(io_err)?;
                    if n == 0 {
                        stdin_open = false;
                        continue;
                    }
                    let (forward, detach) = split_detach(&buf[..n]);
                    if !forward.is_empty() {
                        stream.input.write_all(forward).await.map_err(io_err)?;
                        stream.input.flush().await.map_err(io_err)?;
                    }
                    if detach {
                        // Ends the attach exec; keep pumping until the stream closes.
                        exec_collect(
                            docker,
                            &session.container_id,
                            &session.id,
                            tmux_detach_command(),
                        )
                        .await?;
                    }
                }
                _ = winch.recv() => {
                    let (cols, rows) = terminal_size();
                    docker.resize_exec(&exec, cols, rows).await?;
                }
            }
        }
        drop(afd);
        drop(guard);

        let exit_code = docker.inspect_exec(&exec).await?.exit_code;
        if interactive_alive(docker, session).await? {
            Ok(AttachOutcome::Detached)
        } else {
            Ok(AttachOutcome::Ended { exit_code })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn flags(fd: RawFd) -> libc::c_int {
            // SAFETY: F_GETFL on a descriptor owned by the test.
            unsafe { libc::fcntl(fd, libc::F_GETFL) }
        }

        #[test]
        fn guard_sets_and_restores_nonblock() {
            let mut fds = [0 as libc::c_int; 2];
            // SAFETY: `fds` has room for the two descriptors pipe writes.
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe created");
            let (r, w) = (fds[0], fds[1]);
            let before = flags(r);
            assert_eq!(before & libc::O_NONBLOCK, 0, "pipe starts blocking");
            {
                let _guard = NonBlockingFd::new(r).unwrap();
                assert_ne!(flags(r) & libc::O_NONBLOCK, 0, "guard sets O_NONBLOCK");
            }
            assert_eq!(flags(r), before, "dropping the guard restores the original flags");
            // SAFETY: closing descriptors this test opened.
            unsafe {
                libc::close(r);
                libc::close(w);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDocker, labelled_inspect};
    use crate::session::tests::sample;

    #[test]
    fn splitter_cases() {
        assert_eq!(split_detach(b"\x1cabc"), (&b""[..], true), "detach byte at start");
        assert_eq!(split_detach(b"ab\x1ccd"), (&b"ab"[..], true), "bytes before a mid-chunk detach are forwarded, the rest dropped");
        assert_eq!(split_detach(b"abc\x1c"), (&b"abc"[..], true), "detach byte at end");
        assert_eq!(split_detach(b"abc"), (&b"abc"[..], false), "no detach byte forwards everything");
        assert_eq!(split_detach(b""), (&b""[..], false), "empty chunk is a no-op");
    }

    fn fake_for(s: &SandboxSession) -> FakeDocker {
        let fake = FakeDocker::default();
        let mut i = labelled_inspect(
            &[("managed-by", "mate"), ("session-id", &s.id.to_string())],
            "running",
        );
        i.config.as_mut().unwrap().env = Some(vec!["CLAUDE_CONFIG_DIR=/c".into()]);
        fake.add(&s.container_id, i);
        fake
    }

    #[tokio::test]
    async fn fresh_session_gets_new_id_stored() {
        let mut s = sample();
        let fake = fake_for(&s);
        let arg = prepare_attach(&fake, &mut s).await.unwrap();
        assert!(matches!(arg, SessionArg::New(_)), "no stored id starts a new conversation");
        assert_eq!(s.claude_session_id, Some(arg.id()), "the generated id is recorded for persisting");
    }

    #[tokio::test]
    async fn stored_id_with_transcript_resumes() {
        let mut s = sample();
        let id = Uuid::from_u128(9);
        s.claude_session_id = Some(id);
        let fake = fake_for(&s);
        fake.script(&format!("test -f /c/projects/-workspace/{id}.jsonl"), "", "", 0);
        assert_eq!(
            prepare_attach(&fake, &mut s).await.unwrap(),
            SessionArg::Resume(id),
            "stored id with a transcript resumes"
        );
    }

    #[tokio::test]
    async fn stored_id_without_transcript_restarts_same_id() {
        let mut s = sample();
        let id = Uuid::from_u128(9);
        s.claude_session_id = Some(id);
        let fake = fake_for(&s);
        assert_eq!(
            prepare_attach(&fake, &mut s).await.unwrap(),
            SessionArg::New(id),
            "no transcript (unscripted test -f exits 1) reuses the id with --session-id"
        );
    }
}
