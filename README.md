# phantom

`phantom` is a Rust + LLVM text editor prototype targeting very fast desktop editing on macOS and Windows.

This milestone separates normal inline editing from a large-file engine. Small UTF-8 files open as regular editable buffers. Files above the inline threshold open in a viewport mode that reads only a small window of the file, avoiding full-file `String` allocation and keeping startup responsive for 100GB-class files.

The current inline editor accepts UTF-8 files up to 16 MiB. Larger files open asynchronously in large-file viewport mode with a 1 MiB editable window plus a sparse whole-file line index. The editor renders the full file line count as virtual rows, then loads the byte window needed for the visible rows on demand. The sparse index keeps periodic line checkpoints instead of every line offset, and is built by streaming fixed-size chunks instead of memory-mapping the entire file. The active window keeps a local chunked line index, and only allocates a piece table after the first viewport edit, so opening a large file does not duplicate the viewport text for editing state. Rows that cross viewport boundaries are kept read-only to avoid saving accidental partial-line edits. Saving a changed viewport runs in the background and streams the original file into a replacement file while substituting only the visible byte range.

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
- Preserve UTF-8 BOM and detected line-ending style for inline documents.
- Open large files asynchronously without loading the whole file into memory.
- Build sparse large-file line indexes with bounded streaming buffers instead of whole-file mmap scans.
- Scroll across every indexed line in a large file instead of stopping at the first viewport.
- Horizontally scroll long editor lines, with an optional wrap mode from the View menu.
- Search with match-case, whole-word, and regular-expression options; Replace All is available for inline documents.
- Open Find, Go to Line, Help, file open/save, wrap, and zoom actions from keyboard shortcuts.
- Jump to a requested line in inline documents or large-file viewports.
- Open files by dragging and dropping them into the editor window.
- Zoom the editor font while keeping virtual row measurement in sync.
- Edit and save the visible window of a large file with piece-table edits.
- Allocate large-file piece tables lazily, only after the first edit in the current viewport.
- Render large-file viewports with virtual rows backed by a chunked line index.
- Move between large-file windows in a background worker.
- Save large viewport edits in a background worker with stale-file conflict checks.
- Save the current document with a native save dialog and temporary file replacement flow.
- Show line, character, byte, and save-state metrics.
- Block new/open actions while the current document has unsaved changes.

## miu Desktop Feature Gap

Compared with `kenjinote/miu`, excluding Android, iOS, iPadOS, and Linux targets, phantom now covers native desktop open/save dialogs, drag-and-drop open, dark editor styling, large-file viewport editing, regex/match-case/whole-word search, inline Replace All, Go to Line, F1-style keyboard help, line wrapping, horizontal scrolling, and editor zoom.

The remaining larger desktop gaps are multi-cursor editing, rectangular selection and paste, explicit line move/copy/delete commands, case conversion commands, syntax highlighting, bracket/word occurrence highlighting, overwrite mode, IME composition visualization, full custom clipboard semantics, and a first-class application packaging/install story for Windows and macOS. These require replacing more of the current egui `TextEdit` editing surface with a custom text interaction layer, so they are tracked as follow-up work rather than small incremental additions.

## Next Milestones

- Incremental background line indexing for faster first paint on 100GB-class files.
- SIMD-accelerated search beyond newline counting.
- Custom text interaction layer for multi-cursor, rectangular selection, line operations, and richer selection rendering.
- Syntax highlighting and bracket/word occurrence highlighting.
- Multi-viewport editing semantics for inserts and deletes across large files.
- Platform-specific packaging for macOS and Windows.