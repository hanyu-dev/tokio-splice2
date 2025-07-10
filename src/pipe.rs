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

// #[cfg(feature = "feat-pipe-pool")]
// pub(crate) mod pool;

use std::num::NonZeroUsize;
use std::{io, mem};

use rustix::fd::OwnedFd;
use rustix::pipe::{fcntl_getpipe_size, fcntl_setpipe_size, pipe_with, PipeFlags};

#[allow(unsafe_code)]
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
pub const MAXIMUM_PIPE_SIZE: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(1 << 20) };

#[allow(unsafe_code)]
/// `DEFAULT_PIPE_SIZE` is the default pipe size when pipe size is not known.
pub const DEFAULT_PIPE_SIZE: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(1 << 16) };

#[derive(Debug)]
/// Linux Pipe.
pub(crate) struct Pipe {
    /// File descriptor for reading from the pipe
    read_side_fd: Fd,

    /// File descriptor for writing to the pipe
    write_side_fd: Fd,

    /// Pipe size in bytes.
    size: NonZeroUsize,
}

#[derive(Debug)]
enum Fd {
    /// The file descriptor can be used for reading or writing.
    Running(OwnedFd),

    #[allow(dead_code)]
    /// The file descriptor is reserved for future use (to be recycled).
    Reserved(OwnedFd),

    /// Make compiler happy.
    Closed,
}

impl Fd {
    #[inline]
    /// Convert the file descriptor to a pending state.
    ///
    /// This is used to indicate that the file descriptor is reserved for future
    /// use.
    fn set_reserved(&mut self) {
        if let Fd::Running(owned_fd) = mem::replace(self, Fd::Closed) {
            *self = Fd::Reserved(owned_fd);
        }
    }

    #[inline]
    const fn as_fd(&self) -> Option<&OwnedFd> {
        match self {
            Fd::Running(fd) => Some(fd),
            _ => None,
        }
    }

    // #[inline]
    // #[allow(unsafe_code)]
    // /// Safety: the caller must ensure that the operation doesn't care about the
    // /// file descriptor's state.
    // unsafe fn force_as_fd(&self) -> &OwnedFd {
    //     match self {
    //         Fd::Running(fd) => fd,
    //         Fd::Reserved(fd) => fd,
    //         Fd::Closed => {
    //             panic!("Attempted to access a closed file descriptor");
    //         }
    //     }
    // }

    // #[inline]
    // #[allow(unsafe_code)]
    // // Safety: the caller must ensure that the file descriptor is not in use.
    // unsafe fn force_as_mut_fd(&mut self) -> &mut OwnedFd {
    //     match self {
    //         Fd::Running(fd) => fd,
    //         Fd::Reserved(fd) => fd,
    //         Fd::Closed => {
    //             panic!("Attempted to access a closed file descriptor");
    //         }
    //     }
    // }
}

impl Pipe {
    /// Create a pipe, with flags `O_NONBLOCK` and `O_CLOEXEC`.
    ///
    /// The default pipe size is set to `MAXIMUM_PIPE_SIZE` bytes.
    pub(crate) fn new() -> io::Result<Self> {
        pipe_with(PipeFlags::NONBLOCK | PipeFlags::CLOEXEC)
            .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
            .and_then(|(read_fd, write_fd)| {
                // Splice will loop writing MAXIMUM_PIPE_SIZE bytes from the source to the pipe,
                // and then write those bytes from the pipe to the destination.
                // Set the pipe buffer size to MAXIMUM_PIPE_SIZE to optimize that.
                // Ignore errors here, as a smaller buffer size will work,
                // although it will require more system calls.
                let size = match fcntl_setpipe_size(&read_fd, MAXIMUM_PIPE_SIZE.get()) {
                    Ok(size) => NonZeroUsize::new(size),
                    Err(_) => NonZeroUsize::new(fcntl_getpipe_size(&read_fd)?),
                }
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Other,
                        "failed to set pipe size, using default size",
                    )
                })?;

                Ok(Self {
                    read_side_fd: Fd::Running(read_fd),
                    write_side_fd: Fd::Running(write_fd),
                    size,
                })
            })
    }

    /// Set the pipe size.
    ///
    /// For more details, see [`fcntl(2)`].
    ///
    /// [`fcntl(2)`]: https://man7.org/linux/man-pages/man2/fcntl.2.html.
    pub(crate) fn set_pipe_size(&mut self, pipe_size: usize) -> io::Result<usize> {
        let Some(write_side_fd) = self.write_side_fd.as_fd() else {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "write side file descriptor is not available",
            ));
        };

        fcntl_setpipe_size(write_side_fd, pipe_size)
            .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
    }

    #[inline]
    pub(crate) const fn write_side_fd(&self) -> Option<&OwnedFd> {
        self.write_side_fd.as_fd()
    }

    #[must_use]
    #[inline]
    pub(crate) const fn splice_drain_finished(&self) -> bool {
        matches!(self.write_side_fd, Fd::Reserved(_) | Fd::Closed)
    }

    #[inline(always)]
    /// Close the pipe write side file descriptor.
    pub(crate) fn set_splice_drain_finished(&mut self) {
        self.write_side_fd.set_reserved();
    }

    #[inline]
    pub(crate) const fn read_side_fd(&self) -> Option<&OwnedFd> {
        self.read_side_fd.as_fd()
    }

    #[must_use]
    #[inline]
    pub(crate) const fn splice_pump_finished(&self) -> bool {
        matches!(self.read_side_fd, Fd::Reserved(_) | Fd::Closed)
    }

    #[inline]
    /// Close the pipe read side file descriptor.
    pub(crate) fn set_splice_pump_finished(&mut self) {
        self.read_side_fd.set_reserved();
    }

    #[inline]
    /// Returns the size of the pipe, in bytes.
    pub(crate) const fn size(&self) -> NonZeroUsize {
        self.size
    }
}

// #[cfg(feature = "feat-pipe-pool")]
// impl Drop for Pipe {
//     fn drop(&mut self) {
//         #[allow(unsafe_code)]
//         // Safety: the pipe is not in use, so it is safe to return it to the
// pool.         unsafe {
//             pool::PipePool::return_one(self)
//         };
//     }
// }
