//! Linux Pipe

use std::io;

use rustix::fd::OwnedFd;
use rustix::pipe::{fcntl_setpipe_size, pipe_with, PipeFlags};

/// the size of `PIPE_BUF`
const PIPE_SIZE: usize = 65536;

/// Linux Pipe
#[repr(C)]
pub(crate) struct Pipe {
    /// File descriptor for reading from the pipe
    read_fd: OwnedFd,

    /// File descriptor for writing to the pipe
    write_fd: OwnedFd,
}

impl Pipe {
    /// Create a pipe, with flags `O_NONBLOCK` and `O_CLOEXEC`, and the write
    /// side fd size is set to `PIPE_SIZE` (current: 65536).
    ///
    /// See [`pipe_with`] for more details.
    pub(crate) fn new() -> io::Result<Self> {
        pipe_with(PipeFlags::NONBLOCK | PipeFlags::CLOEXEC)
            .and_then(|(read_fd, write_fd)| {
                fcntl_setpipe_size(&write_fd, PIPE_SIZE)?;

                Ok(Self { read_fd, write_fd })
            })
            .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
    }

    #[inline(always)]
    /// Get the read file descriptor.
    pub(crate) const fn read_fd(&self) -> &OwnedFd {
        &self.read_fd
    }

    #[inline(always)]
    /// Get the write file descriptor.
    pub(crate) const fn write_fd(&self) -> &OwnedFd {
        &self.write_fd
    }
}
