# Gojo documentation

Gojo is a small, fast-compiling systems programming language with no garbage collector and no nil. Ownership, without lifetime annotations, frees memory and rules out data races; errors are values; concurrency is structured.

## Learn

- [A tour of Gojo](tour.md): the language by example, from hello world to scopes and channels. Every program in it is compiled and run by the test suite.

## Reference

- [Language reference](spec.md): the rules the compiler implements, section by section, with the tests that cover each one.
- [Standard library](std/README.md): every module of `std`, generated from its source by `gojo doc`.

## Design

- [Design notes](design.md): why Gojo looks the way it does, what it takes from Rust, Zig, Swift, Kotlin and Go, and what it leaves out.
- [Concurrency](concurrency.md): how goroutines became structured concurrency: scopes, cancellation, channels and `select`.
- [Packages](packages.md): the proposal for packages, versions and a registry, written with coding agents in mind.
- [WebAssembly](wasm.md): the proposal for compiling to Wasm: targets, tasks on one thread, WASI and the browser, with measurements from a prototype.
