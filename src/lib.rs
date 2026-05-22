use memchr::memchr_iter;
use memmap2::MmapOptions;
use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub mod search;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

const BYTES_PER_MIB: u64 = 1024 * 1024;

pub const DEFAULT_MAX_INLINE_EDIT_BYTES: u64 = 16 * BYTES_PER_MIB;
pub const DEFAULT_LARGE_VIEW_BYTES: usize = 1024 * 1024;
const LINE_INDEX_CHUNK_LINES: usize = 1024;
const LINE_START_SCAN_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TextMetrics {
    pub bytes: usize,
    pub characters: usize,
    pub visual_lines: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TextEncoding {
    Utf8,
    Utf8Bom,
}

impl TextEncoding {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            TextEncoding::Utf8 => "UTF-8",
            TextEncoding::Utf8Bom => "UTF-8 BOM",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LineEnding {
    Lf,
    Crlf,
    Cr,
}

impl LineEnding {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
            LineEnding::Cr => "\r",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::Crlf => "CRLF",
            LineEnding::Cr => "CR",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FileProfile {
    pub path: PathBuf,
    pub bytes: u64,
    pub visual_lines: usize,
    pub is_utf8: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ReplacementGuard {
    SafeToReplace,
    BlockedByUnsavedChanges,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DocumentOpenMode {
    Inline,
    Large,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum OpenedDocument {
    Inline(EditorDocument),
    Large(Box<LargeDocument>),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct EditorDocument {
    text: String,
    path: Option<PathBuf>,
    dirty: bool,
    metrics: TextMetrics,
    encoding: TextEncoding,
    line_ending: LineEnding,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PieceSource {
    Original,
    Add,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct TextPiece {
    source: PieceSource,
    start: usize,
    len: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PieceTable {
    original: String,
    add: String,
    pieces: Vec<TextPiece>,
}

impl PieceTable {
    #[must_use]
    pub fn from_original(original: String) -> Self {
        let pieces = if original.is_empty() {
            Vec::new()
        } else {
            vec![TextPiece {
                source: PieceSource::Original,
                start: 0,
                len: original.len(),
            }]
        };

        Self {
            original,
            add: String::new(),
            pieces,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.pieces.iter().map(|piece| piece.len).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    #[must_use]
    pub fn text(&self) -> String {
        let mut text = String::with_capacity(self.len());

        for piece in &self.pieces {
            text.push_str(self.piece_text(piece));
        }

        text
    }

    pub fn replace_range(&mut self, range: Range<usize>, replacement: &str) -> io::Result<()> {
        self.validate_replace_range(range.clone())?;

        if range.is_empty() && replacement.is_empty() {
            return Ok(());
        }

        let replacement_piece = self.append_replacement(replacement);
        let old_pieces = self.pieces.clone();
        let mut next_pieces =
            Vec::with_capacity(old_pieces.len() + usize::from(replacement_piece.is_some()));
        let mut inserted_replacement = false;
        let mut document_offset = 0;

        for piece in old_pieces {
            let piece_start = document_offset;
            let piece_end = piece_start + piece.len;

            if piece_end <= range.start {
                push_piece(&mut next_pieces, piece);
            } else if piece_start >= range.end {
                insert_replacement_once(
                    &mut next_pieces,
                    replacement_piece,
                    &mut inserted_replacement,
                );
                push_piece(&mut next_pieces, piece);
            } else {
                if range.start > piece_start {
                    push_piece(
                        &mut next_pieces,
                        TextPiece {
                            source: piece.source,
                            start: piece.start,
                            len: range.start - piece_start,
                        },
                    );
                }

                insert_replacement_once(
                    &mut next_pieces,
                    replacement_piece,
                    &mut inserted_replacement,
                );

                if range.end < piece_end {
                    let suffix_offset = range.end - piece_start;
                    push_piece(
                        &mut next_pieces,
                        TextPiece {
                            source: piece.source,
                            start: piece.start + suffix_offset,
                            len: piece_end - range.end,
                        },
                    );
                }
            }

            document_offset = piece_end;
        }

        insert_replacement_once(
            &mut next_pieces,
            replacement_piece,
            &mut inserted_replacement,
        );

        self.pieces = next_pieces;
        Ok(())
    }

    fn piece_text(&self, piece: &TextPiece) -> &str {
        let source = match piece.source {
            PieceSource::Original => &self.original,
            PieceSource::Add => &self.add,
        };

        &source[piece.start..piece.start + piece.len]
    }

    fn append_replacement(&mut self, replacement: &str) -> Option<TextPiece> {
        if replacement.is_empty() {
            return None;
        }

        let start = self.add.len();
        self.add.push_str(replacement);

        Some(TextPiece {
            source: PieceSource::Add,
            start,
            len: replacement.len(),
        })
    }

    fn validate_replace_range(&self, range: Range<usize>) -> io::Result<()> {
        let len = self.len();

        if range.start > range.end || range.end > len {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "replacement range is outside the current viewport",
            ));
        }

        let text = self.text();
        if text.is_char_boundary(range.start) && text.is_char_boundary(range.end) {
            Ok(())
        } else {
            Err(io::Error::new(
                ErrorKind::InvalidInput,
                "replacement range must align to UTF-8 boundaries",
            ))
        }
    }
}

fn insert_replacement_once(
    pieces: &mut Vec<TextPiece>,
    replacement: Option<TextPiece>,
    inserted: &mut bool,
) {
    if *inserted {
        return;
    }

    if let Some(piece) = replacement {
        push_piece(pieces, piece);
    }

    *inserted = true;
}

fn push_piece(pieces: &mut Vec<TextPiece>, piece: TextPiece) {
    if piece.len == 0 {
        return;
    }

    if let Some(last_piece) = pieces.last_mut() {
        let can_merge =
            last_piece.source == piece.source && last_piece.start + last_piece.len == piece.start;

        if can_merge {
            last_piece.len += piece.len;
            return;
        }
    }

    pieces.push(piece);
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ChunkedLineIndex {
    chunks: Vec<LineIndexChunk>,
    line_count: usize,
    bytes: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct LineIndexChunk {
    first_line: usize,
    starts: Vec<usize>,
}

impl ChunkedLineIndex {
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self::from_text_with_trailing_empty_line(text, true)
    }

    #[must_use]
    fn from_text_with_trailing_empty_line(text: &str, include_trailing_empty_line: bool) -> Self {
        let mut starts = Vec::with_capacity(count_visual_lines(text));
        starts.push(0);
        starts.extend(
            memchr_iter(b'\n', text.as_bytes())
                .map(|newline_index| newline_index + 1)
                .filter(|line_start| include_trailing_empty_line || *line_start < text.len()),
        );

        let chunks = starts
            .chunks(LINE_INDEX_CHUNK_LINES)
            .enumerate()
            .map(|(chunk_index, starts)| LineIndexChunk {
                first_line: chunk_index * LINE_INDEX_CHUNK_LINES,
                starts: starts.to_vec(),
            })
            .collect::<Vec<_>>();

        Self {
            chunks,
            line_count: starts.len(),
            bytes: text.len(),
        }
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.line_count
    }

    #[must_use]
    pub fn line_start(&self, line_index: usize) -> Option<usize> {
        let chunk = self.chunks.get(line_index / LINE_INDEX_CHUNK_LINES)?;
        let start_index = line_index.checked_sub(chunk.first_line)?;

        chunk.starts.get(start_index).copied()
    }

    #[must_use]
    pub fn line_text_range(&self, line_index: usize, text: &str) -> Option<Range<usize>> {
        if text.len() != self.bytes || line_index >= self.line_count {
            return None;
        }

        let start = self.line_start(line_index)?;
        let mut end = if line_index + 1 < self.line_count {
            self.line_start(line_index + 1)?
        } else {
            self.bytes
        };
        let bytes = text.as_bytes();

        if end > start && bytes[end - 1] == b'\n' {
            end -= 1;
            if end > start && bytes[end - 1] == b'\r' {
                end -= 1;
            }
        }

        Some(start..end)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LargeFileLineIndex {
    checkpoints: Arc<[FileLineIndexCheckpoint]>,
    line_count: usize,
    bytes: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct FileLineIndexCheckpoint {
    line_index: usize,
    byte_offset: u64,
}

impl LargeFileLineIndex {
    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::open(path)?;
        let bytes = file.metadata()?.len();
        let mapped_file = if bytes == 0 {
            None
        } else {
            Some(map_file(&file)?)
        };

        Ok(Self::from_bytes(
            bytes,
            mapped_file.as_deref().unwrap_or_default(),
        ))
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.line_count
    }

    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn line_start_byte(
        &self,
        path: impl AsRef<Path>,
        line_index: usize,
    ) -> io::Result<Option<u64>> {
        if line_index >= self.line_count {
            return Ok(None);
        }

        let checkpoint = self
            .checkpoint_for_line(line_index)
            .expect("line index should always contain the first checkpoint");

        scan_line_start_from_checkpoint(path.as_ref(), checkpoint, line_index)
    }

    pub fn line_end_byte(
        &self,
        path: impl AsRef<Path>,
        line_index: usize,
    ) -> io::Result<Option<u64>> {
        if line_index >= self.line_count {
            return Ok(None);
        }

        if line_index + 1 < self.line_count {
            self.line_start_byte(path, line_index + 1)
        } else {
            Ok(Some(self.bytes))
        }
    }

    pub fn line_at_or_before(&self, path: impl AsRef<Path>, byte_offset: u64) -> io::Result<usize> {
        let byte_offset = byte_offset.min(self.bytes);
        let checkpoint = self
            .checkpoint_for_byte(byte_offset)
            .expect("line index should always contain the first checkpoint");

        scan_line_at_or_before_from_checkpoint(path.as_ref(), checkpoint, byte_offset)
    }

    fn from_bytes(bytes: u64, data: &[u8]) -> Self {
        let mut builder = LargeFileLineIndexBuilder::new(bytes);

        for newline_index in memchr_iter(b'\n', data) {
            builder.push_line_start(newline_index as u64 + 1);
        }

        builder.finish()
    }

    fn checkpoint_for_line(&self, line_index: usize) -> Option<&FileLineIndexCheckpoint> {
        self.checkpoints.get(line_index / LINE_INDEX_CHUNK_LINES)
    }

    fn checkpoint_for_byte(&self, byte_offset: u64) -> Option<&FileLineIndexCheckpoint> {
        let checkpoint_index = self
            .checkpoints
            .partition_point(|checkpoint| checkpoint.byte_offset <= byte_offset)
            .saturating_sub(1);

        self.checkpoints.get(checkpoint_index)
    }
}

struct LargeFileLineIndexBuilder {
    checkpoints: Vec<FileLineIndexCheckpoint>,
    line_count: usize,
    bytes: u64,
}

impl LargeFileLineIndexBuilder {
    fn new(bytes: u64) -> Self {
        Self {
            checkpoints: vec![FileLineIndexCheckpoint {
                line_index: 0,
                byte_offset: 0,
            }],
            line_count: 1,
            bytes,
        }
    }

    fn push_line_start(&mut self, start: u64) {
        if self.line_count % LINE_INDEX_CHUNK_LINES == 0 {
            self.checkpoints.push(FileLineIndexCheckpoint {
                line_index: self.line_count,
                byte_offset: start,
            });
        }

        self.line_count += 1;
    }

    fn finish(self) -> LargeFileLineIndex {
        LargeFileLineIndex {
            checkpoints: self.checkpoints.into(),
            line_count: self.line_count,
            bytes: self.bytes,
        }
    }
}

fn scan_line_start_from_checkpoint(
    path: &Path,
    checkpoint: &FileLineIndexCheckpoint,
    target_line: usize,
) -> io::Result<Option<u64>> {
    if target_line == checkpoint.line_index {
        return Ok(Some(checkpoint.byte_offset));
    }

    let mut file = File::open(path)?;
    let mut buffer = vec![0; LINE_START_SCAN_BUFFER_BYTES];
    let mut current_line = checkpoint.line_index;
    let mut absolute_offset = checkpoint.byte_offset;

    file.seek(SeekFrom::Start(checkpoint.byte_offset))?;

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            return Ok(None);
        }

        for newline_index in memchr_iter(b'\n', &buffer[..bytes_read]) {
            current_line += 1;
            let line_start = absolute_offset + newline_index as u64 + 1;

            if current_line == target_line {
                return Ok(Some(line_start));
            }
        }

        absolute_offset += bytes_read as u64;
    }
}

fn scan_line_at_or_before_from_checkpoint(
    path: &Path,
    checkpoint: &FileLineIndexCheckpoint,
    byte_offset: u64,
) -> io::Result<usize> {
    if byte_offset <= checkpoint.byte_offset {
        return Ok(checkpoint.line_index);
    }

    let mut file = File::open(path)?;
    let mut buffer = vec![0; LINE_START_SCAN_BUFFER_BYTES];
    let mut current_line = checkpoint.line_index;
    let mut absolute_offset = checkpoint.byte_offset;

    file.seek(SeekFrom::Start(checkpoint.byte_offset))?;

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            return Ok(current_line);
        }

        for newline_index in memchr_iter(b'\n', &buffer[..bytes_read]) {
            let next_line_start = absolute_offset + newline_index as u64 + 1;

            if next_line_start <= byte_offset {
                current_line += 1;
            } else {
                return Ok(current_line);
            }
        }

        absolute_offset += bytes_read as u64;

        if absolute_offset > byte_offset {
            return Ok(current_line);
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LargeViewport {
    start_byte: u64,
    end_byte: u64,
    text: String,
    piece_table: PieceTable,
    line_index: ChunkedLineIndex,
    leading_partial_line: bool,
    trailing_partial_line: bool,
    include_trailing_empty_line: bool,
}

impl LargeViewport {
    #[must_use]
    fn from_text(
        start_byte: u64,
        end_byte: u64,
        text: String,
        leading_partial_line: bool,
        trailing_partial_line: bool,
        include_trailing_empty_line: bool,
    ) -> Self {
        Self {
            start_byte,
            end_byte,
            piece_table: PieceTable::from_original(text.clone()),
            line_index: ChunkedLineIndex::from_text_with_trailing_empty_line(
                &text,
                include_trailing_empty_line,
            ),
            leading_partial_line,
            trailing_partial_line,
            include_trailing_empty_line,
            text,
        }
    }

    #[must_use]
    pub fn start_byte(&self) -> u64 {
        self.start_byte
    }

    #[must_use]
    pub fn end_byte(&self) -> u64 {
        self.end_byte
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.line_index.line_count()
    }

    #[must_use]
    pub fn leading_partial_line(&self) -> bool {
        self.leading_partial_line
    }

    #[must_use]
    pub fn trailing_partial_line(&self) -> bool {
        self.trailing_partial_line
    }

    #[must_use]
    pub fn is_line_editable(&self, line_index: usize) -> bool {
        line_index < self.line_count()
            && !(self.leading_partial_line && line_index == 0)
            && !(self.trailing_partial_line && line_index + 1 == self.line_count())
    }

    #[must_use]
    pub fn line_text(&self, line_index: usize) -> Option<&str> {
        let range = self.line_index.line_text_range(line_index, &self.text)?;

        self.text.get(range)
    }

    pub fn replace_line(&mut self, line_index: usize, replacement: &str) -> io::Result<bool> {
        if !self.is_line_editable(line_index) {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "line crosses the viewport boundary; move the viewport before editing it",
            ));
        }

        let range = self
            .line_index
            .line_text_range(line_index, &self.text)
            .ok_or_else(|| {
                io::Error::new(ErrorKind::InvalidInput, "line is outside the viewport")
            })?;

        if &self.text[range.clone()] == replacement {
            return Ok(false);
        }

        self.piece_table.replace_range(range, replacement)?;
        self.rebuild_text_and_index();

        Ok(true)
    }

    #[cfg(test)]
    fn replace_text(&mut self, text: String) -> bool {
        if self.text == text {
            return false;
        }

        self.piece_table = PieceTable::from_original(text.clone());
        self.line_index = ChunkedLineIndex::from_text(&text);
        self.text = text;
        self.leading_partial_line = false;
        self.trailing_partial_line = false;
        self.include_trailing_empty_line = true;

        true
    }

    fn rebuild_text_and_index(&mut self) {
        self.text = self.piece_table.text();
        self.line_index = ChunkedLineIndex::from_text_with_trailing_empty_line(
            &self.text,
            self.include_trailing_empty_line,
        );
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LargeDocument {
    path: PathBuf,
    bytes: u64,
    fingerprint: FileFingerprint,
    viewport_bytes: usize,
    viewport: LargeViewport,
    line_index: LargeFileLineIndex,
    viewport_start_line: usize,
    dirty: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct FileFingerprint {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl LargeDocument {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    #[must_use]
    pub fn viewport(&self) -> &LargeViewport {
        &self.viewport
    }

    #[must_use]
    pub fn viewport_text(&self) -> &str {
        self.viewport.text()
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn record_text_change(&mut self) {
        self.dirty = true;
    }

    #[must_use]
    pub fn viewport_metrics(&self) -> TextMetrics {
        calculate_text_metrics(self.viewport_text())
    }

    #[must_use]
    pub fn viewport_line_count(&self) -> usize {
        self.viewport.line_count()
    }

    #[must_use]
    pub fn file_line_count(&self) -> usize {
        self.line_index.line_count()
    }

    #[must_use]
    pub fn viewport_start_line(&self) -> usize {
        self.viewport_start_line
    }

    #[must_use]
    pub fn viewport_end_line(&self) -> usize {
        (self.viewport_start_line + self.viewport_line_count()).min(self.file_line_count())
    }

    #[must_use]
    pub fn contains_file_line_range(&self, line_range: Range<usize>) -> bool {
        line_range.start >= self.viewport_start_line && line_range.end <= self.viewport_end_line()
    }

    #[must_use]
    pub fn contains_file_line(&self, line_index: usize) -> bool {
        line_index >= self.viewport_start_line && line_index < self.viewport_end_line()
    }

    #[must_use]
    pub fn viewport_line_text(&self, line_index: usize) -> Option<&str> {
        self.viewport.line_text(line_index)
    }

    #[must_use]
    pub fn is_viewport_line_editable(&self, line_index: usize) -> bool {
        self.viewport.is_line_editable(line_index)
    }

    pub fn replace_viewport_line(
        &mut self,
        line_index: usize,
        replacement: &str,
    ) -> io::Result<bool> {
        let changed = self.viewport.replace_line(line_index, replacement)?;

        if changed {
            self.record_text_change();
        }

        Ok(changed)
    }

    #[must_use]
    pub fn file_line_text(&self, line_index: usize) -> Option<&str> {
        let viewport_line = line_index.checked_sub(self.viewport_start_line)?;

        self.viewport_line_text(viewport_line)
    }

    #[must_use]
    pub fn is_file_line_editable(&self, line_index: usize) -> bool {
        let Some(viewport_line) = line_index.checked_sub(self.viewport_start_line) else {
            return false;
        };

        self.is_viewport_line_editable(viewport_line)
    }

    pub fn replace_file_line(&mut self, line_index: usize, replacement: &str) -> io::Result<bool> {
        let viewport_line = line_index
            .checked_sub(self.viewport_start_line)
            .ok_or_else(|| {
                io::Error::new(
                    ErrorKind::InvalidInput,
                    "line is outside the loaded viewport",
                )
            })?;

        self.replace_viewport_line(viewport_line, replacement)
    }

    #[cfg(test)]
    fn replace_viewport_text(&mut self, text: String) {
        if self.viewport.replace_text(text) {
            self.record_text_change();
        }
    }

    #[must_use]
    pub fn display_name(&self) -> String {
        self.path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .filter(|file_name| !file_name.is_empty())
            .unwrap_or("Untitled")
            .to_owned()
    }

    #[must_use]
    pub fn window_title(&self) -> String {
        let dirty_marker = if self.dirty { "*" } else { "" };

        format!("{dirty_marker}{} - phantom", self.display_name())
    }

    pub fn load_viewport_at(&mut self, start_byte: u64) -> io::Result<()> {
        if self.dirty {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "save the current viewport before moving",
            ));
        }

        ensure_file_fingerprint_matches(&self.path, &self.fingerprint)?;
        let viewport = read_large_viewport(&self.path, start_byte, self.viewport_bytes)?;
        ensure_file_fingerprint_matches(&self.path, &self.fingerprint)?;

        let viewport_start_line = self
            .line_index
            .line_at_or_before(&self.path, viewport.start_byte())?;

        self.viewport = viewport;
        self.viewport_start_line = viewport_start_line;
        Ok(())
    }

    pub fn load_viewport_for_line(&mut self, line_index: usize) -> io::Result<()> {
        if self.dirty {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "save the current viewport before moving",
            ));
        }

        let clamped_line = line_index.min(self.file_line_count().saturating_sub(1));
        let start_byte = self
            .line_index
            .line_start_byte(&self.path, clamped_line)?
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "line is outside the file"))?;

        self.load_viewport_at(start_byte)
    }

    pub fn load_next_viewport(&mut self) -> io::Result<()> {
        let next_start = self.viewport.end_byte.min(self.bytes);
        self.load_viewport_at(next_start)
    }

    pub fn load_previous_viewport(&mut self) -> io::Result<()> {
        let previous_start = self
            .viewport
            .start_byte
            .saturating_sub(self.viewport_bytes as u64);
        self.load_viewport_at(previous_start)
    }

    fn mark_saved_as(&mut self, path: PathBuf) -> io::Result<()> {
        self.path = path;
        self.fingerprint = file_fingerprint(&self.path)?;
        self.bytes = self.fingerprint.len;
        self.line_index = LargeFileLineIndex::from_path(&self.path)?;
        self.viewport = read_large_viewport(
            &self.path,
            self.viewport.start_byte.min(self.bytes),
            self.viewport_bytes,
        )?;
        self.viewport_start_line = self
            .line_index
            .line_at_or_before(&self.path, self.viewport.start_byte())?;
        self.dirty = false;
        Ok(())
    }
}

impl Default for EditorDocument {
    fn default() -> Self {
        Self::untitled()
    }
}

impl EditorDocument {
    #[must_use]
    pub fn untitled() -> Self {
        Self {
            text: String::new(),
            path: None,
            dirty: false,
            metrics: calculate_text_metrics(""),
            encoding: TextEncoding::Utf8,
            line_ending: LineEnding::Lf,
        }
    }

    #[must_use]
    pub fn from_saved_text(path: PathBuf, text: String) -> Self {
        Self::from_saved_text_with_format(path, text, TextEncoding::Utf8)
    }

    #[must_use]
    pub fn from_saved_text_with_format(
        path: PathBuf,
        text: String,
        encoding: TextEncoding,
    ) -> Self {
        let metrics = calculate_text_metrics(&text);
        let line_ending = detect_line_ending(&text).unwrap_or(LineEnding::Lf);

        Self {
            text,
            path: Some(path),
            dirty: false,
            metrics,
            encoding,
            line_ending,
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn text_mut(&mut self) -> &mut String {
        &mut self.text
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn record_text_change(&mut self) {
        self.metrics = calculate_text_metrics(&self.text);
        self.mark_dirty();
    }

    pub fn replace_text(&mut self, text: String) {
        if self.text != text {
            self.text = text;
            self.record_text_change();
        }
    }

    pub fn mark_saved_as(&mut self, path: PathBuf) {
        self.path = Some(path);
        self.dirty = false;
    }

    #[must_use]
    pub fn encoding(&self) -> TextEncoding {
        self.encoding
    }

    #[must_use]
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    #[must_use]
    fn encoded_text_for_save(&self) -> Cow<'_, [u8]> {
        let normalized = normalize_line_endings(&self.text, self.line_ending);
        let bytes = match normalized {
            Cow::Borrowed(text) if self.encoding == TextEncoding::Utf8 => {
                return Cow::Borrowed(text.as_bytes());
            }
            Cow::Borrowed(text) => text.as_bytes().to_vec(),
            Cow::Owned(text) => text.into_bytes(),
        };

        if self.encoding == TextEncoding::Utf8Bom {
            let mut encoded = Vec::with_capacity(3 + bytes.len());
            encoded.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            encoded.extend_from_slice(&bytes);

            Cow::Owned(encoded)
        } else {
            Cow::Owned(bytes)
        }
    }

    #[must_use]
    pub fn metrics(&self) -> TextMetrics {
        self.metrics
    }

    #[must_use]
    pub fn display_name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|file_name| file_name.to_str())
            .filter(|file_name| !file_name.is_empty())
            .unwrap_or("Untitled")
            .to_owned()
    }

    #[must_use]
    pub fn window_title(&self) -> String {
        let dirty_marker = if self.dirty { "*" } else { "" };

        format!("{dirty_marker}{} - phantom", self.display_name())
    }
}

#[must_use]
pub fn replacement_guard(document: &EditorDocument) -> ReplacementGuard {
    if document.is_dirty() {
        ReplacementGuard::BlockedByUnsavedChanges
    } else {
        ReplacementGuard::SafeToReplace
    }
}

#[must_use]
pub fn calculate_text_metrics(text: &str) -> TextMetrics {
    TextMetrics {
        bytes: text.len(),
        characters: text.chars().count(),
        visual_lines: count_visual_lines(text),
    }
}

#[must_use]
pub fn count_visual_lines(text: &str) -> usize {
    memchr_iter(b'\n', text.as_bytes()).count() + 1
}

#[must_use]
pub fn detect_line_ending(text: &str) -> Option<LineEnding> {
    let bytes = text.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => return Some(LineEnding::Crlf),
            b'\r' => return Some(LineEnding::Cr),
            b'\n' => return Some(LineEnding::Lf),
            _ => index += 1,
        }
    }

    None
}

#[must_use]
pub fn normalize_line_endings(text: &str, line_ending: LineEnding) -> Cow<'_, str> {
    if line_ending == LineEnding::Lf && !text.as_bytes().contains(&b'\r') {
        return Cow::Borrowed(text);
    }

    let mut normalized = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                normalized.push_str(&text[start..index]);
                normalized.push_str(line_ending.as_str());
                index += usize::from(bytes.get(index + 1) == Some(&b'\n')) + 1;
                start = index;
            }
            b'\n' => {
                normalized.push_str(&text[start..index]);
                normalized.push_str(line_ending.as_str());
                index += 1;
                start = index;
            }
            _ => index += 1,
        }
    }

    if start == 0 {
        Cow::Borrowed(text)
    } else {
        normalized.push_str(&text[start..]);
        Cow::Owned(normalized)
    }
}

#[must_use]
pub fn can_inline_edit(bytes: u64, max_inline_edit_bytes: u64) -> bool {
    bytes <= max_inline_edit_bytes
}

#[must_use]
pub fn choose_document_open_mode(bytes: u64, max_inline_edit_bytes: u64) -> DocumentOpenMode {
    if can_inline_edit(bytes, max_inline_edit_bytes) {
        DocumentOpenMode::Inline
    } else {
        DocumentOpenMode::Large
    }
}

pub fn profile_text_file(path: impl AsRef<Path>) -> io::Result<FileProfile> {
    let document_path = path.as_ref().to_path_buf();
    let file = File::open(&document_path)?;
    let bytes = file.metadata()?.len();

    if bytes == 0 {
        return Ok(FileProfile {
            path: document_path,
            bytes,
            visual_lines: 1,
            is_utf8: true,
        });
    }

    let mapped_file = map_file(&file)?;

    Ok(FileProfile {
        path: document_path,
        bytes,
        visual_lines: memchr_iter(b'\n', &mapped_file).count() + 1,
        is_utf8: std::str::from_utf8(&mapped_file).is_ok(),
    })
}

pub fn load_document(path: impl AsRef<Path>) -> io::Result<EditorDocument> {
    load_document_with_limit(path, DEFAULT_MAX_INLINE_EDIT_BYTES)
}

pub fn open_text_document(path: impl AsRef<Path>) -> io::Result<OpenedDocument> {
    open_text_document_with_limits(
        path,
        DEFAULT_MAX_INLINE_EDIT_BYTES,
        DEFAULT_LARGE_VIEW_BYTES,
    )
}

pub fn open_text_document_with_limits(
    path: impl AsRef<Path>,
    max_inline_edit_bytes: u64,
    large_view_bytes: usize,
) -> io::Result<OpenedDocument> {
    let document_path = path.as_ref().to_path_buf();
    let bytes = fs::metadata(&document_path)?.len();

    match choose_document_open_mode(bytes, max_inline_edit_bytes) {
        DocumentOpenMode::Inline => load_document_with_limit(&document_path, max_inline_edit_bytes)
            .map(OpenedDocument::Inline),
        DocumentOpenMode::Large => {
            open_large_document_with_viewport(&document_path, large_view_bytes)
                .map(Box::new)
                .map(OpenedDocument::Large)
        }
    }
}

pub fn load_document_with_limit(
    path: impl AsRef<Path>,
    max_inline_edit_bytes: u64,
) -> io::Result<EditorDocument> {
    let document_path = path.as_ref().to_path_buf();
    let file = File::open(&document_path)?;
    let bytes = file.metadata()?.len();

    if !can_inline_edit(bytes, max_inline_edit_bytes) {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("file is {bytes} bytes; inline editing limit is {max_inline_edit_bytes} bytes"),
        ));
    }

    if bytes == 0 {
        return Ok(EditorDocument::from_saved_text(
            document_path,
            String::new(),
        ));
    }

    let (text, encoding) = read_mapped_text(&file)?;

    Ok(EditorDocument::from_saved_text_with_format(
        document_path,
        text,
        encoding,
    ))
}

pub fn open_large_document(path: impl AsRef<Path>) -> io::Result<LargeDocument> {
    open_large_document_with_viewport(path, DEFAULT_LARGE_VIEW_BYTES)
}

pub fn open_large_document_with_viewport(
    path: impl AsRef<Path>,
    viewport_bytes: usize,
) -> io::Result<LargeDocument> {
    let document_path = path.as_ref().to_path_buf();
    let fingerprint = file_fingerprint(&document_path)?;
    let bytes = fingerprint.len;
    let line_index = LargeFileLineIndex::from_path(&document_path)?;
    let viewport = read_large_viewport(&document_path, 0, viewport_bytes)?;
    ensure_file_fingerprint_matches(&document_path, &fingerprint)?;
    let viewport_start_line =
        line_index.line_at_or_before(&document_path, viewport.start_byte())?;

    Ok(LargeDocument {
        path: document_path,
        bytes,
        fingerprint,
        viewport_bytes,
        viewport,
        line_index,
        viewport_start_line,
        dirty: false,
    })
}

pub fn read_large_viewport(
    path: impl AsRef<Path>,
    start_byte: u64,
    max_bytes: usize,
) -> io::Result<LargeViewport> {
    let mut file = File::open(path)?;
    let total_bytes = file.metadata()?.len();
    let start_byte = start_byte.min(total_bytes);
    let read_start = start_byte.saturating_sub(3);
    let prefix_bytes = (start_byte - read_start) as usize;
    let bytes_to_read =
        (total_bytes - read_start).min((max_bytes + prefix_bytes + 3) as u64) as usize;

    if bytes_to_read == 0 {
        return Ok(LargeViewport::from_text(
            start_byte,
            start_byte,
            String::new(),
            false,
            false,
            true,
        ));
    }

    let buffer = read_mapped_window_or_file(&mut file, read_start, bytes_to_read)?;

    let start_offset = utf8_boundary_at_or_before(&buffer, prefix_bytes);
    let viewport_start = read_start + start_offset as u64;
    let leading_partial_line = viewport_start > 0
        && previous_byte(&mut file, &buffer, read_start, start_offset)? != Some(b'\n');
    let available_bytes = &buffer[start_offset..];
    let candidate_len = available_bytes.len().min(max_bytes);
    let safe_len = utf8_safe_prefix_len(&available_bytes[..candidate_len])?;
    let text = std::str::from_utf8(&available_bytes[..safe_len])
        .map(str::to_owned)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    let end_byte = viewport_start + safe_len as u64;
    let trailing_partial_line = end_byte < total_bytes && !text.as_bytes().ends_with(b"\n");
    let include_trailing_empty_line = end_byte == total_bytes || !text.as_bytes().ends_with(b"\n");

    Ok(LargeViewport::from_text(
        viewport_start,
        end_byte,
        text,
        leading_partial_line,
        trailing_partial_line,
        include_trailing_empty_line,
    ))
}

pub fn save_document(document: &mut EditorDocument, path: impl AsRef<Path>) -> io::Result<()> {
    let document_path = path.as_ref().to_path_buf();

    let encoded_text = document.encoded_text_for_save();

    atomic_write(&document_path, encoded_text.as_ref())?;
    document.mark_saved_as(document_path);

    Ok(())
}

pub fn save_large_document(document: &mut LargeDocument, path: impl AsRef<Path>) -> io::Result<()> {
    let destination = path.as_ref().to_path_buf();

    ensure_file_fingerprint_matches(&document.path, &document.fingerprint)?;

    if document.dirty {
        atomic_stream_replace(
            &document.path,
            &destination,
            document.viewport.start_byte,
            document.viewport.end_byte,
            document.viewport.text.as_bytes(),
            &document.fingerprint,
        )?;
    } else if destination != document.path {
        atomic_copy(&document.path, &destination, &document.fingerprint)?;
    }

    document.mark_saved_as(destination)?;
    Ok(())
}

fn file_fingerprint(path: &Path) -> io::Result<FileFingerprint> {
    let metadata = fs::metadata(path)?;

    Ok(FileFingerprint {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    })
}

fn ensure_file_fingerprint_matches(path: &Path, expected: &FileFingerprint) -> io::Result<()> {
    let current = file_fingerprint(path)?;

    if &current == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            ErrorKind::InvalidData,
            "file changed on disk; reopen it before saving or moving the viewport",
        ))
    }
}

