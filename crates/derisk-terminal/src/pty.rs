//! The shell's pty, and the two threads that move bytes through it.
//!
//! alacritty_terminal opens the pty and starts the shell as its session
//! leader. Reading and writing are this crate's: a reader thread hands what
//! the shell prints to the window's thread over a bounded channel (when the
//! window falls behind, the reader waits, so the shell does too, rather than
//! output piling up in memory), and a writer thread sends keys back. Both
//! are scoped to [`crate::run`], and both also watch a stop eventfd, so
//! closing the window ends them even while the shell is silent or not
//! reading its input.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsFd, OwnedFd},
    path::PathBuf,
};

use alacritty_terminal::{
    event::WindowSize,
    tty::{self, Options, Shell},
};
use crossbeam_channel::{Receiver, Sender};
use rustix::event::{EventfdFlags, PollFd, PollFlags};

/// The shell, or the command given with `-e`, on a new pty.
pub fn spawn(
    command: Option<Vec<String>>,
    directory: Option<PathBuf>,
    size: WindowSize,
) -> io::Result<tty::Pty> {
    let env = HashMap::from(
        [
            // xterm-256color is in every terminfo database, which a
            // terminal-specific name would not be on a machine this one ssh's
            // into; what it describes is what alacritty_terminal emulates.
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("TERM_PROGRAM", "derisk-terminal"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned())),
    );
    let shell = command.filter(|argv| !argv.is_empty()).map(|mut argv| {
        let program = argv.remove(0);
        Shell::new(program, argv)
    });
    let options = Options {
        shell,
        working_directory: directory,
        drain_on_exit: false,
        env,
    };
    tty::new(&options, size, 0)
}

/// The eventfd that tells both threads to stop.
pub struct Stop(OwnedFd);

impl Stop {
    pub fn new() -> io::Result<Self> {
        Ok(Self(rustix::event::eventfd(
            0,
            EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK,
        )?))
    }

    /// Wakes both threads for good: the counter never goes back to zero.
    pub fn signal(&self) {
        let _ = rustix::io::write(&self.0, &1u64.to_ne_bytes());
    }
}

/// Waits until `fd` is ready for `flags`. `false` once stopped.
fn wait(fd: &impl AsFd, flags: PollFlags, stop: &Stop) -> bool {
    loop {
        let mut fds = [PollFd::new(fd, flags), PollFd::new(&stop.0, PollFlags::IN)];
        match rustix::event::poll(&mut fds, None) {
            Ok(_) if !fds[1].revents().is_empty() => return false,
            Ok(_) => return true,
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return false,
        }
    }
}

/// Copies the shell's output to `output` until the shell is gone, the
/// window's end of the channel is, or `stop` is signalled; `wake` is called
/// after each chunk so the window's event loop looks at the channel. The pty
/// is non-blocking (alacritty_terminal sets it so), hence the poll.
pub fn read(mut pty: &File, stop: &Stop, output: Sender<Vec<u8>>, wake: impl Fn()) {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        if !wait(&pty, PollFlags::IN, stop) {
            return;
        }
        match pty.read(&mut buffer) {
            Ok(0) => return,
            Ok(n) => {
                if output.send(buffer[..n].to_vec()).is_err() {
                    return;
                }
                wake();
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            // EIO: every process holding the pty's other end has exited.
            Err(_) => return,
        }
    }
}

/// Writes what the window sends until it hangs up or `stop` is signalled.
pub fn write(mut pty: &File, stop: &Stop, input: Receiver<Vec<u8>>) {
    for bytes in input {
        let mut rest = &bytes[..];
        while !rest.is_empty() {
            match pty.write(rest) {
                Ok(n) => rest = &rest[n..],
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // The shell is not reading; wait for room, or the end.
                    if !wait(&pty, PollFlags::OUT, stop) {
                        return;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }
}

/// Tells the pty, and so the shell, how big the grid is.
pub fn resize(pty: &File, size: WindowSize) {
    let winsize = rustix::termios::Winsize {
        ws_row: size.num_lines,
        ws_col: size.num_cols,
        ws_xpixel: size.num_cols.saturating_mul(size.cell_width),
        ws_ypixel: size.num_lines.saturating_mul(size.cell_height),
    };
    if let Err(error) = rustix::termios::tcsetwinsize(pty, winsize) {
        tracing::warn!("resizing the pty: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use super::*;

    fn size() -> WindowSize {
        WindowSize {
            num_lines: 24,
            num_cols: 80,
            cell_width: 8,
            cell_height: 16,
        }
    }

    #[test]
    fn runs_a_command_and_reads_its_output() {
        let pty = spawn(
            Some(vec![
                "sh".into(),
                "-c".into(),
                "printf hello; stty size".into(),
            ]),
            None,
            size(),
        )
        .expect("a pty");
        let stop = Stop::new().unwrap();
        let (tx, rx) = crossbeam_channel::bounded(4);
        let mut out = Vec::new();
        thread::scope(|s| {
            s.spawn(|| read(pty.file(), &stop, tx, || {}));
            while let Ok(chunk) = rx.recv_timeout(Duration::from_secs(10)) {
                out.extend(chunk);
            }
        });
        let out = String::from_utf8_lossy(&out);
        assert!(out.contains("hello"), "{out:?}");
        assert!(out.contains("24 80"), "{out:?}");
    }

    #[test]
    fn stop_ends_both_threads_while_the_shell_is_silent() {
        let pty = spawn(Some(vec!["sleep".into(), "30".into()]), None, size()).expect("a pty");
        let stop = Stop::new().unwrap();
        let (tx, _rx) = crossbeam_channel::bounded(4);
        let (in_tx, in_rx) = crossbeam_channel::bounded::<Vec<u8>>(4);
        thread::scope(|s| {
            s.spawn(|| read(pty.file(), &stop, tx, || {}));
            s.spawn(|| write(pty.file(), &stop, in_rx));
            stop.signal();
            drop(in_tx);
        });
    }
}
