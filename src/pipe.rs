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

use std::{fmt, io};

use rustix::fd::OwnedFd;
use rustix::pipe::{fcntl_setpipe_size, pipe_with, PipeFlags};

/// Linux Pipe.
pub struct Pipe {
    /// File descriptor for reading from the pipe
    read_fd: Option<OwnedFd>,

    /// File descriptor for writing to the pipe
    write_fd: Option<OwnedFd>,
}

impl fmt::Debug for Pipe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pipe")
            .field(
                "read_fd",
                match self.read_fd {
                    Some(ref fd) => fd,
                    None => &"(closed)",
                },
            )
            .field(
                "write_fd",
                match self.write_fd {
                    Some(ref fd) => fd,
                    None => &"(closed)",
                },
            )
            .finish()
    }
}

impl Pipe {
    /// Create a pipe, with flags `O_NONBLOCK` and `O_CLOEXEC`.
    pub(crate) fn new() -> io::Result<Self> {
        pipe_with(PipeFlags::NONBLOCK | PipeFlags::CLOEXEC)
            .map(|(read_fd, write_fd)| {
                let _act = fcntl_setpipe_size(&write_fd, 4096 * 64);
                // println!("Set pipe size to: {:?}", act);
                Self {
                    read_fd: Some(read_fd),
                    write_fd: Some(write_fd),
                }
            })
            .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
    }

    /// Customize the pipe size.
    ///
    /// For more details, see [`fcntl(2)`].
    ///
    /// [`fcntl(2)`]: https://man7.org/linux/man-pages/man2/fcntl.2.html.
    pub(crate) fn set_pipe_size(&mut self, pipe_size: usize) -> io::Result<usize> {
        if let Some(write_fd) = self.write_fd.as_ref() {
            fcntl_setpipe_size(write_fd, pipe_size)
                .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
        } else {
            Err(io::Error::new(
                io::ErrorKind::Other,
                "Cannot set pipe size on a closed write end",
            ))
        }
    }

    #[inline(always)]
    /// Get the pipe read side file descriptor.
    ///
    /// This method returns an `Option<&OwnedFd>`, which is `Some` if the pipe
    /// read side file descriptor is available, or `None` if it has been
    /// closed.
    pub(crate) const fn read_fd(&self) -> Option<&OwnedFd> {
        self.read_fd.as_ref()
    }

    #[inline(always)]
    /// Close the pipe read side file descriptor.
    pub(crate) fn close_read_fd(&mut self) {
        self.read_fd.take();
    }

    #[inline(always)]
    /// Get the pipe write side file descriptor.
    ///
    /// This method returns an `Option<&OwnedFd>`, which is `Some` if the pipe
    /// write side file descriptor is available, or `None` if it has been
    /// closed.
    pub(crate) const fn write_fd(&self) -> Option<&OwnedFd> {
        self.write_fd.as_ref()
    }

    #[inline(always)]
    /// Close the pipe write side file descriptor.
    pub(crate) fn close_write_fd(&mut self) {
        self.write_fd.take();
    }
}