fn read_mapped_text(file: &File) -> io::Result<(String, TextEncoding)> {
    if file.metadata()?.len() == 0 {
        return Ok((String::new(), TextEncoding::Utf8));
    }

    let mapped_file = map_file(file)?;
    let (bytes, encoding) = decode_utf8_bytes(&mapped_file);

    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map(|text| (text, encoding))
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
}

fn decode_utf8_bytes(bytes: &[u8]) -> (&[u8], TextEncoding) {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (&bytes[3..], TextEncoding::Utf8Bom)
    } else {
        (bytes, TextEncoding::Utf8)
    }
}

fn map_file(file: &File) -> io::Result<memmap2::Mmap> {
    let mapped_file = unsafe {
        // The map is read-only and callers keep the file handle alive while mapping.
        MmapOptions::new().map(file)?
    };

    Ok(mapped_file)
}

fn read_mapped_window_or_file(file: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }

    let mapped_window = unsafe { MmapOptions::new().offset(offset).len(len).map(&*file) };

    match mapped_window {
        Ok(mapped_window) => Ok(mapped_window[..].to_vec()),
        Err(_) => {
            let mut buffer = vec![0; len];
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(&mut buffer)?;
            Ok(buffer)
        }
    }
}

fn previous_byte(
    file: &mut File,
    buffer: &[u8],
    buffer_start: u64,
    offset_in_buffer: usize,
) -> io::Result<Option<u8>> {
    if buffer_start == 0 && offset_in_buffer == 0 {
        return Ok(None);
    }

    if offset_in_buffer > 0 {
        return Ok(buffer.get(offset_in_buffer - 1).copied());
    }

    let mut byte = [0];
    file.seek(SeekFrom::Start(buffer_start.saturating_sub(1)))?;
    file.read_exact(&mut byte)?;

    Ok(Some(byte[0]))
}

