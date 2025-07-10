//! Some utils.

use std::io;
use std::num::NonZeroUsize;

#[derive(Debug)]
pub(crate) enum Offset {
    None,
    /// Read offset set.
    In(Option<u64>),
    /// Write offset set.
    Out(Option<u64>),
}

impl Offset {
    #[inline]
    pub(crate) fn off_in(&mut self) -> Option<&mut u64> {
        match self {
            Offset::In(off) => off.as_mut(),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn off_out(&mut self) -> Option<&mut u64> {
        match self {
            Offset::Out(off) => off.as_mut(),
            _ => None,
        }
    }

    pub(crate) fn calc_size_to_splice(
        f_len: u64,
        f_offset_start: Option<u64>,
        f_offset_end: Option<u64>,
    ) -> io::Result<u64> {
        match (f_offset_start, f_offset_end) {
            (Some(start), Some(end)) => {
                if start > end || end > f_len {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid offset range",
                    ));
                }
                Ok(end - start)
            }
            (Some(start), None) => {
                if start > f_len {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid offset start",
                    ));
                }
                Ok(f_len - start)
            }
            (None, Some(end)) => {
                if end > f_len {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid offset end",
                    ));
                }
                Ok(end)
            }
            (None, None) => Ok(f_len),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Drained {
    /// New data has been read from `r` into pipe.
    Some(NonZeroUsize),

    /// Indicates that the draining process is complete and no more data can be
    /// read from the `r` into pipe.
    Done,
}
