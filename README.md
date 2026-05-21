# phantom

`phantom` is a Rust + LLVM text editor prototype targeting very fast desktop editing on macOS and Windows.

This first milestone is intentionally small: it provides a native desktop window, a monospace text editing surface, basic open/save by file path, live document metrics, memory-mapped UTF-8 loading, SIMD-accelerated newline scanning through `memchr`, safer save replacement, and dirty-state protection. The editing state is split into a testable Rust library so future work can add viewport-based editing for extremely large files without coupling that logic to the GUI.

## Requirements

- Rust stable toolchain
- macOS or Windows desktop environment

Rust compiles through LLVM, so the current prototype already uses the Rust + LLVM toolchain path while keeping the application code portable.

## Run

```sh
cargo run
```

## Test

```sh
cargo test
```

## Current Capabilities

- Create a new empty document.
- Edit text in a native desktop window.
- Open a UTF-8 text file with a native file selection dialog and memory-mapped file access.
- Save the current document with a native save dialog and temporary file replacement flow.
- Show line, character, byte, and save-state metrics.
- Block new/open actions while the current document has unsaved changes.

## Next Milestones

- Viewport-based editing for 100GB-class files beyond the current inline editing limit.
- SIMD-accelerated line indexing and search beyond newline counting.
- Incremental rendering for large buffers.
- Platform-specific packaging for macOS and Windows.