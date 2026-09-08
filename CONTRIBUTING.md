# Contributing to AeroExecute-Engine

## Development Setup

### Prerequisites
- Rust 1.78+ (`rustup install stable`)
- C++20 compiler (`gcc-13` or `clang-17`)

### Building
```bash
cargo build --release
cargo test
```

### Code Standards
- All unsafe blocks must have a safety comment
- Benchmark any change to critical path with `cargo bench`
- Lock-free structures must pass Miri (`cargo miri test`)

## Pull Request Process
1. Create a feature branch from `main`
2. Write tests for new behaviour
3. Ensure `cargo clippy` passes with zero warnings
4. Open a PR with a clear description of the performance delta
