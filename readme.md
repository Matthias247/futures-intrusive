futures-intrusive
=================

This crate provides a variety of futures-aware synchronization primitives
that are based on the idea of intrusive collections.

Please refer to the [documentation](https://docs.rs/futures-intrusive/) for details.

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
futures-intrusive = "0.1.0"
```

In order to use the crate in a `no-std` environment, it needs to be compiled
without default features:

```toml
[dependencies]
futures-intrusive = { version = "0.1.0", default-features = false }
```

The crate defines a feature `std`, which can be used in order to re-enable `std` features.

## Minimum Rust version

Due to the usage of unstable features, the library depends on a nightly version
of the Rust compiler.
The current minimum required Rust version is 1.36 nightly 2019-05-21.

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.