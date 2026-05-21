use memchr::memchr_iter;
use memmap2::MmapOptions;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_MAX_INLINE_EDIT_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TextMetrics {
    pub bytes: usize,
    pub characters: usize,
    pub visual_lines: usize,
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct EditorDocument {
    text: String,
    path: Option<PathBuf>,
    dirty: bool,
    metrics: TextMetrics,
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
        }
    }

    #[must_use]
    pub fn from_saved_text(path: PathBuf, text: String) -> Self {
        let metrics = calculate_text_metrics(&text);

        Self {
            text,
            path: Some(path),
            dirty: false,
            metrics,
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

pub fn load_document_with_limit(
    path: impl AsRef<Path>,
    max_inline_edit_bytes: u64,
) -> io::Result<EditorDocument> {
    let profile = profile_text_file(path)?;

    if profile.bytes > max_inline_edit_bytes {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "file is {} bytes across {} visual lines; inline editing limit is {} bytes",
                profile.bytes, profile.visual_lines, max_inline_edit_bytes
            ),
        ));
    }

    if profile.bytes == 0 {
        return Ok(EditorDocument::from_saved_text(profile.path, String::new()));
    }

    let file = File::open(&profile.path)?;
    let text = read_mapped_utf8(&file)?;

    Ok(EditorDocument::from_saved_text(profile.path, text))
}

pub fn save_document(document: &mut EditorDocument, path: impl AsRef<Path>) -> io::Result<()> {
    let document_path = path.as_ref().to_path_buf();

    atomic_write(&document_path, document.text().as_bytes())?;
    document.mark_saved_as(document_path);

    Ok(())
}

fn read_mapped_utf8(file: &File) -> io::Result<String> {
    if file.metadata()?.len() == 0 {
        return Ok(String::new());
    }

    let mapped_file = map_file(file)?;

    std::str::from_utf8(&mapped_file)
        .map(str::to_owned)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
}

fn map_file(file: &File) -> io::Result<memmap2::Mmap> {
    let mapped_file = unsafe {
        // The map is read-only and callers keep the file handle alive while mapping.
        MmapOptions::new().map(file)?
    };

    Ok(mapped_file)
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
