# tokio-splice2

[![crates.io](https://img.shields.io/crates/v/tokio-splice2)](https://crates.io/crates/tokio-splice2)
[![docs.rs](https://img.shields.io/docsrs/tokio-splice2)](https://docs.rs/crate/tokio-splice2/latest)

Implemented `splice(2)` based bidirectional data transmission in tokio-rs.
Just like [`tokio::io::copy_bidirectional`](https://docs.rs/tokio/latest/tokio/io/fn.copy_bidirectional.html).

## Examples

See [examples](./examples/).

## Changelog

- 0.3.0:
  - [BREAKING] use `rustix` instead of `libc`
  - [BREAKING] Do not shutdown stream after transfer finished (let user controls).
  - [BREAKING] MSRV is changed to 1.70.0
  - Add unidirectional splicing.
  - Accept `tokio::fs::File` as input.

- 0.2.1:
  - Fix the maximum value of the `size_t` type. Closes: [https://github.com/Hanaasagi/tokio-splice/issues/2](https://github.com/Hanaasagi/tokio-splice/issues/2).

## Benchmark

See [BENCHMARK](./BENCHMARK.md).

## MSRV

1.70.0

## LICENSE

MIT OR Apache-2.0

## Credits

[tokio-splice](https://github.com/Hanaasagi/tokio-splice)