fn utf8_safe_prefix_len(bytes: &[u8]) -> io::Result<usize> {
    match std::str::from_utf8(bytes) {
        Ok(_) => Ok(bytes.len()),
        Err(error) if error.error_len().is_none() => Ok(error.valid_up_to()),
        Err(error) => Err(io::Error::new(ErrorKind::InvalidData, error)),
    }
}

fn utf8_boundary_at_or_before(bytes: &[u8], index: usize) -> usize {
    let mut offset = index.min(bytes.len());

    while offset > 0 && offset < bytes.len() && is_utf8_continuation_byte(bytes[offset]) {
        offset -= 1;
    }

    offset
}

fn is_utf8_continuation_byte(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

fn atomic_write(destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = destination.file_name().ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidInput,
            "save path must include a file name",
        )
    })?;
    let (temporary_path, temporary_file) = create_temporary_save_file(parent, file_name)?;

    let write_result = write_temporary_file(temporary_file, bytes)
        .and_then(|()| replace_file(&temporary_path, destination))
        .and_then(|()| sync_parent_directory(parent));

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }

    write_result
}

fn atomic_copy(
    source: &Path,
    destination: &Path,
    expected_source: &FileFingerprint,
) -> io::Result<()> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = destination.file_name().ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidInput,
            "save path must include a file name",
        )
    })?;
    let (temporary_path, mut temporary_file) = create_temporary_save_file(parent, file_name)?;

    let write_result = File::open(source)
        .and_then(|mut source_file| io::copy(&mut source_file, &mut temporary_file))
        .and_then(|_| temporary_file.sync_all())
        .and_then(|_| ensure_file_fingerprint_matches(source, expected_source))
        .and_then(|()| replace_file(&temporary_path, destination))
        .and_then(|()| sync_parent_directory(parent));

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }

    write_result
}

