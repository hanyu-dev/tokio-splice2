# **tokio-splice2**  - The [`splice(2)`] syscall API wrapper, async Rust ready.

[![Crates.io Version](https://img.shields.io/crates/v/tokio-splice2)](https://crates.io/crates/tokio-splice2)
[![GitHub Tag](https://img.shields.io/github/v/tag/hanyu-dev/tokio-splice2)](https://github.com/hanyu-dev/tokio-splice2/tags)
[![Documentation](https://img.shields.io/docsrs/tokio-splice2)](https://docs.rs/tokio-splice2)
[![CI Check](https://img.shields.io/github/actions/workflow/status/hanyu-dev/tokio-splice2/rust.yml?branch=master)](https://github.com/hanyu-dev/tokio-splice2/actions?query=branch%master)
[![License](https://img.shields.io/crates/l/tokio-splice2)](https://github.com/hanyu-dev/tokio-splice2/blob/master/LICENSE)

Implemented [`splice(2)`] based unidirectional/bidirectional data transmission, just like [`tokio::io::copy_bidirectional`](https://docs.rs/tokio/latest/tokio/io/fn.copy_bidirectional.html).

## Examples

See [examples/proxy.rs](./examples/proxy.rs), with a [Go implementation](./examples/proxy.go) for comparison.

## Changelogs

- TODO:

  - Reuse pipe.

- 0.3.0:

  - MSRV is now 1.70.0.
  - Replace `libc` with `rustix`.
  - Add `tracing` logger support.
  - Add unidirectional copy.
  - Returns `TrafficResult` instead of `io::Result<T>` to have traffic statistics returned when error occurs.
  - (Experimental) Add `tokio::fs::File` support to splice from (like `sendfile`) / to (not fully tested).
  - (Experimental) Traffic rate limitation support.

## MSRV

1.70.0

## Benchmark

While no formal benchmarks have been conducted, `iperf3` testing results indicate that the throughput is comparable to [tokio-splice] and _slightly_ outperforms the Go implementation.

## Limitations

- When splicing data from a file to a pipe and then splicing from the pipe
  to a socket for transmission, the data is referenced from the page cache
  corresponding to the file. If the original file is modified while the
  splice operation is in progress (i.e., the data is still in the kernel
  buffer and has not been fully sent to the network), there may be a
  situation where the transmitted data is the old data (before
  modification). Because there is no clear mechanism to know when the data
  has truly "left" the kernel and been sent to the network, thus safely
  allowing the file to be modified. Linus Torvalds once commented that this
  is the "key point" of splice design, which shares references to data pages
  and behaves similarly to `mmap()`. This is a complex issue concerning data
  consistency and concurrent access.

  See [lwn.net/Articles/923237] and [rust#116451].

  This crate requires passing `&mut R` to prevent modification elsewhere
  before the `Future` of `splice(2)` I/O completes. However, this is just
  best-effort guarantee.

- In certain cases, such as transferring small chunks of data, frequently
  calling splice, or when the underlying driver/hardware does not support
  efficient zero-copy, the performance improvement may not meet
  expectations. It could even be lower than an optimized read/write loop due
  to additional system call overhead. The choice of pipe buffer size may
  also affect performance.

- A successful [`splice(2)`] call returns the number of bytes transferred,
  but this ONLY indicates that the data has entered the kernel buffer of the
  destination file descriptor (such as the send buffer of a socket). It does
  not mean the data has actually left the local network interface or been
  received by the peer.

  We call `flush` / `poll_flush` after bytes data is spliced from pipe to
  the target fd to ensure that it has been flushed to the destination.
  However, poor implementation of `tokio::io::AsyncWrite`
  may break this, as they may not flush the data immediately.

- For UDP _zero-copy_ I/O, `splice(2)` does not help. Linux kernel actually
  pays less attention optimizing UDP performance.

  Consider using `sendmmsg` and `recvmmsg` instead, which is a more efficient
  way to send/receive multiple UDP packets in a single system call. eBPF XDP
  is also a good choice for high-performance stateless UDP packet forwarding.


## LICENSE

MIT OR Apache-2.0

## Credits

[tokio-splice]

[`splice(2)`]: https://man7.org/linux/man-pages/man2/splice.2.html
[lwn.net/Articles/923237]: https://lwn.net/Articles/923237/
[rust#116451]: https://github.com/rust-lang/rust/issues/116451
[tokio-splice]: https://github.com/Hanaasagi/tokio-splice
