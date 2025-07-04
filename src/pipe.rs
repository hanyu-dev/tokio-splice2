//! Linux Pipe - [`pipe(7)`].
//!
//! The default pipe capacity depends on the system default, but it is
//! usually `65536` for those whose page size is `4096`. See [`pipe(7)`]
//! for more details.
//!
//! Customize the pipe size is supported, see [`fcntl(2)`] for details and
//! notices.
//!
//! [`pipe(7)`]: https://man7.org/linux/man-pages/man7/pipe.7.html
//! [`fcntl(2)`]: https://man7.org/linux/man-pages/man2/fcntl.2.html

// FIXME: Pipe pool?

use std::{io, mem};

use rustix::fd::OwnedFd;
use rustix::pipe::{fcntl_setpipe_size, pipe_with, PipeFlags};

/// `MAXIMUM_PIPE_SIZE` is the maximum amount of data we asks
/// the kernel to move in a single call to `splice(2)`.
///
/// We use 1MB as `splice(2)` writes data through a pipe, and 1MB is the default
/// maximum pipe buffer size, which is determined by
/// `/proc/sys/fs/pipe-max-size`.
///
/// Running applications under unprivileged user may have the pages usage
/// limited. See [`pipe(7)`] for details.
///
/// [`pipe(7)`]: https://man7.org/linux/man-pages/man7/pipe.7.html
pub const MAXIMUM_PIPE_SIZE: usize = 1 << 20;

#[derive(Debug)]
/// Linux Pipe.
pub struct Pipe {
    /// File descriptor for reading from the pipe
    read_side_fd: Fd,

    /// File descriptor for writing to the pipe
    write_side_fd: Fd,
}

#[derive(Debug)]
enum Fd {
    Running(OwnedFd),
    Closed,
}

impl Pipe {
    /// Create a pipe, with flags `O_NONBLOCK` and `O_CLOEXEC`.
    ///
    /// The default pipe size is set to `65536` bytes.
    pub(crate) fn new() -> io::Result<Self> {
        // Create a pipe with `O_CLOEXEC` flags.
        pipe_with(PipeFlags::NONBLOCK | PipeFlags::CLOEXEC)
            .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
            .map(|(read_fd, write_fd)| {
                // Splice will loop writing MAXIMUM_PIPE_SIZE bytes from the source to the pipe,
                // and then write those bytes from the pipe to the destination.
                // Set the pipe buffer size to MAXIMUM_PIPE_SIZE to optimize that.
                // Ignore errors here, as a smaller buffer size will work,
                // although it will require more system calls.
                let _ = fcntl_setpipe_size(&read_fd, MAXIMUM_PIPE_SIZE);

                Self {
                    read_side_fd: Fd::Running(read_fd),
                    write_side_fd: Fd::Running(write_fd),
                }
            })
    }

    /// Customize the pipe size.
    ///
    /// For more details, see [`fcntl(2)`].
    ///
    /// [`fcntl(2)`]: https://man7.org/linux/man-pages/man2/fcntl.2.html.
    pub(crate) fn set_pipe_size(&mut self, pipe_size: usize) -> io::Result<usize> {
        if let Fd::Running(read_side_fd) = &self.read_side_fd {
            fcntl_setpipe_size(read_side_fd, pipe_size)
                .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "pipe closed"))
        }
    }

    #[inline]
    pub(super) const fn write_side_fd(&self) -> Option<&OwnedFd> {
        match &self.write_side_fd {
            Fd::Running(fd) => Some(fd),
            Fd::Closed => None,
        }
    }

    #[must_use]
    #[inline]
    pub(crate) const fn splice_drain_finished(&self) -> bool {
        matches!(self.write_side_fd, Fd::Closed)
    }

    #[inline(always)]
    /// Close the pipe write side file descriptor.
    pub(crate) fn set_splice_drain_finished(&mut self) {
        let _ = mem::replace(&mut self.write_side_fd, Fd::Closed);
    }

    #[inline]
    pub(super) const fn read_side_fd(&self) -> Option<&OwnedFd> {
        match &self.read_side_fd {
            Fd::Running(fd) => Some(fd),
            Fd::Closed => None,
        }
    }

    #[must_use]
    #[inline]
    pub(crate) const fn splice_pump_finished(&self) -> bool {
        matches!(self.read_side_fd, Fd::Closed)
    }

    #[inline(always)]
    /// Close the pipe read side file descriptor.
    pub(crate) fn set_splice_pump_finished(&mut self) {
        let _ = mem::replace(&mut self.read_side_fd, Fd::Closed);
    }
}
