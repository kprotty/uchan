# μchan
Small, scalable, unbounded, mpsc channel.

[![Cargo](https://img.shields.io/crates/v/uchan.svg)](
https://crates.io/crates/uchan)
[![Documentation](https://docs.rs/uchan/badge.svg)](
https://docs.rs/uchan)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](
https://github.com/kprotty/uchan)

This is (almost) a drop-in replacement for `std::sync::mpsc` with a focus on being lock-free and scalable for both producers and consumers.
It also supports being used as `#![no_std]`, in which the caller provides a trait used to block and unblock a thread, with the queue implementing everything else from there.

## Usage

```toml
[dependencies]
uchan = "0.1.4"
```

## Benchmarking

```bash
cd benchmark
cargo run --release
```

For adding custom channels to the benchmark, see `benchmark/src/queues.rs`.

## License

uchan is licensed under MIT (http://opensource.org/licenses/MIT)