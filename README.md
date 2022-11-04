# μchan
Small, scalable, unbounded, mpsc channel.

This is (almost) a drop-in replacement for `std::sync::mpsc` with a focus on being lock-free and scalable for both producers and consumers.
It also supports being used as `#![no_std]`, in which the caller provides a trait used to block and unblock a thread, with the queue implementing everything else from there. Finally, `benchmark/` contains (you guessed it) robust benchmarks against other channel implementations/

## Usage

```toml
[dependencies]
uchan = "0.1.0"
```

## License

uchan is licensed under MIT (http://opensource.org/licenses/MIT)