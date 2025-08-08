//! Pipe pool to reuse pipes.

use core::{mem, sync::atomic::AtomicUsize};
use std::collections::VecDeque;
use std::io;

use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::pipe::{Fd, Pipe};

static mut PIPE_CACHE: Vec<VecDeque<Pipe>> = Vec::new();

static PIPE_CACHE_SLOT: AtomicUsize = AtomicUsize::new(0);

const INITIAL_PIPE_POOL_SIZE: usize = 8;

#[derive(Debug)]
/// The pipe pool.
pub(crate) struct PipePool {
    inner: Mutex<VecDeque<Pipe>>,

    /// Optional sender to notify the background task to recycle pipes.
    recycle_tx: Option<mpsc::Sender<Pipe>>,

    /// Optional sender to notify the background task to fill the pipe pool.
    refill_tx: Option<mpsc::Sender<usize>>,
}

impl PipePool {
    const fn new() -> Self {
        PipePool {
            inner: Mutex::new(VecDeque::new()),
            recycle_tx: None,
            refill_tx: None,
        }
    }

    /// Start the background task to recycle and refill pipes.
    pub(crate) async fn background_task(&mut self) {
        let (tx, mut rx) = mpsc::channel(256);

        self.recycle_tx = Some(tx);

        tokio::spawn(async move {
            while let Some(pipe) = rx.recv().await {
                if rustix::io::read(
                    #[allow(unsafe_code)]
                    // Safety: we can read from the pipe as long as it is not in use.
                    unsafe {
                        &pipe.read_side_fd.force_as_fd()
                    },
                    &mut [0; 1],
                )
                .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock)
                {
                    // Pipe is empty, return to pool

                    PIPE_CACHE.with(|i| i.inner.lock().push_front(pipe));

                    continue;
                }

                if Self::refill_one().is_ok() {
                    // continue;
                } else {
                    // Error, spawn a task to refill the pipe pool
                    tokio::spawn(async {
                        let mut retry = 0;

                        while retry < 3 {
                            use std::time::Duration;

                            use tokio::time::sleep;

                            if Self::refill_one().is_ok() {
                                return;
                            }

                            retry += 1;

                            sleep(Duration::from_millis(100)).await;
                        }

                        crate::error!(
                            "Failed to refill pipe pool after 3 retries, last os error: {:?}",
                            io::Error::last_os_error()
                        );
                    });
                }
            }
        });

        let (tx, mut rx) = mpsc::channel(256);

        self.refill_tx = Some(tx);

        tokio::spawn(async move {
            while let Some(_) = rx.recv().await {
                if let Err(e) = Self::refill_one() {
                    crate::error!("Failed to refill pipe pool: {e:?}",);
                }
            }
        });
    }

    fn refill_one() -> io::Result<()> {
        // Before refilling the pipe pool, check if the cache is too large.
        // TODO: dynamic pool size?
        {
            let mut cache = PIPE_CACHE.inner.lock();

            if cache.len() >= 512 {
                cache.truncate(256);
                return Ok(());
            }
        }

        // Refill the pipe pool with a new pipe.
        {
            Pipe::new().map(|pipe| {
                PIPE_CACHE.inner.lock().push_front(pipe);
            })
        }
    }

    fn notice_recycle(&self, pipe: Pipe) {
        if let Some(tx) = &self.recycle_tx {
            // Notify the background task to recycle the pipe
            let _ = tx.blocking_send(pipe);
        }
    }

    fn notice_refill(&self) {
        if let Some(tx) = &self.refill_tx {
            let _ = tx.blocking_send(2);
        }
    }

    /// Get a pipe from the pool, or create a new one if the pool is empty.
    pub(crate) fn take_one(&self) -> io::Result<Pipe> {
        match {
            let mut pipes = self.inner.lock();
            pipes.pop_back()
        } {
            Some(pipe) => Ok(pipe),
            None => {
                self.notice_refill();
                Pipe::new()
            }
        }
    }

    #[allow(unsafe_code)]
    /// Return a pipe to the pool.
    ///
    /// Safety: the caller must ensure that the pipe is not used after this
    /// call.
    pub(super) unsafe fn return_one(pipe: &mut Pipe) {
        PIPE_CACHE.notice_recycle(Pipe {
            read_side_fd: Fd::Running(mem::replace(pipe.read_side_fd.force_as_mut_fd(), unsafe {
                mem::zeroed()
            })),
            write_side_fd: Fd::Running(mem::replace(
                pipe.write_side_fd.force_as_mut_fd(),
                unsafe { mem::zeroed() },
            )),
            size: pipe.size,
        });
    }
}