fn atomic_stream_replace(
    source: &Path,
    destination: &Path,
    replace_start: u64,
    replace_end: u64,
    replacement: &[u8],
    expected_source: &FileFingerprint,
) -> io::Result<()> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = destination.file_name().ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidInput,
            "save path must include a file name",
        )
    })?;
    let (temporary_path, temporary_file) = create_temporary_save_file(parent, file_name)?;

    let write_result = write_stream_replace_file(
        source,
        temporary_file,
        replace_start,
        replace_end,
        replacement,
    )
    .and_then(|()| ensure_file_fingerprint_matches(source, expected_source))
    .and_then(|()| replace_file(&temporary_path, destination))
    .and_then(|()| sync_parent_directory(parent));

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }

    write_result
}

fn write_stream_replace_file(
    source: &Path,
    mut destination: File,
    replace_start: u64,
    replace_end: u64,
    replacement: &[u8],
) -> io::Result<()> {
    let mut source_file = File::open(source)?;

    {
        let mut prefix = Read::by_ref(&mut source_file).take(replace_start);
        io::copy(&mut prefix, &mut destination)?;
    }

    destination.write_all(replacement)?;
    source_file.seek(SeekFrom::Start(replace_end))?;
    io::copy(&mut source_file, &mut destination)?;
    destination.sync_all()?;

    Ok(())
}

