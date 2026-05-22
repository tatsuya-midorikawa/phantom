# phantom

`phantom` is a Rust + LLVM text editor prototype targeting very fast desktop editing on macOS and Windows.

This milestone separates normal inline editing from a large-file engine. Small UTF-8 files open as regular editable buffers. Files above the inline threshold open in a viewport mode that reads only a small window of the file, avoiding full-file `String` allocation and keeping startup responsive for 100GB-class files.

The current inline editor accepts UTF-8 files up to 16 MiB. Larger files open asynchronously in large-file viewport mode with a 1 MiB editable window plus a sparse whole-file line index. The editor renders the full file line count as virtual rows, then loads the byte window needed for the visible rows on demand. The sparse index keeps periodic line checkpoints instead of every line offset, and scans from the nearest checkpoint when jumping to a visible row. The active window is backed by a piece table and a local chunked line index, so the UI does not hand the whole file to one text widget. Rows that cross viewport boundaries are kept read-only to avoid saving accidental partial-line edits. Saving a changed viewport runs in the background and streams the original file into a replacement file while substituting only the visible byte range.

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
- Open a UTF-8 text file with a native file selection dialog.
- Open large files asynchronously without loading the whole file into memory.
- Scroll across every indexed line in a large file instead of stopping at the first viewport.
- Horizontally scroll long editor lines, with an optional wrap mode from the View menu.
- Edit and save the visible window of a large file with piece-table edits.
- Render large-file viewports with virtual rows backed by a chunked line index.
- Move between large-file windows in a background worker.
- Save large viewport edits in a background worker with stale-file conflict checks.
- Save the current document with a native save dialog and temporary file replacement flow.
- Show line, character, byte, and save-state metrics.
- Block new/open actions while the current document has unsaved changes.

## Next Milestones

- Incremental background line indexing for faster first paint on 100GB-class files.
- SIMD-accelerated search beyond newline counting.
- Multi-viewport editing semantics for inserts and deletes across large files.
- Platform-specific packaging for macOS and Windows.