fn create_temporary_save_file(
    parent: &Path,
    file_name: &std::ffi::OsStr,
) -> io::Result<(PathBuf, File)> {
    let timestamp = current_timestamp_nanos()?;

    for attempt_index in 0..16 {
        let temporary_name = format!(
            ".{}.phantom-save-{}-{timestamp}-{attempt_index}.tmp",
            file_name.to_string_lossy(),
            std::process::id()
        );
        let temporary_path = parent.join(temporary_name);

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        "could not create a unique temporary save file",
    ))
}

fn current_timestamp_nanos() -> io::Result<u128> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(io::Error::other)
}

fn write_temporary_file(mut temporary_file: File, bytes: &[u8]) -> io::Result<()> {
    temporary_file.write_all(bytes)?;
    temporary_file.sync_all()?;

    Ok(())
}

#[cfg(unix)]
fn replace_file(temporary_path: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary_path, destination)
}

#[cfg(windows)]
fn replace_file(temporary_path: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temporary_path_wide = path_to_wide_string(temporary_path);
    let destination_wide = path_to_wide_string(destination);
    let result = unsafe {
        MoveFileExW(
            temporary_path_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };

    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn path_to_wide_string(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(not(any(unix, windows)))]
fn replace_file(temporary_path: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary_path, destination)
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn counts_empty_text_as_one_visual_line() {
        let metrics = calculate_text_metrics("");

        assert_eq!(metrics.bytes, 0);
        assert_eq!(metrics.characters, 0);
        assert_eq!(metrics.visual_lines, 1);
    }

    #[test]
    fn counts_utf8_bytes_characters_and_visual_lines() {
        let metrics = calculate_text_metrics("hello\n高速");

        assert_eq!(metrics.bytes, 12);
        assert_eq!(metrics.characters, 8);
        assert_eq!(metrics.visual_lines, 2);
    }

    #[test]
    fn detects_first_line_ending_style() {
        assert_eq!(detect_line_ending("a\r\nb"), Some(LineEnding::Crlf));
        assert_eq!(detect_line_ending("a\rb"), Some(LineEnding::Cr));
        assert_eq!(detect_line_ending("a\nb"), Some(LineEnding::Lf));
        assert_eq!(detect_line_ending("single line"), None);
    }

    #[test]
    fn normalizes_line_endings_to_document_style() {
        let normalized = normalize_line_endings("a\nb\r\nc\r", LineEnding::Crlf);

        assert_eq!(normalized.as_ref(), "a\r\nb\r\nc\r\n");
    }

    #[test]
    fn strips_utf8_bom_from_loaded_text_and_preserves_it_on_save() -> io::Result<()> {
        let path = unique_temp_path("utf8_bom.txt");
        fs::write(&path, [0xEF, 0xBB, 0xBF, b'a', b'\r', b'\n', b'b'])?;

        let mut document = load_document(&path)?;

        assert_eq!(document.text(), "a\r\nb");
        assert_eq!(document.encoding(), TextEncoding::Utf8Bom);
        assert_eq!(document.line_ending(), LineEnding::Crlf);

        document.replace_text("x\ny".to_owned());
        save_document(&mut document, &path)?;

        assert_eq!(
            fs::read(&path)?,
            [0xEF, 0xBB, 0xBF, b'x', b'\r', b'\n', b'y']
        );
        let reloaded_document = load_document(&path)?;

        assert_eq!(reloaded_document.text(), "x\r\ny");
        assert_eq!(reloaded_document.encoding(), TextEncoding::Utf8Bom);
        assert_eq!(reloaded_document.line_ending(), LineEnding::Crlf);

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn replace_text_marks_document_dirty_only_when_changed() {
        let mut document = EditorDocument::untitled();

        document.replace_text(String::new());
        assert!(!document.is_dirty());

        document.replace_text("phantom".to_owned());
        assert!(document.is_dirty());
        assert_eq!(document.metrics().characters, 7);
    }

    #[test]
    fn record_text_change_updates_cached_metrics() {
        let mut document = EditorDocument::untitled();

        document.text_mut().push_str("first\nsecond");
        document.record_text_change();

        assert!(document.is_dirty());
        assert_eq!(document.metrics().visual_lines, 2);
    }

    #[test]
    fn replacement_guard_blocks_dirty_documents() {
        let mut document = EditorDocument::untitled();

        assert_eq!(
            replacement_guard(&document),
            ReplacementGuard::SafeToReplace
        );

        document.replace_text("draft".to_owned());
        assert_eq!(
            replacement_guard(&document),
            ReplacementGuard::BlockedByUnsavedChanges
        );
    }

    #[test]
    fn piece_table_replaces_ranges_without_mutating_original_text() -> io::Result<()> {
        let mut piece_table = PieceTable::from_original("alpha beta gamma".to_owned());

        piece_table.replace_range(6..10, "BETA")?;
        piece_table.replace_range(0..0, "start ")?;
        piece_table.replace_range(piece_table.len()..piece_table.len(), " end")?;

        assert_eq!(piece_table.text(), "start alpha BETA gamma end");

        Ok(())
    }

    #[test]
    fn chunked_line_index_returns_line_text_ranges() {
        let text = "first\nsecond\r\nthird";
        let index = ChunkedLineIndex::from_text(text);

        assert_eq!(index.line_count(), 3);
        assert_eq!(index.line_text_range(0, text), Some(0..5));
        assert_eq!(index.line_text_range(1, text), Some(6..12));
        assert_eq!(index.line_text_range(2, text), Some(14..19));
    }

    #[test]
    fn large_file_line_index_spans_chunk_boundaries() -> io::Result<()> {
        let path = unique_temp_path("global_line_index.txt");
        let text = (0..=LINE_INDEX_CHUNK_LINES)
            .map(|line_index| format!("line {line_index}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, &text)?;

        let index = LargeFileLineIndex::from_path(&path)?;

        assert_eq!(index.bytes(), text.len() as u64);
        assert_eq!(index.line_count(), LINE_INDEX_CHUNK_LINES + 1);
        assert_eq!(index.line_start_byte(&path, 0)?, Some(0));
        assert_eq!(
            index.line_start_byte(&path, LINE_INDEX_CHUNK_LINES)?,
            Some(
                text.rfind('\n')
                    .expect("test text should contain a newline") as u64
                    + 1
            )
        );
        assert_eq!(
            index.line_at_or_before(&path, text.len() as u64)?,
            LINE_INDEX_CHUNK_LINES
        );

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn profiles_text_file_with_memory_map_and_simd_line_scan() -> io::Result<()> {
        let path = unique_temp_path("profile.txt");
        fs::write(&path, "alpha\nbeta\ngamma")?;

        let profile = profile_text_file(&path)?;

        assert_eq!(profile.path, path);
        assert_eq!(profile.bytes, 16);
        assert_eq!(profile.visual_lines, 3);
        assert!(profile.is_utf8);

        fs::remove_file(profile.path)?;
        Ok(())
    }

    #[test]
    fn load_document_handles_empty_files() -> io::Result<()> {
        let path = unique_temp_path("empty.txt");
        fs::write(&path, "")?;

        let document = load_document(&path)?;

        assert_eq!(document.text(), "");
        assert_eq!(document.metrics().visual_lines, 1);
        assert!(!document.is_dirty());

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn reported_200mb_json_uses_large_file_mode() {
        let reported_json_size = 201_838_595;

        assert_eq!(
            choose_document_open_mode(reported_json_size, DEFAULT_MAX_INLINE_EDIT_BYTES),
            DocumentOpenMode::Large
        );
    }

    #[test]
    fn default_inline_limit_accepts_small_files_only() {
        assert!(can_inline_edit(
            DEFAULT_MAX_INLINE_EDIT_BYTES,
            DEFAULT_MAX_INLINE_EDIT_BYTES,
        ));
    }

    #[test]
    fn inline_limit_rejects_one_byte_over_limit() {
        assert!(!can_inline_edit(
            DEFAULT_MAX_INLINE_EDIT_BYTES + 1,
            DEFAULT_MAX_INLINE_EDIT_BYTES
        ));
    }

    #[test]
    fn save_and_load_document_round_trip() -> io::Result<()> {
        let path = unique_temp_path("round_trip.txt");
        let mut document = EditorDocument::untitled();
        document.replace_text("first line\nsecond line".to_owned());

        save_document(&mut document, &path)?;
        let loaded_document = load_document(&path)?;

        assert!(!document.is_dirty());
        assert_eq!(document.path(), Some(path.as_path()));
        assert_eq!(loaded_document.text(), "first line\nsecond line");
        assert!(!loaded_document.is_dirty());

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn load_document_rejects_files_larger_than_inline_limit() -> io::Result<()> {
        let path = unique_temp_path("too_large.txt");
        fs::write(&path, "12345")?;

        let error = load_document_with_limit(&path, 4).expect_err("file should exceed the limit");

        assert_eq!(error.kind(), ErrorKind::InvalidData);

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn open_text_document_uses_large_mode_without_loading_full_file() -> io::Result<()> {
        let path = unique_temp_path("large_mode.txt");
        fs::write(&path, "0123456789abcdef")?;

        let opened_document = open_text_document_with_limits(&path, 4, 6)?;

        match opened_document {
            OpenedDocument::Large(document) => {
                assert_eq!(document.bytes(), 16);
                assert_eq!(document.viewport().start_byte(), 0);
                assert_eq!(document.viewport().end_byte(), 6);
                assert_eq!(document.viewport_text(), "012345");
            }
            OpenedDocument::Inline(_) => panic!("document should use large mode"),
        }

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn large_document_indexes_all_json_lines_for_virtual_scroll() -> io::Result<()> {
        let path = unique_temp_path("eighty_thousand_lines.json");
        let text = (0..80_000)
            .map(|line_index| format!(r#"{{"line":{line_index}}}"#))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, text)?;

        let opened_document = open_text_document_with_limits(&path, 4, 1024)?;

        match opened_document {
            OpenedDocument::Large(mut document) => {
                assert_eq!(document.file_line_count(), 80_000);
                assert!(document.viewport_line_count() < document.file_line_count());

                document.load_viewport_for_line(79_990)?;

                assert!(document.contains_file_line(79_990));
                assert!(document.contains_file_line_range(79_990..80_000));
                assert_eq!(document.file_line_text(79_990), Some(r#"{"line":79990}"#));
            }
            OpenedDocument::Inline(_) => panic!("document should use large mode"),
        }

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn large_document_can_move_between_viewports() -> io::Result<()> {
        let path = unique_temp_path("viewport_move.txt");
        fs::write(&path, "abcdefghij")?;
        let mut document = open_large_document_with_viewport(&path, 4)?;

        assert_eq!(document.viewport_text(), "abcd");

        document.load_next_viewport()?;
        assert_eq!(document.viewport_text(), "efgh");

        document.load_previous_viewport()?;
        assert_eq!(document.viewport_text(), "abcd");

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn large_document_rejects_dirty_viewport_movement() -> io::Result<()> {
        let path = unique_temp_path("dirty_viewport_move.txt");
        fs::write(&path, "abcdefghij")?;
        let mut document = open_large_document_with_viewport(&path, 4)?;

        document.replace_viewport_text("abcdX".to_owned());

        let error = document
            .load_next_viewport()
            .expect_err("dirty viewport should not move");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(document.viewport_text(), "abcdX");

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn save_large_document_replaces_only_visible_viewport() -> io::Result<()> {
        let path = unique_temp_path("large_save.txt");
        fs::write(&path, "abcdef")?;
        let mut document = open_large_document_with_viewport(&path, 3)?;

        document.replace_viewport_text("XYZW".to_owned());
        save_large_document(&mut document, &path)?;

        assert_eq!(fs::read_to_string(&path)?, "XYZWdef");
        assert!(!document.is_dirty());

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn save_large_document_rejects_external_source_changes() -> io::Result<()> {
        let path = unique_temp_path("large_save_conflict.txt");
        fs::write(&path, "abcdef")?;
        let mut document = open_large_document_with_viewport(&path, 3)?;

        document.replace_viewport_text("XYZ".to_owned());
        fs::write(&path, "abc---def")?;

        let error = save_large_document(&mut document, &path)
            .expect_err("externally changed file should be rejected");

        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(fs::read_to_string(&path)?, "abc---def");

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn large_document_replaces_single_indexed_line() -> io::Result<()> {
        let path = unique_temp_path("large_line_edit.txt");
        fs::write(&path, "first\nsecond\nthird")?;
        let mut document = open_large_document_with_viewport(&path, 64)?;

        assert_eq!(document.viewport_line_count(), 3);
        assert_eq!(document.viewport_line_text(1), Some("second"));

        let changed = document.replace_viewport_line(1, "SECOND")?;

        assert!(changed);
        assert!(document.is_dirty());
        assert_eq!(document.viewport_text(), "first\nSECOND\nthird");

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn large_document_rejects_leading_partial_line_edit() -> io::Result<()> {
        let path = unique_temp_path("leading_partial_line.txt");
        fs::write(&path, "first\nsecond\nthird")?;
        let mut document = open_large_document_with_viewport(&path, 64)?;

        document.load_viewport_at(8)?;

        assert!(document.viewport().leading_partial_line());
        assert!(!document.is_viewport_line_editable(0));
        assert!(document.is_viewport_line_editable(1));

        let error = document
            .replace_viewport_line(0, "SECOND")
            .expect_err("partial leading file line should be read-only");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(document.viewport_text(), "cond\nthird");

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn large_document_does_not_expose_next_line_at_newline_boundary() -> io::Result<()> {
        let path = unique_temp_path("newline_boundary.txt");
        fs::write(&path, "a\nb\nc")?;
        let mut document = open_large_document_with_viewport(&path, 2)?;

        assert_eq!(document.viewport_text(), "a\n");
        assert_eq!(document.file_line_count(), 3);
        assert_eq!(document.viewport_line_count(), 1);
        assert!(document.contains_file_line(0));
        assert!(!document.contains_file_line(1));
        assert!(document.contains_file_line_range(0..1));
        assert!(!document.contains_file_line_range(1..2));
        assert_eq!(document.file_line_text(1), None);

        let error = document
            .replace_file_line(1, "B")
            .expect_err("line outside the loaded viewport should not be editable");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        document.replace_file_line(0, "A")?;
        save_large_document(&mut document, &path)?;
        assert_eq!(fs::read_to_string(&path)?, "A\nb\nc");

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn large_document_preserves_trailing_partial_line_on_save() -> io::Result<()> {
        let path = unique_temp_path("trailing_partial_line.txt");
        fs::write(&path, "first\nsecond\nthird")?;
        let mut document = open_large_document_with_viewport(&path, 8)?;

        assert!(document.viewport().trailing_partial_line());
        assert!(document.is_viewport_line_editable(0));
        assert!(!document.is_viewport_line_editable(1));

        document.replace_viewport_line(0, "FIRST")?;
        let error = document
            .replace_viewport_line(1, "SECOND")
            .expect_err("partial trailing file line should be read-only");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        save_large_document(&mut document, &path)?;
        assert_eq!(fs::read_to_string(&path)?, "FIRST\nsecond\nthird");

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn stream_replace_rechecks_source_before_replacing_destination() -> io::Result<()> {
        let path = unique_temp_path("stream_replace_conflict.txt");
        fs::write(&path, "abcdef")?;
        let stale_fingerprint = file_fingerprint(&path)?;
        fs::write(&path, "abc---def")?;

        let error = atomic_stream_replace(&path, &path, 0, 3, b"XYZ", &stale_fingerprint)
            .expect_err("stale source should be rejected before replacement");

        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(fs::read_to_string(&path)?, "abc---def");

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn atomic_copy_rechecks_source_before_replacing_destination() -> io::Result<()> {
        let source_path = unique_temp_path("copy_conflict_source.txt");
        let destination_path = unique_temp_path("copy_conflict_destination.txt");
        fs::write(&source_path, "abcdef")?;
        let stale_fingerprint = file_fingerprint(&source_path)?;
        fs::write(&source_path, "abc---def")?;

        let error = atomic_copy(&source_path, &destination_path, &stale_fingerprint)
            .expect_err("stale source copy should be rejected before replacement");

        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(!destination_path.exists());

        fs::remove_file(source_path)?;
        Ok(())
    }

    #[test]
    fn read_large_viewport_rejects_invalid_utf8_bytes() -> io::Result<()> {
        let path = unique_temp_path("invalid_utf8.txt");
        fs::write(&path, [0xff, b'a', b'b', b'c'])?;

        let error = read_large_viewport(&path, 0, 4)
            .expect_err("invalid UTF-8 should not be converted lossily");

        assert_eq!(error.kind(), ErrorKind::InvalidData);

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn large_viewport_save_preserves_utf8_boundary_prefix() -> io::Result<()> {
        let path = unique_temp_path("large_utf8_boundary.txt");
        fs::write(&path, "éabcdef")?;
        let mut document = open_large_document_with_viewport(&path, 4)?;

        document.load_viewport_at(1)?;
        assert_eq!(document.viewport().start_byte(), 0);
        assert_eq!(document.viewport_text(), "éab");

        document.replace_viewport_text("éXY".to_owned());
        save_large_document(&mut document, &path)?;

        assert_eq!(fs::read_to_string(&path)?, "éXYcdef");

        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn save_document_replaces_existing_content() -> io::Result<()> {
        let path = unique_temp_path("replace_existing.txt");
        fs::write(&path, "old content")?;

        let mut document = EditorDocument::untitled();
        document.replace_text("new content".to_owned());
        save_document(&mut document, &path)?;

        assert_eq!(fs::read_to_string(&path)?, "new content");
        assert!(!document.is_dirty());

        fs::remove_file(path)?;
        Ok(())
    }

    fn unique_temp_path(file_name: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after the Unix epoch")
            .as_nanos();

        std::env::temp_dir().join(format!(
            "phantom_{}_{}_{}",
            std::process::id(),
            timestamp,
            file_name
        ))
    }
}
