use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui::{
    self,
    text::{CCursor, CCursorRange, LayoutJob},
    widgets::text_edit::TextEditState,
    Align, Align2, Color32, CornerRadius, FontFamily, FontId, Frame, KeyboardShortcut, Layout,
    Margin, Modifiers, RichText, Sense, Stroke, TextEdit, TextFormat, Vec2,
};
use phantom::{
    editor_ops::{self, CaseTransform, TextSelection},
    open_text_document, save_document, save_large_document,
    search::{find_text_matches, line_preview, replace_all as search_replace_all, SearchOptions},
    EditorDocument, LargeDocument, OpenedDocument,
};
use rfd::FileDialog;

const VSCODE_BG: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x1e);
const VSCODE_PANEL: Color32 = Color32::from_rgb(0x25, 0x25, 0x26);
const VSCODE_ACTIVITY: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);
const VSCODE_BORDER: Color32 = Color32::from_rgb(0x2d, 0x2d, 0x30);
const VSCODE_TAB: Color32 = Color32::from_rgb(0x2d, 0x2d, 0x2d);
const VSCODE_STATUS: Color32 = Color32::from_rgb(0x00, 0x7a, 0xcc);
const VSCODE_STATUS_DIRTY: Color32 = Color32::from_rgb(0xcc, 0x6c, 0x00);
const VSCODE_STATUS_ERROR: Color32 = Color32::from_rgb(0xa1, 0x26, 0x0d);
const VSCODE_TEXT: Color32 = Color32::from_rgb(0xcc, 0xcc, 0xcc);
const VSCODE_TEXT_DIM: Color32 = Color32::from_rgb(0x85, 0x85, 0x85);
const VSCODE_ACCENT: Color32 = Color32::from_rgb(0x00, 0x7a, 0xcc);
const VSCODE_HIGHLIGHT: Color32 = Color32::from_rgb(0x61, 0x3f, 0x12);
const VSCODE_SELECTION_HIGHLIGHT: Color32 = Color32::from_rgb(0x26, 0x4f, 0x78);
const SYNTAX_KEYWORD: Color32 = Color32::from_rgb(0x56, 0x9c, 0xd6);
const SYNTAX_STRING: Color32 = Color32::from_rgb(0xce, 0x91, 0x78);
const SYNTAX_NUMBER: Color32 = Color32::from_rgb(0xb5, 0xce, 0xa8);
const SYNTAX_COMMENT: Color32 = Color32::from_rgb(0x6a, 0x99, 0x55);
const SYNTAX_BRACKET: Color32 = Color32::from_rgb(0xda, 0xd2, 0x70);
const SIDEBAR_CONTENT_INDENT: f32 = 12.0;
const SIDEBAR_PATH_MIN_WRAP_WIDTH: f32 = 48.0;
const EDITOR_GUTTER_WIDTH: f32 = 72.0;
const EDITOR_GUTTER_GAP: f32 = 10.0;
const EDITOR_MIN_TEXT_WIDTH: f32 = 360.0;
const EDITOR_MIN_WRAP_TEXT_WIDTH: f32 = 48.0;
const EDITOR_MONOSPACE_CHAR_WIDTH: f32 = 9.0;
const EDITOR_ROW_HEIGHT: f32 = 22.0;
const DEFAULT_EDITOR_FONT_SIZE: f32 = 14.0;
const MIN_EDITOR_FONT_SIZE: f32 = 10.0;
const MAX_EDITOR_FONT_SIZE: f32 = 32.0;
const INLINE_EDITOR_ID_SOURCE: &str = "inline_editor";
const WRAP_MEASURE_OVERSCAN_LINES: usize = 64;
const SEARCH_RESULT_LIMIT: usize = 200;
const HIGHLIGHT_SEARCH_LIMIT: usize = 1_000;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("phantom")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "phantom",
        native_options,
        Box::new(|creation_context| {
            apply_vscode_theme(&creation_context.egui_ctx);
            Ok(Box::<PhantomApp>::default())
        }),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AppMessage {
    Ready,
    Info(String),
    Error(String),
}

impl AppMessage {
    fn text(&self) -> &str {
        match self {
            AppMessage::Ready => "Ready",
            AppMessage::Info(message) | AppMessage::Error(message) => message,
        }
    }

    fn is_error(&self) -> bool {
        matches!(self, AppMessage::Error(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarView {
    Explorer,
    Search,
    Outline,
}

impl SidebarView {
    fn title(self) -> &'static str {
        match self {
            SidebarView::Explorer => "EXPLORER",
            SidebarView::Search => "SEARCH",
            SidebarView::Outline => "OUTLINE",
        }
    }
}

#[derive(Debug, Clone)]
enum ActiveDocument {
    Inline(EditorDocument),
    Large(Box<LargeDocument>),
}

impl Default for ActiveDocument {
    fn default() -> Self {
        Self::Inline(EditorDocument::untitled())
    }
}

impl ActiveDocument {
    fn display_name(&self) -> String {
        match self {
            ActiveDocument::Inline(document) => document.display_name(),
            ActiveDocument::Large(document) => document.display_name(),
        }
    }

    fn window_title(&self) -> String {
        match self {
            ActiveDocument::Inline(document) => document.window_title(),
            ActiveDocument::Large(document) => document.window_title(),
        }
    }

    fn path(&self) -> Option<&Path> {
        match self {
            ActiveDocument::Inline(document) => document.path(),
            ActiveDocument::Large(document) => Some(document.path()),
        }
    }

    fn is_dirty(&self) -> bool {
        match self {
            ActiveDocument::Inline(document) => document.is_dirty(),
            ActiveDocument::Large(document) => document.is_dirty(),
        }
    }

    fn mode_label(&self) -> &'static str {
        match self {
            ActiveDocument::Inline(_) => "Inline Edit",
            ActiveDocument::Large(_) => "Large File Viewport",
        }
    }
}

#[derive(Debug)]
enum OpenTaskResult {
    Opened(OpenedDocument),
    Failed { path: PathBuf, error: String },
}

#[derive(Debug)]
enum SaveTaskResult {
    Saved {
        document: Box<ActiveDocument>,
        path: PathBuf,
    },
    Failed {
        path: PathBuf,
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewportMove {
    Previous,
    Next,
    Line(usize),
}

#[derive(Debug)]
enum ViewportTaskResult {
    Loaded(Box<LargeDocument>),
    Failed(String),
}

enum LargeEditorAction {
    Message(AppMessage),
    LoadLine(usize),
}

#[derive(Debug, Default)]
struct WrapLineHeightCache {
    extras: BTreeMap<usize, f32>,
    measured_lines: BTreeSet<usize>,
    text_width: Option<f32>,
    line_range: Option<Range<usize>>,
    scroll_anchor_line: Option<usize>,
}

impl WrapLineHeightCache {
    fn clear(&mut self) {
        self.extras.clear();
        self.measured_lines.clear();
        self.text_width = None;
        self.line_range = None;
        self.scroll_anchor_line = None;
    }

    fn invalidate_measurements(&mut self) {
        self.extras.clear();
        self.measured_lines.clear();
        self.text_width = None;
        self.line_range = None;
    }
}

#[derive(Debug, Default)]
struct SearchPanelState {
    query: String,
    replacement: String,
    match_case: bool,
    whole_word: bool,
    use_regex: bool,
    results: Vec<SearchResultRow>,
    error: Option<String>,
    searched_scope: Option<&'static str>,
}

impl SearchPanelState {
    fn options(&self) -> SearchOptions {
        SearchOptions {
            match_case: self.match_case,
            whole_word: self.whole_word,
            use_regex: self.use_regex,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct SearchResultRow {
    line_number: usize,
    preview: String,
}

#[derive(Debug, Default)]
struct GoToLineState {
    visible: bool,
    input: String,
    error: Option<String>,
    request_focus: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShortcutAction {
    New,
    Open,
    Save,
    SaveAs,
    Search,
    GoToLine,
    RunSearch,
    ToggleWrap,
    ToggleHelp,
    AddNextOccurrence,
    SelectAllOccurrences,
    SelectLines,
    RectangularSelection,
    CopyRectangle,
    PasteRectangle,
    MoveLineUp,
    MoveLineDown,
    CopyLineUp,
    CopyLineDown,
    DeleteLine,
    Uppercase,
    Lowercase,
    ClearMultiCursor,
    ZoomIn,
    ZoomOut,
    ResetZoom,
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum InlineInputEdit {
    Insert(String),
    Backspace,
    Delete,
}

struct PhantomApp {
    document: ActiveDocument,
    path_input: String,
    message: AppMessage,
    active_view: SidebarView,
    sidebar_visible: bool,
    wrap_lines: bool,
    wrap_line_heights: WrapLineHeightCache,
    search: SearchPanelState,
    go_to_line: GoToLineState,
    show_help: bool,
    editor_font_size: f32,
    current_inline_selection: Option<TextSelection>,
    inline_selections: Vec<TextSelection>,
    pending_inline_selection: Option<TextSelection>,
    rectangular_clipboard: Option<String>,
    open_receiver: Option<Receiver<OpenTaskResult>>,
    save_receiver: Option<Receiver<SaveTaskResult>>,
    viewport_receiver: Option<Receiver<ViewportTaskResult>>,
}

impl Default for PhantomApp {
    fn default() -> Self {
        Self {
            document: ActiveDocument::default(),
            path_input: String::new(),
            message: AppMessage::Ready,
            active_view: SidebarView::Explorer,
            sidebar_visible: true,
            wrap_lines: false,
            wrap_line_heights: WrapLineHeightCache::default(),
            search: SearchPanelState::default(),
            go_to_line: GoToLineState::default(),
            show_help: false,
            editor_font_size: DEFAULT_EDITOR_FONT_SIZE,
            current_inline_selection: None,
            inline_selections: Vec::new(),
            pending_inline_selection: None,
            rectangular_clipboard: None,
            open_receiver: None,
            save_receiver: None,
            viewport_receiver: None,
        }
    }
}

impl eframe::App for PhantomApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_background_open(context);
        self.poll_background_save(context);
        self.poll_background_viewport(context);
        self.guard_close_request(context);
        self.handle_dropped_files(context);
        self.handle_keyboard_shortcuts(context);
        self.handle_inline_multi_cursor_input(context);
        context.send_viewport_cmd(egui::ViewportCommand::Title(self.document.window_title()));

        self.show_menu_bar(context);
        self.show_status_bar(context);
        self.show_activity_bar(context);

        if self.sidebar_visible {
            self.show_sidebar(context);
        }

        self.show_tab_bar(context);
        self.show_editor(context);
        self.show_go_to_line_window(context);
        self.show_help_window(context);
    }
}

impl PhantomApp {
    fn poll_background_open(&mut self, context: &egui::Context) {
        let Some(receiver) = self.open_receiver.as_ref() else {
            return;
        };

        match receiver.try_recv() {
            Ok(result) => {
                self.open_receiver = None;
                self.apply_open_result(result);
            }
            Err(TryRecvError::Empty) => context.request_repaint_after(Duration::from_millis(50)),
            Err(TryRecvError::Disconnected) => {
                self.open_receiver = None;
                self.message = AppMessage::Error("Open task failed".to_owned());
            }
        }
    }

    fn poll_background_save(&mut self, context: &egui::Context) {
        let Some(receiver) = self.save_receiver.as_ref() else {
            return;
        };

        match receiver.try_recv() {
            Ok(result) => {
                self.save_receiver = None;
                self.apply_save_result(result);
            }
            Err(TryRecvError::Empty) => context.request_repaint_after(Duration::from_millis(50)),
            Err(TryRecvError::Disconnected) => {
                self.save_receiver = None;
                self.message = AppMessage::Error("Save task failed".to_owned());
            }
        }
    }

    fn poll_background_viewport(&mut self, context: &egui::Context) {
        let Some(receiver) = self.viewport_receiver.as_ref() else {
            return;
        };

        match receiver.try_recv() {
            Ok(result) => {
                self.viewport_receiver = None;
                self.apply_viewport_result(result);
            }
            Err(TryRecvError::Empty) => context.request_repaint_after(Duration::from_millis(50)),
            Err(TryRecvError::Disconnected) => {
                self.viewport_receiver = None;
                self.message = AppMessage::Error("Viewport task failed".to_owned());
            }
        }
    }

    fn apply_open_result(&mut self, result: OpenTaskResult) {
        match result {
            OpenTaskResult::Opened(OpenedDocument::Inline(document)) => {
                self.path_input = document
                    .path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                self.wrap_line_heights.clear();
                self.clear_inline_edit_state();
                self.document = ActiveDocument::Inline(document);
                self.message = AppMessage::Info("Opened".to_owned());
            }
            OpenTaskResult::Opened(OpenedDocument::Large(document)) => {
                self.path_input = document.path().display().to_string();
                self.message = AppMessage::Info(format!(
                    "Opened window {}..{} of {} bytes",
                    document.viewport().start_byte(),
                    document.viewport().end_byte(),
                    document.bytes()
                ));
                self.wrap_line_heights.clear();
                self.clear_inline_edit_state();
                self.document = ActiveDocument::Large(document);
            }
            OpenTaskResult::Failed { path, error } => {
                self.path_input = path.display().to_string();
                self.message = AppMessage::Error(format!("Open failed: {error}"));
            }
        }
    }

    fn apply_save_result(&mut self, result: SaveTaskResult) {
        match result {
            SaveTaskResult::Saved { document, path } => {
                self.document = *document;
                self.path_input = path.display().to_string();
                self.message = AppMessage::Info("Saved".to_owned());
            }
            SaveTaskResult::Failed { path, error } => {
                self.path_input = path.display().to_string();
                self.message = AppMessage::Error(format!("Save failed: {error}"));
            }
        }
    }

    fn apply_viewport_result(&mut self, result: ViewportTaskResult) {
        match result {
            ViewportTaskResult::Loaded(document) => {
                self.message = AppMessage::Info(format!(
                    "Loaded window {}..{}",
                    document.viewport().start_byte(),
                    document.viewport().end_byte()
                ));
                self.document = ActiveDocument::Large(document);
            }
            ViewportTaskResult::Failed(error) => {
                self.message = AppMessage::Error(format!("Move failed: {error}"));
            }
        }
    }

    fn handle_dropped_files(&mut self, context: &egui::Context) {
        let dropped_path = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .find_map(|file| file.path.clone())
        });

        if let Some(path) = dropped_path {
            if self.can_replace_document("opening a dropped file") {
                self.open_document_at(path);
            }
        }
    }

    fn handle_keyboard_shortcuts(&mut self, context: &egui::Context) {
        for action in collect_shortcut_actions(context) {
            self.apply_shortcut_action(action, context);
        }
    }

    fn handle_inline_multi_cursor_input(&mut self, context: &egui::Context) {
        if self.inline_selections.is_empty() || !matches!(self.document, ActiveDocument::Inline(_))
        {
            return;
        }

        let edits = take_multi_cursor_text_edits(context);

        for edit in edits {
            let ActiveDocument::Inline(document) = &self.document else {
                return;
            };
            let result = match edit {
                InlineInputEdit::Insert(text) => {
                    editor_ops::replace_selections(document.text(), &self.inline_selections, &text)
                }
                InlineInputEdit::Backspace => {
                    let targets =
                        editor_ops::backspace_targets(document.text(), &self.inline_selections);

                    if targets.is_empty() {
                        continue;
                    }

                    editor_ops::replace_selections(document.text(), &targets, "")
                }
                InlineInputEdit::Delete => {
                    let targets =
                        editor_ops::delete_targets(document.text(), &self.inline_selections);

                    if targets.is_empty() {
                        continue;
                    }

                    editor_ops::replace_selections(document.text(), &targets, "")
                }
            };

            self.apply_inline_edit_result(result, "Edited multi-cursor selection(s)");
        }
    }

    fn apply_shortcut_action(&mut self, action: ShortcutAction, context: &egui::Context) {
        match action {
            ShortcutAction::New => self.new_document(),
            ShortcutAction::Open => self.open_document_with_dialog(),
            ShortcutAction::Save => self.save_document(),
            ShortcutAction::SaveAs => self.save_document_with_dialog(),
            ShortcutAction::Search => self.open_search_panel(),
            ShortcutAction::GoToLine => self.open_go_to_line(),
            ShortcutAction::RunSearch => {
                if self.search.query.is_empty() {
                    self.open_search_panel();
                } else {
                    self.run_search();
                }
            }
            ShortcutAction::AddNextOccurrence => self.add_next_occurrence_selection(),
            ShortcutAction::SelectAllOccurrences => self.select_all_occurrences(),
            ShortcutAction::SelectLines => self.select_current_lines(),
            ShortcutAction::RectangularSelection => self.create_rectangular_selection(),
            ShortcutAction::CopyRectangle => self.copy_rectangular_selection(),
            ShortcutAction::PasteRectangle => self.paste_rectangular_selection(),
            ShortcutAction::MoveLineUp => self.move_selected_lines(true),
            ShortcutAction::MoveLineDown => self.move_selected_lines(false),
            ShortcutAction::CopyLineUp => self.copy_selected_lines(true),
            ShortcutAction::CopyLineDown => self.copy_selected_lines(false),
            ShortcutAction::DeleteLine => self.delete_selected_lines(),
            ShortcutAction::Uppercase => self.convert_selected_case(CaseTransform::Upper),
            ShortcutAction::Lowercase => self.convert_selected_case(CaseTransform::Lower),
            ShortcutAction::ClearMultiCursor => self.clear_multi_cursor(),
            ShortcutAction::ToggleWrap => {
                self.wrap_lines = !self.wrap_lines;
                self.wrap_line_heights.invalidate_measurements();
                self.message = AppMessage::Info(if self.wrap_lines {
                    "Wrap Lines enabled".to_owned()
                } else {
                    "Wrap Lines disabled".to_owned()
                });
            }
            ShortcutAction::ToggleHelp => {
                self.show_help = !self.show_help;
            }
            ShortcutAction::ZoomIn => self.zoom_editor(1.1),
            ShortcutAction::ZoomOut => self.zoom_editor(1.0 / 1.1),
            ShortcutAction::ResetZoom => self.reset_editor_zoom(),
        }

        context.request_repaint();
    }

    fn open_search_panel(&mut self) {
        self.sidebar_visible = true;
        self.active_view = SidebarView::Search;
        self.message = AppMessage::Info("Search ready".to_owned());
    }

    fn open_go_to_line(&mut self) {
        self.go_to_line.visible = true;
        self.go_to_line.input = self.current_line_hint().to_string();
        self.go_to_line.error = None;
        self.go_to_line.request_focus = true;
    }

    fn current_line_hint(&self) -> usize {
        match &self.document {
            ActiveDocument::Inline(_) => 1,
            ActiveDocument::Large(document) => document.viewport_start_line() + 1,
        }
    }

    fn document_line_count(&self) -> usize {
        match &self.document {
            ActiveDocument::Inline(document) => document.metrics().visual_lines,
            ActiveDocument::Large(document) => document.file_line_count(),
        }
        .max(1)
    }

    fn submit_go_to_line(&mut self, context: &egui::Context) {
        let line_count = self.document_line_count();
        let line_index = match parse_go_to_line_index(&self.go_to_line.input, line_count) {
            Ok(line_index) => line_index,
            Err(error) => {
                self.go_to_line.error = Some(error);
                return;
            }
        };

        self.go_to_line.visible = false;
        self.go_to_line.error = None;

        if let ActiveDocument::Inline(document) = &self.document {
            self.pending_inline_selection = Some(TextSelection::cursor(line_start_char_index(
                document.text(),
                line_index,
            )));
            self.message = AppMessage::Info(format!("Moved cursor to line {}", line_index + 1));
        } else {
            self.move_large_viewport_to_line(line_index);
        }

        context.request_repaint();
    }

    fn zoom_editor(&mut self, multiplier: f32) {
        self.editor_font_size = (self.editor_font_size * multiplier)
            .clamp(MIN_EDITOR_FONT_SIZE, MAX_EDITOR_FONT_SIZE)
            .round();
        self.wrap_line_heights.invalidate_measurements();
        self.message = AppMessage::Info(format!("Editor zoom {}px", self.editor_font_size));
    }

    fn reset_editor_zoom(&mut self) {
        self.editor_font_size = DEFAULT_EDITOR_FONT_SIZE;
        self.wrap_line_heights.invalidate_measurements();
        self.message = AppMessage::Info(format!("Editor zoom {}px", self.editor_font_size));
    }

    fn clear_inline_edit_state(&mut self) {
        self.current_inline_selection = None;
        self.inline_selections.clear();
        self.pending_inline_selection = None;
        self.rectangular_clipboard = None;
    }

    fn active_inline_selections(&self) -> Vec<TextSelection> {
        if !self.inline_selections.is_empty() {
            return self.inline_selections.clone();
        }

        self.current_inline_selection
            .map(|selection| vec![selection])
            .unwrap_or_else(|| vec![TextSelection::cursor(0)])
    }

    fn set_inline_selections(
        &mut self,
        selections: Vec<TextSelection>,
        message: impl Into<String>,
    ) {
        let selections = editor_ops::normalize_selections(&selections);

        self.pending_inline_selection = selections.last().copied();
        self.current_inline_selection = self.pending_inline_selection;
        self.inline_selections = selections;
        self.message = AppMessage::Info(message.into());
    }

    fn apply_inline_edit_result(
        &mut self,
        result: editor_ops::EditResult,
        message: impl Into<String>,
    ) {
        let ActiveDocument::Inline(document) = &mut self.document else {
            self.message =
                AppMessage::Error("This command is available for inline documents".to_owned());
            return;
        };

        document.replace_text(result.text);
        self.set_inline_selections(result.selections, message);
    }

    fn add_next_occurrence_selection(&mut self) {
        let ActiveDocument::Inline(document) = &self.document else {
            self.message =
                AppMessage::Error("Multi-cursor is available for inline documents".to_owned());
            return;
        };
        let mut selections = self.active_inline_selections();

        if selections.len() == 1 && selections[0].is_cursor() {
            if let Some(word_selection) = editor_ops::word_at(document.text(), selections[0].head) {
                selections[0] = word_selection;
            }
        }

        let next = editor_ops::add_next_occurrence(document.text(), &selections);
        let count = next.len();

        self.set_inline_selections(next, format!("{count} cursor/selection target(s)"));
    }

    fn select_all_occurrences(&mut self) {
        let ActiveDocument::Inline(document) = &self.document else {
            self.message =
                AppMessage::Error("Multi-cursor is available for inline documents".to_owned());
            return;
        };
        let selection = self
            .active_inline_selections()
            .last()
            .copied()
            .and_then(|selection| {
                if selection.is_cursor() {
                    editor_ops::word_at(document.text(), selection.head)
                } else {
                    Some(selection)
                }
            });

        let Some(selection) = selection else {
            self.message =
                AppMessage::Error("Select text or place the cursor on a word".to_owned());
            return;
        };

        let selections = editor_ops::select_all_occurrences(document.text(), selection);
        let count = selections.len();

        self.set_inline_selections(selections, format!("Selected {count} occurrence(s)"));
    }

    fn select_current_lines(&mut self) {
        let ActiveDocument::Inline(document) = &self.document else {
            self.message =
                AppMessage::Error("Line selection is available for inline documents".to_owned());
            return;
        };
        let selections =
            editor_ops::select_current_lines(document.text(), &self.active_inline_selections());

        self.set_inline_selections(selections, "Selected line range");
    }

    fn create_rectangular_selection(&mut self) {
        let ActiveDocument::Inline(document) = &self.document else {
            self.message = AppMessage::Error(
                "Rectangular selection is available for inline documents".to_owned(),
            );
            return;
        };
        let Some(selection) = self.current_inline_selection else {
            self.message = AppMessage::Error("Select a multi-line range first".to_owned());
            return;
        };

        if selection.is_cursor() {
            self.message = AppMessage::Error("Select a multi-line range first".to_owned());
            return;
        }

        let selections = editor_ops::rectangular_selections(document.text(), selection);
        let count = selections.len();

        self.set_inline_selections(
            selections,
            format!("Rectangular selection: {count} line(s)"),
        );
    }

    fn copy_rectangular_selection(&mut self) {
        let ActiveDocument::Inline(document) = &self.document else {
            self.message =
                AppMessage::Error("Rectangular copy is available for inline documents".to_owned());
            return;
        };
        let selections = self.active_inline_selections();

        self.rectangular_clipboard =
            Some(editor_ops::rectangular_text(document.text(), &selections));
        self.message = AppMessage::Info(format!(
            "Copied rectangle from {} line(s)",
            selections.len()
        ));
    }

    fn paste_rectangular_selection(&mut self) {
        let Some(block) = self.rectangular_clipboard.clone() else {
            self.message = AppMessage::Error("Copy a rectangle before pasting it".to_owned());
            return;
        };
        let ActiveDocument::Inline(document) = &self.document else {
            self.message =
                AppMessage::Error("Rectangular paste is available for inline documents".to_owned());
            return;
        };
        let result = editor_ops::paste_rectangular(
            document.text(),
            &self.active_inline_selections(),
            &block,
        );

        self.apply_inline_edit_result(result, "Pasted rectangle");
    }

    fn move_selected_lines(&mut self, up: bool) {
        let ActiveDocument::Inline(document) = &self.document else {
            self.message =
                AppMessage::Error("Line move is available for inline documents".to_owned());
            return;
        };
        let result = editor_ops::move_lines(document.text(), &self.active_inline_selections(), up);

        self.apply_inline_edit_result(
            result,
            if up {
                "Moved line(s) up"
            } else {
                "Moved line(s) down"
            },
        );
    }

    fn copy_selected_lines(&mut self, up: bool) {
        let ActiveDocument::Inline(document) = &self.document else {
            self.message =
                AppMessage::Error("Line copy is available for inline documents".to_owned());
            return;
        };
        let result = editor_ops::copy_lines(document.text(), &self.active_inline_selections(), up);

        self.apply_inline_edit_result(
            result,
            if up {
                "Copied line(s) up"
            } else {
                "Copied line(s) down"
            },
        );
    }

    fn delete_selected_lines(&mut self) {
        let ActiveDocument::Inline(document) = &self.document else {
            self.message =
                AppMessage::Error("Line delete is available for inline documents".to_owned());
            return;
        };
        let result = editor_ops::delete_lines(document.text(), &self.active_inline_selections());

        self.apply_inline_edit_result(result, "Deleted line(s)");
    }

    fn convert_selected_case(&mut self, transform: CaseTransform) {
        let ActiveDocument::Inline(document) = &self.document else {
            self.message =
                AppMessage::Error("Case conversion is available for inline documents".to_owned());
            return;
        };
        let result =
            editor_ops::convert_case(document.text(), &self.active_inline_selections(), transform);
        let message = match transform {
            CaseTransform::Upper => "Converted to uppercase",
            CaseTransform::Lower => "Converted to lowercase",
        };

        self.apply_inline_edit_result(result, message);
    }

    fn clear_multi_cursor(&mut self) {
        let selection = self.current_inline_selection;
        self.inline_selections.clear();
        self.pending_inline_selection = selection;
        self.message = AppMessage::Info("Cleared multi-cursor selections".to_owned());
    }

    fn show_menu_bar(&mut self, context: &egui::Context) {
        let frame = Frame::default()
            .fill(VSCODE_PANEL)
            .inner_margin(Margin::symmetric(8, 4))
            .stroke(Stroke::new(1.0, VSCODE_BORDER));

        egui::TopBottomPanel::top("menu_bar")
            .frame(frame)
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    egui::menu::bar(ui, |ui| {
                        ui.menu_button("File", |ui| {
                            if ui.button("New").clicked() {
                                self.new_document();
                                ui.close_menu();
                            }
                            if ui.button("Open...").clicked() {
                                self.open_document_with_dialog();
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Save").clicked() {
                                self.save_document();
                                ui.close_menu();
                            }
                            if ui.button("Save As...").clicked() {
                                self.save_document_with_dialog();
                                ui.close_menu();
                            }
                        });

                        ui.menu_button("Edit", |ui| {
                            if ui.button("Find...").clicked() {
                                self.open_search_panel();
                                ui.close_menu();
                            }
                            if ui.button("Go to Line...").clicked() {
                                self.open_go_to_line();
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Add Cursor to Next Match").clicked() {
                                self.add_next_occurrence_selection();
                                ui.close_menu();
                            }
                            if ui.button("Select All Occurrences").clicked() {
                                self.select_all_occurrences();
                                ui.close_menu();
                            }
                            if ui.button("Select Current Lines").clicked() {
                                self.select_current_lines();
                                ui.close_menu();
                            }
                            if ui.button("Clear Multi-Cursor").clicked() {
                                self.clear_multi_cursor();
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Rectangular Selection").clicked() {
                                self.create_rectangular_selection();
                                ui.close_menu();
                            }
                            if ui.button("Copy Rectangle").clicked() {
                                self.copy_rectangular_selection();
                                ui.close_menu();
                            }
                            if ui.button("Paste Rectangle").clicked() {
                                self.paste_rectangular_selection();
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Move Line Up").clicked() {
                                self.move_selected_lines(true);
                                ui.close_menu();
                            }
                            if ui.button("Move Line Down").clicked() {
                                self.move_selected_lines(false);
                                ui.close_menu();
                            }
                            if ui.button("Copy Line Up").clicked() {
                                self.copy_selected_lines(true);
                                ui.close_menu();
                            }
                            if ui.button("Copy Line Down").clicked() {
                                self.copy_selected_lines(false);
                                ui.close_menu();
                            }
                            if ui.button("Delete Line").clicked() {
                                self.delete_selected_lines();
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Uppercase").clicked() {
                                self.convert_selected_case(CaseTransform::Upper);
                                ui.close_menu();
                            }
                            if ui.button("Lowercase").clicked() {
                                self.convert_selected_case(CaseTransform::Lower);
                                ui.close_menu();
                            }
                        });

                        ui.menu_button("View", |ui| {
                            ui.checkbox(&mut self.sidebar_visible, "Show Sidebar");
                            ui.checkbox(&mut self.wrap_lines, "Wrap Lines");
                            ui.separator();
                            if ui.button("Zoom In").clicked() {
                                self.zoom_editor(1.1);
                                ui.close_menu();
                            }
                            if ui.button("Zoom Out").clicked() {
                                self.zoom_editor(1.0 / 1.1);
                                ui.close_menu();
                            }
                            if ui.button("Reset Zoom").clicked() {
                                self.reset_editor_zoom();
                                ui.close_menu();
                            }
                        });

                        ui.menu_button("Help", |ui| {
                            if ui.button("Keyboard Shortcuts").clicked() {
                                self.show_help = true;
                                ui.close_menu();
                            }
                        });
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(self.document.window_title())
                                .color(VSCODE_TEXT_DIM)
                                .size(12.0),
                        );
                    });
                });
            });
    }

    fn show_activity_bar(&mut self, context: &egui::Context) {
        let frame = Frame::default()
            .fill(VSCODE_ACTIVITY)
            .inner_margin(Margin::symmetric(0, 8));

        egui::SidePanel::left("activity_bar")
            .frame(frame)
            .exact_width(48.0)
            .resizable(false)
            .show(context, |ui| {
                ui.vertical_centered(|ui| {
                    self.activity_button(ui, SidebarView::Explorer, "E", "Explorer");
                    self.activity_button(ui, SidebarView::Search, "S", "Search");
                    self.activity_button(ui, SidebarView::Outline, "O", "Outline");
                });
            });
    }

    fn activity_button(
        &mut self,
        ui: &mut egui::Ui,
        view: SidebarView,
        label: &str,
        tooltip: &str,
    ) {
        let active = self.sidebar_visible && self.active_view == view;
        let color = if active { VSCODE_TEXT } else { VSCODE_TEXT_DIM };
        let (rect, response) = ui.allocate_exact_size(Vec2::new(40.0, 40.0), Sense::click());

        if active {
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left(), rect.top()),
                    egui::pos2(rect.left(), rect.bottom()),
                ],
                Stroke::new(2.0, VSCODE_TEXT),
            );
        }

        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::new(16.0, FontFamily::Proportional),
            color,
        );

        if response.on_hover_text(tooltip).clicked() {
            if active {
                self.sidebar_visible = false;
            } else {
                self.sidebar_visible = true;
                self.active_view = view;
            }
        }

        ui.add_space(4.0);
    }

    fn show_sidebar(&mut self, context: &egui::Context) {
        let frame = Frame::default()
            .fill(VSCODE_PANEL)
            .inner_margin(Margin::ZERO);

        egui::SidePanel::left("sidebar")
            .frame(frame)
            .default_width(280.0)
            .min_width(200.0)
            .resizable(true)
            .show(context, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(self.active_view.title())
                            .color(VSCODE_TEXT_DIM)
                            .size(11.0)
                            .strong(),
                    );
                });
                ui.add_space(4.0);
                ui.separator();

                match self.active_view {
                    SidebarView::Explorer => self.show_explorer(ui),
                    SidebarView::Search => self.show_search(ui),
                    SidebarView::Outline => self.show_outline(ui),
                }
            });
    }

    fn show_explorer(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        sidebar_heading(ui, "OPEN EDITOR");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(RichText::new(if self.document.is_dirty() {
                "*"
            } else {
                "-"
            }));
            ui.label(RichText::new(self.document.display_name()).color(VSCODE_TEXT));
        });

        ui.add_space(16.0);
        sidebar_heading(ui, "FILE PATH");
        ui.add_space(4.0);
        let path_text = self
            .document
            .path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "(unsaved)".to_owned());
        sidebar_wrapped_path(ui, &path_text);

        ui.add_space(18.0);
        sidebar_heading(ui, "ACTIONS");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.vertical(|ui| {
                if ui.button("Open File...").clicked() {
                    self.open_document_with_dialog();
                }
                if ui.button("Save").clicked() {
                    self.save_document();
                }
                if ui.button("Save As...").clicked() {
                    self.save_document_with_dialog();
                }
            });
        });

        if matches!(self.document, ActiveDocument::Large(_)) {
            ui.add_space(18.0);
            sidebar_heading(ui, "LARGE FILE VIEWPORT");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                if ui.button("Previous").clicked() {
                    self.move_large_viewport_previous();
                }
                if ui.button("Next").clicked() {
                    self.move_large_viewport_next();
                }
            });
        }
    }

    fn show_search(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.vertical(|ui| {
                let query_response = ui.add(
                    TextEdit::singleline(&mut self.search.query)
                        .hint_text("Search")
                        .desired_width(ui.available_width()),
                );
                let enter_pressed = ui.input(|input| input.key_pressed(egui::Key::Enter));

                ui.add_space(4.0);
                ui.add(
                    TextEdit::singleline(&mut self.search.replacement)
                        .hint_text("Replace")
                        .desired_width(ui.available_width()),
                );

                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.search.match_case, "Match Case");
                    ui.checkbox(&mut self.search.whole_word, "Whole Word");
                    ui.checkbox(&mut self.search.use_regex, "Regex");
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Find").clicked() || (query_response.lost_focus() && enter_pressed)
                    {
                        self.run_search();
                    }

                    let replace_enabled = matches!(self.document, ActiveDocument::Inline(_));

                    if ui
                        .add_enabled(replace_enabled, egui::Button::new("Replace All"))
                        .clicked()
                    {
                        self.replace_all_inline_matches();
                    }
                });

                ui.add_space(10.0);
                self.show_search_results(ui);
            });
        });
    }

    fn show_search_results(&self, ui: &mut egui::Ui) {
        if let Some(error) = &self.search.error {
            ui.label(RichText::new(error).color(VSCODE_STATUS_ERROR));
            return;
        }

        let scope = self.search.searched_scope.unwrap_or("document");
        ui.label(
            RichText::new(format!(
                "{} result{} in {scope}",
                self.search.results.len(),
                if self.search.results.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ))
            .color(VSCODE_TEXT_DIM),
        );
        ui.add_space(6.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for result in &self.search.results {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{}", result.line_number))
                                .color(VSCODE_TEXT_DIM)
                                .monospace(),
                        );
                        ui.label(RichText::new(&result.preview).color(VSCODE_TEXT));
                    });
                    ui.add_space(3.0);
                }
            });
    }

    fn run_search(&mut self) {
        let query = self.search.query.clone();
        let options = self.search.options();
        let (scope, base_line, text) = match &self.document {
            ActiveDocument::Inline(document) => ("document", 0, document.text()),
            ActiveDocument::Large(document) => (
                "current viewport",
                document.viewport_start_line(),
                document.viewport_text(),
            ),
        };

        match find_text_matches(text, &query, options, SEARCH_RESULT_LIMIT) {
            Ok(matches) => {
                self.search.results = matches
                    .into_iter()
                    .map(|search_match| SearchResultRow {
                        line_number: base_line + search_match.line_index + 1,
                        preview: line_preview(text, search_match.range.start),
                    })
                    .collect();
                self.search.error = None;
                self.search.searched_scope = Some(scope);
                self.message = AppMessage::Info(format!(
                    "Found {} result{}",
                    self.search.results.len(),
                    if self.search.results.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                ));
            }
            Err(error) => {
                self.search.results.clear();
                self.search.error = Some(error.to_string());
                self.search.searched_scope = Some(scope);
                self.message = AppMessage::Error(format!("Search failed: {error}"));
            }
        }
    }

    fn replace_all_inline_matches(&mut self) {
        let ActiveDocument::Inline(document) = &mut self.document else {
            self.message =
                AppMessage::Error("Replace All is available for inline documents".to_owned());
            return;
        };
        let query = self.search.query.clone();
        let replacement = self.search.replacement.clone();
        let options = self.search.options();

        match search_replace_all(document.text(), &query, &replacement, options) {
            Ok((text, count)) => {
                if count > 0 {
                    document.replace_text(text);
                }

                self.message = AppMessage::Info(format!(
                    "Replaced {count} match{}",
                    if count == 1 { "" } else { "es" }
                ));
                self.run_search();
            }
            Err(error) => {
                self.search.error = Some(error.to_string());
                self.message = AppMessage::Error(format!("Replace failed: {error}"));
            }
        }
    }

    fn show_outline(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);

        for (label, value) in self.outline_rows() {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new(label).color(VSCODE_TEXT_DIM));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new(value).color(VSCODE_TEXT).monospace());
                });
            });
            ui.add_space(4.0);
        }
    }

    fn outline_rows(&self) -> Vec<(&'static str, String)> {
        match &self.document {
            ActiveDocument::Inline(document) => {
                let metrics = document.metrics();
                vec![
                    ("Mode", self.document.mode_label().to_owned()),
                    ("Encoding", document.encoding().label().to_owned()),
                    ("Line endings", document.line_ending().label().to_owned()),
                    ("Lines", metrics.visual_lines.to_string()),
                    ("Characters", metrics.characters.to_string()),
                    ("Bytes", metrics.bytes.to_string()),
                ]
            }
            ActiveDocument::Large(document) => {
                vec![
                    ("Mode", self.document.mode_label().to_owned()),
                    ("File bytes", document.bytes().to_string()),
                    ("File lines", document.file_line_count().to_string()),
                    ("Window start", document.viewport().start_byte().to_string()),
                    ("Window end", document.viewport().end_byte().to_string()),
                    (
                        "Window lines",
                        format!(
                            "{}..{}",
                            document.viewport_start_line() + 1,
                            document.viewport_end_line()
                        ),
                    ),
                    ("Window bytes", document.viewport_text().len().to_string()),
                ]
            }
        }
    }

    fn show_tab_bar(&mut self, context: &egui::Context) {
        let frame = Frame::default().fill(VSCODE_TAB).inner_margin(Margin::ZERO);

        egui::TopBottomPanel::top("tab_bar")
            .frame(frame)
            .exact_height(34.0)
            .show(context, |ui| {
                let dirty_marker = if self.document.is_dirty() { " *" } else { "" };
                let label = format!("{}{dirty_marker}", self.document.display_name());
                let (rect, _) = ui.allocate_exact_size(Vec2::new(240.0, 34.0), Sense::hover());

                ui.painter()
                    .rect_filled(rect, CornerRadius::ZERO, VSCODE_BG);
                ui.painter().line_segment(
                    [rect.left_top(), rect.right_top()],
                    Stroke::new(1.0, VSCODE_ACCENT),
                );
                ui.painter().text(
                    egui::pos2(rect.left() + 14.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::new(13.0, FontFamily::Monospace),
                    VSCODE_TEXT,
                );
            });
    }

    fn show_editor(&mut self, context: &egui::Context) {
        let frame = Frame::default().fill(VSCODE_BG).inner_margin(Margin::ZERO);

        egui::CentralPanel::default()
            .frame(frame)
            .show(context, |ui| {
                if self.open_receiver.is_some()
                    || self.save_receiver.is_some()
                    || self.viewport_receiver.is_some()
                {
                    let label = if self.open_receiver.is_some() {
                        "Opening file..."
                    } else if self.save_receiver.is_some() {
                        "Saving file..."
                    } else {
                        "Loading window..."
                    };

                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new(label).color(VSCODE_TEXT_DIM));
                    });
                    return;
                }

                let editor_font_size = self.editor_font_size;
                let search_query = self.search.query.clone();
                let search_options = self.search.options();
                let current_inline_selection_snapshot = self.current_inline_selection;
                let inline_selections_snapshot = self.inline_selections.clone();
                let pending_inline_selection = &mut self.pending_inline_selection;
                let current_inline_selection = &mut self.current_inline_selection;
                let large_editor_action = match &mut self.document {
                    ActiveDocument::Inline(document) => {
                        let wrap_lines = self.wrap_lines;
                        let available_width = ui.available_width();
                        let editor_width = editor_content_width_for_text(
                            document.text(),
                            available_width,
                            wrap_lines,
                            editor_font_size,
                        );
                        let editor_min_height = inline_editor_min_height(
                            ui.available_height(),
                            document.metrics().visual_lines,
                            editor_font_size,
                        );
                        let scroll_area = if wrap_lines {
                            egui::ScrollArea::vertical()
                        } else {
                            egui::ScrollArea::both()
                        };

                        scroll_area.auto_shrink([false, false]).show(ui, |ui| {
                            ui.set_min_width(editor_width);
                            let editor_id = ui.make_persistent_id(INLINE_EDITOR_ID_SOURCE);
                            let focus_editor = apply_inline_selection_request(
                                ui.ctx(),
                                editor_id,
                                pending_inline_selection.take(),
                            );
                            let mut layouter = |ui: &egui::Ui, text: &str, wrap_width: f32| {
                                ui.fonts(|fonts| {
                                    fonts.layout_job(editor_highlight_layout_job(
                                        text,
                                        wrap_width,
                                        editor_font_size,
                                        &search_query,
                                        search_options,
                                        current_inline_selection_snapshot,
                                        &inline_selections_snapshot,
                                    ))
                                })
                            };
                            let editor = editor_widget(
                                document.text_mut(),
                                editor_width,
                                editor_min_height,
                                editor_font_size,
                            )
                            .id(editor_id)
                            .layouter(&mut layouter);

                            let output = editor.show(ui);

                            if focus_editor {
                                output.response.request_focus();
                            }

                            if output.response.changed() {
                                document.record_text_change();
                            }

                            if let Some(cursor_range) = output.cursor_range {
                                let cursor_range = cursor_range.as_ccursor_range();
                                *current_inline_selection = Some(TextSelection::new(
                                    cursor_range.secondary.index,
                                    cursor_range.primary.index,
                                ));
                            }
                        });
                        None
                    }
                    ActiveDocument::Large(document) => show_large_virtual_editor(
                        ui,
                        document,
                        self.wrap_lines,
                        editor_font_size,
                        &self.search.query,
                        self.search.options(),
                        &mut self.wrap_line_heights,
                    ),
                };

                if let Some(action) = large_editor_action {
                    self.apply_large_editor_action(action);
                }
            });
    }

    fn show_go_to_line_window(&mut self, context: &egui::Context) {
        if !self.go_to_line.visible {
            return;
        }

        let mut open = self.go_to_line.visible;
        let mut submit = false;
        let mut cancel = false;
        let request_focus = self.go_to_line.request_focus;

        egui::Window::new("Go to Line")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(context, |ui| {
                ui.set_min_width(260.0);
                ui.label(
                    RichText::new(format!("Line 1..{}", self.document_line_count()))
                        .color(VSCODE_TEXT_DIM),
                );
                let response = ui.add(
                    TextEdit::singleline(&mut self.go_to_line.input)
                        .desired_width(220.0)
                        .id(ui.make_persistent_id("go_to_line_input"))
                        .hint_text("Line number"),
                );

                if request_focus {
                    response.request_focus();
                }

                let enter_pressed = ui.input(|input| input.key_pressed(egui::Key::Enter));

                if response.lost_focus() && enter_pressed {
                    submit = true;
                }

                if let Some(error) = &self.go_to_line.error {
                    ui.label(RichText::new(error).color(VSCODE_STATUS_ERROR));
                }

                ui.horizontal(|ui| {
                    if ui.button("Go").clicked() {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.go_to_line.error = None;
                        cancel = true;
                    }
                });
            });

        if cancel {
            open = false;
        }

        self.go_to_line.visible = open;
        self.go_to_line.request_focus = false;

        if submit {
            self.submit_go_to_line(context);
        }
    }

    fn show_help_window(&mut self, context: &egui::Context) {
        if !self.show_help {
            return;
        }

        let mut open = self.show_help;

        egui::Window::new("Keyboard Shortcuts")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(context, |ui| {
                ui.set_min_width(420.0);
                help_row(ui, "New", "Cmd/Ctrl+N");
                help_row(ui, "Open", "Cmd/Ctrl+O or drop a file");
                help_row(ui, "Save", "Cmd/Ctrl+S");
                help_row(ui, "Save As", "Cmd/Ctrl+Shift+S");
                help_row(ui, "Find", "Cmd/Ctrl+F, F3");
                help_row(ui, "Go to Line", "Cmd/Ctrl+G");
                help_row(ui, "Add Cursor", "Cmd/Ctrl+D");
                help_row(ui, "Select Occurrences", "Cmd/Ctrl+Shift+L");
                help_row(ui, "Select Lines", "Cmd/Ctrl+L");
                help_row(ui, "Move/Copy Lines", "Alt+Up/Down, Alt+Shift+Up/Down");
                help_row(ui, "Delete Line", "Cmd/Ctrl+Shift+K");
                help_row(ui, "Case", "Cmd/Ctrl+U, Cmd/Ctrl+Shift+U");
                help_row(ui, "Rectangle", "Alt+Shift+R/C/V");
                help_row(ui, "Wrap Lines", "Alt+Z");
                help_row(ui, "Zoom", "Cmd/Ctrl+Plus, Minus, 0");
                help_row(ui, "Help", "F1");
            });

        self.show_help = open;
    }

    fn show_status_bar(&mut self, context: &egui::Context) {
        let background = if self.message.is_error() {
            VSCODE_STATUS_ERROR
        } else if self.document.is_dirty() {
            VSCODE_STATUS_DIRTY
        } else {
            VSCODE_STATUS
        };
        let frame = Frame::default()
            .fill(background)
            .inner_margin(Margin::symmetric(10, 3));

        egui::TopBottomPanel::bottom("status_bar")
            .frame(frame)
            .exact_height(24.0)
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(status_text(self.document.display_name()));
                    ui.add_space(12.0);

                    match &self.document {
                        ActiveDocument::Inline(document) => {
                            let metrics = document.metrics();
                            ui.label(status_text(format!("Ln {}", metrics.visual_lines)));
                            ui.add_space(8.0);
                            ui.label(status_text(format!("{} bytes", metrics.bytes)));
                        }
                        ActiveDocument::Large(document) => {
                            ui.label(status_text(format!(
                                "Ln {}..{} / {}  |  {}..{} / {} bytes",
                                document.viewport_start_line() + 1,
                                document.viewport_end_line(),
                                document.file_line_count(),
                                document.viewport().start_byte(),
                                document.viewport().end_byte(),
                                document.bytes()
                            )));
                        }
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(status_text(self.document.mode_label()));
                        ui.add_space(12.0);
                        ui.label(status_text(format!("{}px", self.editor_font_size)));
                        ui.add_space(12.0);
                        if !self.inline_selections.is_empty() {
                            ui.label(status_text(format!(
                                "{} selections",
                                self.inline_selections.len()
                            )));
                            ui.add_space(12.0);
                        }
                        ui.label(status_text(self.message.text()));
                    });
                });
            });
    }

    fn new_document(&mut self) {
        if !self.can_replace_document("creating a new document") {
            return;
        }

        self.open_receiver = None;
        self.save_receiver = None;
        self.viewport_receiver = None;
        self.wrap_line_heights.clear();
        self.clear_inline_edit_state();
        self.document = ActiveDocument::default();
        self.path_input.clear();
        self.message = AppMessage::Info("New document".to_owned());
    }

    fn open_document_with_dialog(&mut self) {
        if !self.can_replace_document("opening another file") {
            return;
        }

        match self.open_file_dialog().pick_file() {
            Some(path) => self.open_document_at(path),
            None => self.message = AppMessage::Info("Open canceled".to_owned()),
        }
    }

    fn open_document_at(&mut self, path: PathBuf) {
        let (sender, receiver) = mpsc::channel();
        let task_path = path.clone();

        self.open_receiver = Some(receiver);
        self.path_input = path.display().to_string();
        self.message = AppMessage::Info(format!("Opening {}", path.display()));

        thread::spawn(move || {
            let result = match open_text_document(&task_path) {
                Ok(document) => OpenTaskResult::Opened(document),
                Err(error) => OpenTaskResult::Failed {
                    path: task_path,
                    error: error.to_string(),
                },
            };

            let _ = sender.send(result);
        });
    }

    fn save_document(&mut self) {
        if !self.can_start_save() {
            return;
        }

        if let Some(path) = self.save_target_path() {
            self.save_document_at(path);
        } else {
            self.save_document_with_dialog();
        }
    }

    fn save_document_with_dialog(&mut self) {
        if !self.can_start_save() {
            return;
        }

        match self.save_file_dialog().save_file() {
            Some(path) => self.save_document_at(path),
            None => self.message = AppMessage::Info("Save canceled".to_owned()),
        }
    }

    fn save_document_at(&mut self, path: PathBuf) {
        if !self.can_start_save() {
            return;
        }

        let (sender, receiver) = mpsc::channel();
        let mut document = self.document.clone();
        let task_path = path.clone();

        self.save_receiver = Some(receiver);
        self.path_input = path.display().to_string();
        self.message = AppMessage::Info(format!("Saving {}", path.display()));

        thread::spawn(move || {
            let result = match &mut document {
                ActiveDocument::Inline(document) => save_document(document, &task_path),
                ActiveDocument::Large(document) => save_large_document(document, &task_path),
            };

            let message = match result {
                Ok(()) => SaveTaskResult::Saved {
                    document: Box::new(document),
                    path: task_path,
                },
                Err(error) => SaveTaskResult::Failed {
                    path: task_path,
                    error: error.to_string(),
                },
            };

            let _ = sender.send(message);
        });
    }

    fn can_start_save(&mut self) -> bool {
        if self.open_receiver.is_some() {
            self.message = AppMessage::Error("Wait for the current open task to finish".to_owned());
            false
        } else if self.save_receiver.is_some() {
            self.message = AppMessage::Error("Wait for the current save task to finish".to_owned());
            false
        } else if self.viewport_receiver.is_some() {
            self.message =
                AppMessage::Error("Wait for the current viewport task to finish".to_owned());
            false
        } else {
            true
        }
    }

    fn move_large_viewport_next(&mut self) {
        self.start_large_viewport_move(ViewportMove::Next);
    }

    fn move_large_viewport_previous(&mut self) {
        self.start_large_viewport_move(ViewportMove::Previous);
    }

    fn move_large_viewport_to_line(&mut self, line_index: usize) {
        self.start_large_viewport_move(ViewportMove::Line(line_index));
    }

    fn apply_large_editor_action(&mut self, action: LargeEditorAction) {
        match action {
            LargeEditorAction::Message(message) => {
                self.wrap_line_heights.invalidate_measurements();
                self.message = message;
            }
            LargeEditorAction::LoadLine(line_index) => {
                if self.wrap_lines {
                    self.wrap_line_heights.scroll_anchor_line = Some(line_index);
                }

                self.move_large_viewport_to_line(line_index);
            }
        }
    }

    fn start_large_viewport_move(&mut self, viewport_move: ViewportMove) {
        if !self.can_start_viewport_move() {
            return;
        }

        let ActiveDocument::Large(document) = &self.document else {
            return;
        };

        let (sender, receiver) = mpsc::channel();
        let mut task_document = document.clone();
        let direction_label = match viewport_move {
            ViewportMove::Previous => "previous",
            ViewportMove::Next => "next",
            ViewportMove::Line(_) => "selected",
        };

        self.viewport_receiver = Some(receiver);
        self.message = AppMessage::Info(format!("Loading {direction_label} window"));

        thread::spawn(move || {
            let result = match viewport_move {
                ViewportMove::Previous => task_document.load_previous_viewport(),
                ViewportMove::Next => task_document.load_next_viewport(),
                ViewportMove::Line(line_index) => task_document.load_viewport_for_line(line_index),
            };
            let message = match result {
                Ok(()) => ViewportTaskResult::Loaded(task_document),
                Err(error) => ViewportTaskResult::Failed(error.to_string()),
            };

            let _ = sender.send(message);
        });
    }

    fn can_start_viewport_move(&mut self) -> bool {
        if self.open_receiver.is_some() {
            self.message = AppMessage::Error("Wait for the current open task to finish".to_owned());
            false
        } else if self.save_receiver.is_some() {
            self.message = AppMessage::Error("Wait for the current save task to finish".to_owned());
            false
        } else if self.viewport_receiver.is_some() {
            self.message =
                AppMessage::Error("Wait for the current viewport task to finish".to_owned());
            false
        } else if self.document.is_dirty() {
            self.message = AppMessage::Error("Save the current viewport before moving".to_owned());
            false
        } else {
            true
        }
    }

    fn open_file_dialog(&self) -> FileDialog {
        self.file_dialog()
    }

    fn save_file_dialog(&self) -> FileDialog {
        self.file_dialog()
            .set_file_name(self.save_dialog_file_name())
    }

    fn file_dialog(&self) -> FileDialog {
        let dialog = FileDialog::new();

        if let Some(directory) = self.dialog_start_directory() {
            dialog.set_directory(directory)
        } else {
            dialog
        }
    }

    fn dialog_start_directory(&self) -> Option<PathBuf> {
        self.path_from_input()
            .as_deref()
            .and_then(parent_directory)
            .or_else(|| self.document.path().and_then(parent_directory))
    }

    fn save_dialog_file_name(&self) -> String {
        self.document
            .path()
            .and_then(Path::file_name)
            .and_then(|file_name| file_name.to_str())
            .filter(|file_name| !file_name.is_empty())
            .unwrap_or("Untitled.txt")
            .to_owned()
    }

    fn path_from_input(&self) -> Option<PathBuf> {
        let trimmed_path = self.path_input.trim();

        if trimmed_path.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed_path))
        }
    }

    fn save_target_path(&self) -> Option<PathBuf> {
        self.document
            .path()
            .map(Path::to_path_buf)
            .or_else(|| self.path_from_input())
    }

    fn can_replace_document(&mut self, action: &str) -> bool {
        if self.open_receiver.is_some() {
            self.message = AppMessage::Error("Wait for the current open task to finish".to_owned());
            return false;
        }

        if self.save_receiver.is_some() {
            self.message = AppMessage::Error("Wait for the current save task to finish".to_owned());
            return false;
        }

        if self.viewport_receiver.is_some() {
            self.message =
                AppMessage::Error("Wait for the current viewport task to finish".to_owned());
            return false;
        }

        if self.document.is_dirty() {
            self.message = AppMessage::Error(format!("Save the current document before {action}"));
            false
        } else {
            true
        }
    }

    fn should_block_close(&self) -> bool {
        self.document.is_dirty()
            || self.open_receiver.is_some()
            || self.save_receiver.is_some()
            || self.viewport_receiver.is_some()
    }

    fn guard_close_request(&mut self, context: &egui::Context) {
        if context.input(|input| input.viewport().close_requested()) && self.should_block_close() {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.message = AppMessage::Error("Save or wait before closing".to_owned());
        }
    }
}

fn show_large_virtual_editor(
    ui: &mut egui::Ui,
    document: &mut LargeDocument,
    wrap_lines: bool,
    editor_font_size: f32,
    search_query: &str,
    search_options: SearchOptions,
    wrap_line_heights: &mut WrapLineHeightCache,
) -> Option<LargeEditorAction> {
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(
            RichText::new(format!(
                "Lines {}..{} of {}  |  Window {}..{} of {} bytes",
                document.viewport_start_line() + 1,
                document.viewport_end_line(),
                document.file_line_count(),
                document.viewport().start_byte(),
                document.viewport().end_byte(),
                document.bytes()
            ))
            .color(VSCODE_TEXT_DIM),
        );
        ui.add_space(12.0);
        ui.label(
            RichText::new(format!(
                "{} visible window lines",
                document.viewport_line_count()
            ))
            .color(VSCODE_TEXT_DIM),
        );
    });
    ui.add_space(4.0);

    let available_width = ui.available_width();
    let content_width = editor_content_width_for_text(
        document.viewport_text(),
        available_width,
        wrap_lines,
        editor_font_size,
    )
    .max(available_width);
    let text_width = editor_text_width(content_width, wrap_lines);
    let line_count = document.file_line_count().max(1);
    let row_height = editor_row_height(editor_font_size);
    let mut action = None;

    if wrap_lines {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                show_wrapped_large_virtual_rows(
                    ui,
                    document,
                    WrappedRowsGeometry {
                        viewport,
                        line_count,
                        content_width,
                        text_width,
                        row_height,
                        editor_font_size,
                    },
                    WrappedRowsState {
                        cache: wrap_line_heights,
                        action: &mut action,
                    },
                );
            });

        return action;
    }

    ui.scope(|ui| {
        configure_large_non_wrapped_row_spacing(ui);

        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show_rows(ui, row_height, line_count, |ui, row_range| {
                ui.set_min_width(content_width);

                if action.is_none() {
                    if let Some(line_index) =
                        first_missing_visible_line(row_range.clone(), |line_index| {
                            document.contains_file_line(line_index)
                        })
                    {
                        action = Some(LargeEditorAction::LoadLine(line_index));
                    }
                }

                if !document.contains_file_line(row_range.start) {
                    for line_index in row_range {
                        show_loading_line(ui, line_index, row_height, text_width);
                    }

                    return;
                }

                if document.contains_file_line_range(row_range.clone()) {
                    action = show_virtual_line_block(
                        ui,
                        document,
                        row_range,
                        LargeLineBlockView {
                            row_height,
                            text_width,
                            editor_font_size,
                            search_query,
                            search_options,
                        },
                    );

                    return;
                }

                for line_index in row_range {
                    if !document.contains_file_line(line_index) {
                        show_loading_line(ui, line_index, row_height, text_width);
                        continue;
                    }

                    if let Some(line_message) = show_virtual_line(
                        ui,
                        document,
                        line_index,
                        row_height,
                        text_width,
                        wrap_lines,
                        editor_font_size,
                    ) {
                        action = Some(LargeEditorAction::Message(line_message));
                    }
                }
            });
    });

    action
}

fn configure_large_non_wrapped_row_spacing(ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing.y = 0.0;
}

struct WrappedRowsGeometry {
    viewport: egui::Rect,
    line_count: usize,
    content_width: f32,
    text_width: f32,
    row_height: f32,
    editor_font_size: f32,
}

struct WrappedRowsState<'a> {
    cache: &'a mut WrapLineHeightCache,
    action: &'a mut Option<LargeEditorAction>,
}

#[derive(Debug, Clone, Copy)]
struct WrappedLineMeasurement {
    text_width: f32,
    row_height: f32,
    editor_font_size: f32,
}

fn show_wrapped_large_virtual_rows(
    ui: &mut egui::Ui,
    document: &mut LargeDocument,
    geometry: WrappedRowsGeometry,
    state: WrappedRowsState<'_>,
) {
    reset_line_height_extras_if_layout_changed(
        &mut state.cache.extras,
        &mut state.cache.measured_lines,
        &mut state.cache.text_width,
        &mut state.cache.line_range,
        geometry.text_width,
        document.viewport_start_line()..document.viewport_end_line(),
    );
    let measurement_line_range = wrapped_measurement_line_range(
        geometry.viewport,
        geometry.line_count,
        geometry.row_height,
        &state.cache.extras,
    );

    measure_wrapped_line_height_extras(
        ui,
        document,
        WrappedLineMeasurement {
            text_width: geometry.text_width,
            row_height: geometry.row_height,
            editor_font_size: geometry.editor_font_size,
        },
        measurement_line_range,
        &mut state.cache.extras,
        &mut state.cache.measured_lines,
    );

    let total_height = virtual_editor_total_height(
        geometry.line_count,
        geometry.row_height,
        &state.cache.extras,
    );

    ui.set_height(total_height);
    ui.set_min_width(geometry.content_width);

    let anchored_this_frame = if let Some(anchor_line_index) = state.cache.scroll_anchor_line.take()
    {
        if anchor_line_index < geometry.line_count {
            let anchor_top =
                virtual_line_top(anchor_line_index, geometry.row_height, &state.cache.extras);
            let anchor_height =
                virtual_line_height(anchor_line_index, geometry.row_height, &state.cache.extras);
            let anchor_rect = egui::Rect::from_min_size(
                egui::pos2(ui.max_rect().left(), ui.max_rect().top() + anchor_top),
                Vec2::new(geometry.content_width, anchor_height),
            );

            ui.scroll_to_rect(anchor_rect, Some(Align::Min));
        }
        true
    } else {
        false
    };

    let mut line_index = line_index_at_virtual_offset(
        geometry.viewport.min.y,
        geometry.line_count,
        geometry.row_height,
        &state.cache.extras,
    );

    let mut line_top = virtual_line_top(line_index, geometry.row_height, &state.cache.extras);

    while line_index < geometry.line_count {
        if line_top > geometry.viewport.max.y {
            break;
        }

        let line_height = virtual_line_height(line_index, geometry.row_height, &state.cache.extras);
        let row_rect = egui::Rect::from_min_size(
            egui::pos2(ui.max_rect().left(), ui.max_rect().top() + line_top),
            Vec2::new(geometry.content_width, line_height),
        );

        with_positioned_row_ui(ui, row_rect, |row_ui| {
            row_ui.set_min_width(geometry.content_width);

            if !document.contains_file_line(line_index) {
                if state.action.is_none() && !anchored_this_frame {
                    *state.action = Some(LargeEditorAction::LoadLine(line_index));
                }

                show_loading_line(row_ui, line_index, line_height, geometry.text_width);
            } else if let Some(line_message) = show_virtual_line(
                row_ui,
                document,
                line_index,
                line_height,
                geometry.text_width,
                true,
                geometry.editor_font_size,
            ) {
                *state.action = Some(LargeEditorAction::Message(line_message));
            }
        });

        line_top += line_height;
        line_index += 1;
    }
}

fn with_positioned_row_ui<R>(
    ui: &mut egui::Ui,
    row_rect: egui::Rect,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let mut row_ui = ui.new_child(egui::UiBuilder::new().max_rect(row_rect));

    add_contents(&mut row_ui)
}

fn measure_wrapped_line_height_extras(
    ui: &egui::Ui,
    document: &LargeDocument,
    measurement: WrappedLineMeasurement,
    line_range: Range<usize>,
    line_height_extras: &mut BTreeMap<usize, f32>,
    measured_lines: &mut BTreeSet<usize>,
) {
    for line_index in line_range {
        if measured_lines.contains(&line_index) {
            continue;
        }

        let Some(line_text) = document.file_line_text(line_index) else {
            continue;
        };
        let line_height = editor_line_row_height_for_ui(
            ui,
            line_text,
            measurement.text_width,
            measurement.row_height,
            measurement.editor_font_size,
        );

        update_line_height_extra(
            line_height_extras,
            line_index,
            line_height,
            measurement.row_height,
        );
        measured_lines.insert(line_index);
    }
}

fn wrapped_measurement_line_range(
    viewport: egui::Rect,
    line_count: usize,
    row_height: f32,
    line_height_extras: &BTreeMap<usize, f32>,
) -> Range<usize> {
    let Some(last_line_index) = line_count.checked_sub(1) else {
        return 0..0;
    };
    let start =
        line_index_at_virtual_offset(viewport.min.y, line_count, row_height, line_height_extras)
            .saturating_sub(WRAP_MEASURE_OVERSCAN_LINES);
    let end_line =
        line_index_at_virtual_offset(viewport.max.y, line_count, row_height, line_height_extras);
    let end = end_line
        .saturating_add(WRAP_MEASURE_OVERSCAN_LINES + 1)
        .min(line_count)
        .max((start + 1).min(last_line_index + 1));

    start..end
}

fn reset_line_height_extras_if_layout_changed(
    line_height_extras: &mut BTreeMap<usize, f32>,
    measured_lines: &mut BTreeSet<usize>,
    cached_text_width: &mut Option<f32>,
    cached_line_range: &mut Option<Range<usize>>,
    text_width: f32,
    line_range: Range<usize>,
) -> bool {
    let width_changed = cached_text_width
        .map(|cached_text_width| cached_text_width.round() != text_width.round())
        .unwrap_or(true);
    let range_changed = cached_line_range
        .as_ref()
        .map(|cached_line_range| cached_line_range != &line_range)
        .unwrap_or(true);

    if width_changed || range_changed {
        line_height_extras.clear();
        measured_lines.clear();
        *cached_text_width = Some(text_width);
        *cached_line_range = Some(line_range);

        true
    } else {
        false
    }
}

fn update_line_height_extra(
    line_height_extras: &mut BTreeMap<usize, f32>,
    line_index: usize,
    line_height: f32,
    row_height: f32,
) {
    let extra_height = line_height - row_height;

    if extra_height > 0.0 {
        line_height_extras.insert(line_index, extra_height);
    } else {
        line_height_extras.remove(&line_index);
    }
}

fn virtual_editor_total_height(
    line_count: usize,
    row_height: f32,
    line_height_extras: &BTreeMap<usize, f32>,
) -> f32 {
    line_count as f32 * row_height + line_height_extras.values().copied().sum::<f32>()
}

fn virtual_line_top(
    line_index: usize,
    row_height: f32,
    line_height_extras: &BTreeMap<usize, f32>,
) -> f32 {
    line_index as f32 * row_height
        + line_height_extras
            .range(..line_index)
            .map(|(_line_index, extra_height)| extra_height)
            .copied()
            .sum::<f32>()
}

fn virtual_line_height(
    line_index: usize,
    row_height: f32,
    line_height_extras: &BTreeMap<usize, f32>,
) -> f32 {
    row_height + line_height_extras.get(&line_index).copied().unwrap_or(0.0)
}

fn line_index_at_virtual_offset(
    offset_y: f32,
    line_count: usize,
    row_height: f32,
    line_height_extras: &BTreeMap<usize, f32>,
) -> usize {
    let Some(last_line_index) = line_count.checked_sub(1) else {
        return 0;
    };

    let offset_y = offset_y.max(0.0);
    let mut accumulated_extra_height = 0.0;

    for (line_index, extra_height) in line_height_extras {
        let line_top = *line_index as f32 * row_height + accumulated_extra_height;

        if offset_y < line_top {
            return (((offset_y - accumulated_extra_height).max(0.0) / row_height).floor()
                as usize)
                .min(last_line_index);
        }

        let line_bottom = line_top + row_height + *extra_height;

        if offset_y < line_bottom {
            return (*line_index).min(last_line_index);
        }

        accumulated_extra_height += *extra_height;
    }

    ((offset_y - accumulated_extra_height).max(0.0) / row_height)
        .floor()
        .min(last_line_index as f32) as usize
}

fn first_missing_visible_line(
    row_range: Range<usize>,
    mut contains_line: impl FnMut(usize) -> bool,
) -> Option<usize> {
    row_range
        .into_iter()
        .find(|line_index| !contains_line(*line_index))
}

fn show_loading_line(ui: &mut egui::Ui, line_index: usize, row_height: f32, text_width: f32) {
    ui.horizontal(|ui| {
        let (line_number_rect, _) =
            ui.allocate_exact_size(Vec2::new(EDITOR_GUTTER_WIDTH, row_height), Sense::hover());
        ui.painter().text(
            line_number_top_pos(line_number_rect),
            Align2::RIGHT_TOP,
            (line_index + 1).to_string(),
            FontId::new(12.0, FontFamily::Monospace),
            VSCODE_TEXT_DIM,
        );

        ui.add_space(EDITOR_GUTTER_GAP);
        ui.allocate_ui(Vec2::new(text_width, row_height), |ui| {
            ui.label(RichText::new("Loading...").color(VSCODE_TEXT_DIM));
        });
    });
}

#[derive(Debug, Clone, Copy)]
struct LargeLineBlockView<'a> {
    row_height: f32,
    text_width: f32,
    editor_font_size: f32,
    search_query: &'a str,
    search_options: SearchOptions,
}

fn show_virtual_line_block(
    ui: &mut egui::Ui,
    document: &mut LargeDocument,
    row_range: Range<usize>,
    view: LargeLineBlockView<'_>,
) -> Option<LargeEditorAction> {
    let line_count = row_range.end.saturating_sub(row_range.start);

    if line_count == 0 {
        return None;
    }

    let mut message = None;
    let mut block_text = row_range
        .clone()
        .map(|line_index| document.file_line_text(line_index).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    let block_height = view.row_height * line_count as f32;

    ui.horizontal(|ui| {
        let (line_number_rect, _) =
            ui.allocate_exact_size(Vec2::new(EDITOR_GUTTER_WIDTH, block_height), Sense::hover());

        for (offset, line_index) in row_range.clone().enumerate() {
            let number_rect = egui::Rect::from_min_size(
                egui::pos2(
                    line_number_rect.left(),
                    line_number_rect.top() + offset as f32 * view.row_height,
                ),
                Vec2::new(EDITOR_GUTTER_WIDTH, view.row_height),
            );
            ui.painter().text(
                line_number_top_pos(number_rect),
                Align2::RIGHT_TOP,
                (line_index + 1).to_string(),
                FontId::new(12.0, FontFamily::Monospace),
                VSCODE_TEXT_DIM,
            );
        }

        ui.add_space(EDITOR_GUTTER_GAP);

        let editor_id = ui.make_persistent_id((
            "large_line_block_editor",
            document.path(),
            row_range.start,
            row_range.end,
        ));
        let mut layouter = |ui: &egui::Ui, text: &str, wrap_width: f32| {
            ui.fonts(|fonts| {
                fonts.layout_job(editor_highlight_layout_job(
                    text,
                    large_line_block_layout_width(wrap_width, view.text_width),
                    view.editor_font_size,
                    view.search_query,
                    view.search_options,
                    None,
                    &[],
                ))
            })
        };
        let response = ui.add_sized(
            Vec2::new(view.text_width, block_height),
            TextEdit::multiline(&mut block_text)
                .id(editor_id)
                .font(editor_font_id(view.editor_font_size))
                .text_color(VSCODE_TEXT)
                .desired_width(view.text_width)
                .desired_rows(line_count)
                .min_size(Vec2::new(view.text_width, block_height))
                .lock_focus(true)
                .margin(Margin::ZERO)
                .frame(false)
                .layouter(&mut layouter),
        );

        if response.changed() {
            message = apply_large_line_block_edit(document, row_range.clone(), &block_text);
        }
    });

    message.map(LargeEditorAction::Message)
}

fn large_line_block_layout_width(provided_wrap_width: f32, text_width: f32) -> f32 {
    if text_width.is_finite() {
        text_width.max(provided_wrap_width)
    } else {
        provided_wrap_width
    }
}

fn apply_large_line_block_edit(
    document: &mut LargeDocument,
    row_range: Range<usize>,
    block_text: &str,
) -> Option<AppMessage> {
    let edited_lines = block_text.split('\n').collect::<Vec<_>>();
    let expected_line_count = row_range.end.saturating_sub(row_range.start);

    if edited_lines.len() != expected_line_count {
        return Some(AppMessage::Error(
            "Large viewport block edits must keep the same line count".to_owned(),
        ));
    }

    for line_index in row_range.clone() {
        if !document.is_file_line_editable(line_index) {
            return Some(AppMessage::Error(
                "The visible block includes a partial viewport boundary line".to_owned(),
            ));
        }
    }

    let mut changed = false;

    for (line_index, replacement) in row_range.zip(edited_lines) {
        let mut replacement = replacement.to_owned();
        remove_line_breaks(&mut replacement);

        match document.replace_file_line(line_index, &replacement) {
            Ok(line_changed) => changed |= line_changed,
            Err(error) => return Some(AppMessage::Error(format!("Edit failed: {error}"))),
        }
    }

    changed.then(|| AppMessage::Info("Edited visible line block".to_owned()))
}

fn show_virtual_line(
    ui: &mut egui::Ui,
    document: &mut LargeDocument,
    line_index: usize,
    row_height: f32,
    text_width: f32,
    wrap_lines: bool,
    editor_font_size: f32,
) -> Option<AppMessage> {
    let mut message = None;

    ui.horizontal(|ui| {
        let (line_number_rect, _) =
            ui.allocate_exact_size(Vec2::new(EDITOR_GUTTER_WIDTH, row_height), Sense::hover());
        ui.painter().text(
            line_number_top_pos(line_number_rect),
            Align2::RIGHT_TOP,
            (line_index + 1).to_string(),
            FontId::new(12.0, FontFamily::Monospace),
            VSCODE_TEXT_DIM,
        );

        ui.add_space(EDITOR_GUTTER_GAP);

        let mut line_text = document
            .file_line_text(line_index)
            .unwrap_or_default()
            .to_owned();
        let editable = document.is_file_line_editable(line_index);
        let editor_id = ui.make_persistent_id(("large_line_editor", document.path(), line_index));

        let response = ui
            .add_enabled_ui(editable, |ui| {
                add_line_editor(
                    ui,
                    &mut line_text,
                    text_width,
                    row_height,
                    wrap_lines,
                    editor_font_size,
                    editor_id,
                )
            })
            .inner;

        if editable && response.changed() {
            remove_line_breaks(&mut line_text);

            message = match document.replace_file_line(line_index, &line_text) {
                Ok(true) => Some(AppMessage::Info(format!("Edited line {}", line_index + 1))),
                Ok(false) => None,
                Err(error) => Some(AppMessage::Error(format!("Edit failed: {error}"))),
            };
        }
    });

    message
}

fn line_number_top_pos(line_number_rect: egui::Rect) -> egui::Pos2 {
    egui::pos2(line_number_rect.right(), line_number_rect.top() + 2.0)
}

fn remove_line_breaks(text: &mut String) {
    text.retain(|character| character != '\n' && character != '\r');
}

fn sidebar_heading(ui: &mut egui::Ui, label: &str) {
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(
            RichText::new(label)
                .color(VSCODE_TEXT_DIM)
                .size(10.0)
                .strong(),
        );
    });
}

fn sidebar_wrapped_path(ui: &mut egui::Ui, path_text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(SIDEBAR_CONTENT_INDENT);
        let wrap_width = sidebar_path_wrap_width(ui.available_width());

        ui.add(sidebar_path_label(path_text, wrap_width));
    });
}

fn sidebar_path_label(path_text: &str, wrap_width: f32) -> egui::Label {
    egui::Label::new(path_label_layout_job(path_text, wrap_width))
        .wrap()
        .selectable(true)
}

fn sidebar_path_wrap_width(available_width: f32) -> f32 {
    if available_width.is_finite() {
        available_width.max(SIDEBAR_PATH_MIN_WRAP_WIDTH)
    } else {
        SIDEBAR_PATH_MIN_WRAP_WIDTH
    }
}

fn path_label_layout_job(path_text: &str, wrap_width: f32) -> LayoutJob {
    let mut layout_job = LayoutJob::single_section(
        path_text.to_owned(),
        TextFormat::simple(FontId::new(12.0, FontFamily::Monospace), VSCODE_TEXT_DIM),
    );

    layout_job.wrap.max_width = sidebar_path_wrap_width(wrap_width);
    layout_job.wrap.break_anywhere = true;

    layout_job
}

#[derive(Debug, Clone)]
struct HighlightRange {
    range: Range<usize>,
    foreground: Option<Color32>,
    background: Option<Color32>,
}

fn editor_highlight_layout_job(
    text: &str,
    wrap_width: f32,
    editor_font_size: f32,
    search_query: &str,
    search_options: SearchOptions,
    primary_selection: Option<TextSelection>,
    multi_selections: &[TextSelection],
) -> LayoutJob {
    let mut ranges = syntax_highlight_ranges(text);
    ranges.extend(search_highlight_ranges(text, search_query, search_options));
    ranges.extend(word_highlight_ranges(text, primary_selection));
    ranges.extend(selection_highlight_ranges(text, multi_selections));
    ranges.extend(bracket_highlight_ranges(text, primary_selection));

    let mut boundaries = vec![0, text.len()];
    for highlight in &ranges {
        boundaries.push(highlight.range.start.min(text.len()));
        boundaries.push(highlight.range.end.min(text.len()));
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries.retain(|boundary| text.is_char_boundary(*boundary));

    let mut layout_job = LayoutJob::default();
    layout_job.wrap.max_width = wrap_width.max(EDITOR_MIN_WRAP_TEXT_WIDTH);
    layout_job.wrap.break_anywhere = true;

    for boundary_pair in boundaries.windows(2) {
        let start = boundary_pair[0];
        let end = boundary_pair[1];

        if start == end {
            continue;
        }

        let foreground = ranges
            .iter()
            .find(|highlight| {
                highlight.foreground.is_some()
                    && highlight.range.start <= start
                    && end <= highlight.range.end
            })
            .and_then(|highlight| highlight.foreground)
            .unwrap_or(VSCODE_TEXT);
        let background = ranges
            .iter()
            .rev()
            .find(|highlight| {
                highlight.background.is_some()
                    && highlight.range.start <= start
                    && end <= highlight.range.end
            })
            .and_then(|highlight| highlight.background);
        let mut format = TextFormat::simple(editor_font_id(editor_font_size), foreground);
        format.line_height = Some(editor_row_height(editor_font_size));

        if let Some(background) = background {
            format.background = background;
        }

        layout_job.append(&text[start..end], 0.0, format);
    }

    layout_job
}

fn syntax_highlight_ranges(text: &str) -> Vec<HighlightRange> {
    let mut ranges = Vec::new();
    let mut iterator = text.char_indices().peekable();

    while let Some((start, character)) = iterator.next() {
        if character == '"' {
            let mut end = start + character.len_utf8();
            let mut escaped = false;

            for (index, next_character) in iterator.by_ref() {
                end = index + next_character.len_utf8();

                if escaped {
                    escaped = false;
                } else if next_character == '\\' {
                    escaped = true;
                } else if next_character == '"' {
                    break;
                }
            }

            ranges.push(HighlightRange {
                range: start..end,
                foreground: Some(SYNTAX_STRING),
                background: None,
            });
        } else if character == '/' && iterator.peek().is_some_and(|(_, next)| *next == '/') {
            let mut end = text.len();

            for (index, next_character) in iterator.by_ref() {
                if next_character == '\n' {
                    end = index;
                    break;
                }
            }

            ranges.push(HighlightRange {
                range: start..end,
                foreground: Some(SYNTAX_COMMENT),
                background: None,
            });
        } else if character.is_ascii_digit() {
            let mut end = start + character.len_utf8();

            while let Some((index, next_character)) = iterator.peek().copied() {
                if next_character.is_ascii_digit()
                    || matches!(next_character, '.' | '_' | 'e' | 'E' | '-' | '+')
                {
                    iterator.next();
                    end = index + next_character.len_utf8();
                } else {
                    break;
                }
            }

            ranges.push(HighlightRange {
                range: start..end,
                foreground: Some(SYNTAX_NUMBER),
                background: None,
            });
        } else if is_identifier_start(character) {
            let mut end = start + character.len_utf8();
            let mut word = String::from(character);

            while let Some((index, next_character)) = iterator.peek().copied() {
                if is_identifier_continue(next_character) {
                    iterator.next();
                    word.push(next_character);
                    end = index + next_character.len_utf8();
                } else {
                    break;
                }
            }

            if is_syntax_keyword(&word) {
                ranges.push(HighlightRange {
                    range: start..end,
                    foreground: Some(SYNTAX_KEYWORD),
                    background: None,
                });
            }
        } else if matches!(character, '{' | '}' | '[' | ']' | '(' | ')') {
            ranges.push(HighlightRange {
                range: start..start + character.len_utf8(),
                foreground: Some(SYNTAX_BRACKET),
                background: None,
            });
        }
    }

    ranges
}

fn search_highlight_ranges(
    text: &str,
    search_query: &str,
    search_options: SearchOptions,
) -> Vec<HighlightRange> {
    if search_query.is_empty() {
        return Vec::new();
    }

    find_text_matches(text, search_query, search_options, HIGHLIGHT_SEARCH_LIMIT)
        .map(|matches| {
            matches
                .into_iter()
                .map(|search_match| HighlightRange {
                    range: search_match.range,
                    foreground: None,
                    background: Some(VSCODE_HIGHLIGHT),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn selection_highlight_ranges(text: &str, selections: &[TextSelection]) -> Vec<HighlightRange> {
    let character_count = text.chars().count();

    selections
        .iter()
        .copied()
        .filter(|selection| !selection.is_cursor())
        .filter(|selection| {
            selection.start() <= character_count && selection.end() <= character_count
        })
        .map(|selection| HighlightRange {
            range: editor_ops::char_to_byte_index(text, selection.start())
                ..editor_ops::char_to_byte_index(text, selection.end()),
            foreground: None,
            background: Some(VSCODE_SELECTION_HIGHLIGHT),
        })
        .collect()
}

fn word_highlight_ranges(
    text: &str,
    primary_selection: Option<TextSelection>,
) -> Vec<HighlightRange> {
    let Some(selection) = primary_selection else {
        return Vec::new();
    };
    let selection = if selection.is_cursor() {
        editor_ops::word_at(text, selection.head)
    } else {
        Some(selection)
    };
    let Some(selection) = selection else {
        return Vec::new();
    };
    let word = editor_ops::selected_text(text, selection);

    if word.is_empty() {
        return Vec::new();
    }

    editor_ops::select_all_occurrences(text, selection)
        .into_iter()
        .filter(|candidate| editor_ops::selected_text(text, *candidate) == word)
        .map(|candidate| HighlightRange {
            range: editor_ops::char_to_byte_index(text, candidate.start())
                ..editor_ops::char_to_byte_index(text, candidate.end()),
            foreground: None,
            background: Some(Color32::from_rgb(0x33, 0x3f, 0x4f)),
        })
        .collect()
}

fn bracket_highlight_ranges(
    text: &str,
    primary_selection: Option<TextSelection>,
) -> Vec<HighlightRange> {
    let Some(selection) = primary_selection else {
        return Vec::new();
    };
    let Some((left, right)) = matching_bracket_pair(text, selection.head) else {
        return Vec::new();
    };

    [left, right]
        .into_iter()
        .map(|index| HighlightRange {
            range: editor_ops::char_to_byte_index(text, index)
                ..editor_ops::char_to_byte_index(text, index + 1),
            foreground: Some(Color32::WHITE),
            background: Some(VSCODE_ACCENT),
        })
        .collect()
}

fn matching_bracket_pair(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let characters = text.chars().collect::<Vec<_>>();

    if characters.is_empty() {
        return None;
    }

    let candidate = if cursor < characters.len() && is_bracket(characters[cursor]) {
        cursor
    } else if cursor > 0 && is_bracket(characters[cursor - 1]) {
        cursor - 1
    } else {
        return None;
    };
    let bracket = characters[candidate];
    let (matching, forward) = match bracket {
        '(' => (')', true),
        '[' => (']', true),
        '{' => ('}', true),
        ')' => ('(', false),
        ']' => ('[', false),
        '}' => ('{', false),
        _ => return None,
    };
    let mut depth = 0;

    if forward {
        for (index, character) in characters.iter().copied().enumerate().skip(candidate) {
            if character == bracket {
                depth += 1;
            } else if character == matching {
                depth -= 1;

                if depth == 0 {
                    return Some((candidate, index));
                }
            }
        }
    } else {
        for (index, character) in characters
            .iter()
            .copied()
            .enumerate()
            .take(candidate + 1)
            .rev()
        {
            if character == bracket {
                depth += 1;
            } else if character == matching {
                depth -= 1;

                if depth == 0 {
                    return Some((index, candidate));
                }
            }
        }
    }

    None
}

fn is_bracket(character: char) -> bool {
    matches!(character, '(' | ')' | '[' | ']' | '{' | '}')
}

fn is_identifier_start(character: char) -> bool {
    character == '_' || character.is_ascii_alphabetic()
}

fn is_identifier_continue(character: char) -> bool {
    is_identifier_start(character) || character.is_ascii_digit()
}

fn is_syntax_keyword(word: &str) -> bool {
    matches!(
        word,
        "as" | "async"
            | "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "default"
            | "else"
            | "enum"
            | "false"
            | "fn"
            | "for"
            | "from"
            | "function"
            | "if"
            | "impl"
            | "import"
            | "in"
            | "let"
            | "match"
            | "mod"
            | "mut"
            | "null"
            | "pub"
            | "return"
            | "self"
            | "static"
            | "struct"
            | "switch"
            | "true"
            | "try"
            | "type"
            | "use"
            | "var"
            | "while"
            | "where"
    )
}

fn collect_shortcut_actions(context: &egui::Context) -> Vec<ShortcutAction> {
    let mut actions = Vec::new();

    context.input_mut(|input| {
        if input.consume_shortcut(&KeyboardShortcut::new(
            Modifiers::COMMAND | Modifiers::SHIFT,
            egui::Key::S,
        )) {
            actions.push(ShortcutAction::SaveAs);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, egui::Key::S)) {
            actions.push(ShortcutAction::Save);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, egui::Key::N)) {
            actions.push(ShortcutAction::New);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, egui::Key::O)) {
            actions.push(ShortcutAction::Open);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, egui::Key::F)) {
            actions.push(ShortcutAction::Search);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, egui::Key::G)) {
            actions.push(ShortcutAction::GoToLine);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(
            Modifiers::COMMAND | Modifiers::SHIFT,
            egui::Key::L,
        )) {
            actions.push(ShortcutAction::SelectAllOccurrences);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, egui::Key::D)) {
            actions.push(ShortcutAction::AddNextOccurrence);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, egui::Key::L)) {
            actions.push(ShortcutAction::SelectLines);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(
            Modifiers::COMMAND | Modifiers::SHIFT,
            egui::Key::K,
        )) {
            actions.push(ShortcutAction::DeleteLine);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(
            Modifiers::COMMAND | Modifiers::SHIFT,
            egui::Key::U,
        )) {
            actions.push(ShortcutAction::Lowercase);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, egui::Key::U)) {
            actions.push(ShortcutAction::Uppercase);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, egui::Key::Plus))
            || input.consume_shortcut(&KeyboardShortcut::new(
                Modifiers::COMMAND,
                egui::Key::Equals,
            ))
        {
            actions.push(ShortcutAction::ZoomIn);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, egui::Key::Minus)) {
            actions.push(ShortcutAction::ZoomOut);
        }
        if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, egui::Key::Num0)) {
            actions.push(ShortcutAction::ResetZoom);
        }
        if input.consume_key(Modifiers::ALT, egui::Key::Z) {
            actions.push(ShortcutAction::ToggleWrap);
        }
        if input.consume_key(Modifiers::ALT | Modifiers::SHIFT, egui::Key::ArrowUp) {
            actions.push(ShortcutAction::CopyLineUp);
        }
        if input.consume_key(Modifiers::ALT | Modifiers::SHIFT, egui::Key::ArrowDown) {
            actions.push(ShortcutAction::CopyLineDown);
        }
        if input.consume_key(Modifiers::ALT, egui::Key::ArrowUp) {
            actions.push(ShortcutAction::MoveLineUp);
        }
        if input.consume_key(Modifiers::ALT, egui::Key::ArrowDown) {
            actions.push(ShortcutAction::MoveLineDown);
        }
        if input.consume_key(Modifiers::ALT | Modifiers::SHIFT, egui::Key::R) {
            actions.push(ShortcutAction::RectangularSelection);
        }
        if input.consume_key(Modifiers::ALT | Modifiers::SHIFT, egui::Key::C) {
            actions.push(ShortcutAction::CopyRectangle);
        }
        if input.consume_key(Modifiers::ALT | Modifiers::SHIFT, egui::Key::V) {
            actions.push(ShortcutAction::PasteRectangle);
        }
        if input.consume_key(Modifiers::NONE, egui::Key::Escape) {
            actions.push(ShortcutAction::ClearMultiCursor);
        }
        if input.consume_key(Modifiers::NONE, egui::Key::F1) {
            actions.push(ShortcutAction::ToggleHelp);
        }
        if input.consume_key(Modifiers::NONE, egui::Key::F3)
            || input.consume_key(Modifiers::SHIFT, egui::Key::F3)
        {
            actions.push(ShortcutAction::RunSearch);
        }
    });

    actions
}

fn take_multi_cursor_text_edits(context: &egui::Context) -> Vec<InlineInputEdit> {
    let mut edits = Vec::new();

    context.input_mut(|input| {
        let events = std::mem::take(&mut input.raw.events);

        input.raw.events = events
            .into_iter()
            .filter_map(|event| match event {
                egui::Event::Text(text) if !text.is_empty() => {
                    edits.push(InlineInputEdit::Insert(text));
                    None
                }
                egui::Event::Paste(text) if !text.is_empty() => {
                    edits.push(InlineInputEdit::Insert(text));
                    None
                }
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    modifiers,
                    ..
                } if !modifiers.command && !modifiers.ctrl && !modifiers.mac_cmd => {
                    edits.push(InlineInputEdit::Insert("\n".to_owned()));
                    None
                }
                egui::Event::Key {
                    key: egui::Key::Backspace,
                    pressed: true,
                    modifiers,
                    ..
                } if !modifiers.command && !modifiers.ctrl && !modifiers.mac_cmd => {
                    edits.push(InlineInputEdit::Backspace);
                    None
                }
                egui::Event::Key {
                    key: egui::Key::Delete,
                    pressed: true,
                    modifiers,
                    ..
                } if !modifiers.command && !modifiers.ctrl && !modifiers.mac_cmd => {
                    edits.push(InlineInputEdit::Delete);
                    None
                }
                event => Some(event),
            })
            .collect();
    });

    edits
}

fn parse_go_to_line_index(input: &str, line_count: usize) -> Result<usize, String> {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        return Err("Enter a line number".to_owned());
    }

    let line_number = trimmed
        .parse::<usize>()
        .map_err(|_| "Line number must be a positive integer".to_owned())?;

    if line_number == 0 {
        return Err("Line number starts at 1".to_owned());
    }

    Ok(line_number.min(line_count.max(1)) - 1)
}

fn line_start_char_index(text: &str, line_index: usize) -> usize {
    if line_index == 0 {
        return 0;
    }

    let mut current_line = 0;

    for (byte_index, character) in text.char_indices() {
        if character == '\n' {
            current_line += 1;

            if current_line == line_index {
                return text[..byte_index + character.len_utf8()].chars().count();
            }
        }
    }

    text.chars().count()
}

fn apply_inline_selection_request(
    context: &egui::Context,
    editor_id: egui::Id,
    selection: Option<TextSelection>,
) -> bool {
    let Some(selection) = selection else {
        return false;
    };

    let mut state = TextEditState::load(context, editor_id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(
        CCursor::new(selection.anchor),
        CCursor::new(selection.head),
    )));
    state.store(context, editor_id);

    true
}

fn editor_monospace_char_width(editor_font_size: f32) -> f32 {
    EDITOR_MONOSPACE_CHAR_WIDTH * editor_font_size / DEFAULT_EDITOR_FONT_SIZE
}

fn editor_row_height(editor_font_size: f32) -> f32 {
    (EDITOR_ROW_HEIGHT * editor_font_size / DEFAULT_EDITOR_FONT_SIZE).ceil()
}

fn inline_editor_min_height(
    available_height: f32,
    visual_lines: usize,
    editor_font_size: f32,
) -> f32 {
    let row_height = editor_row_height(editor_font_size);
    let document_height = visual_lines.max(1) as f32 * row_height;

    if available_height.is_finite() {
        available_height.max(document_height).ceil()
    } else {
        document_height.ceil()
    }
}

fn longest_line_character_count(text: &str) -> usize {
    text.lines()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
}

fn editor_text_width_for_characters(character_count: usize, editor_font_size: f32) -> f32 {
    let char_width = editor_monospace_char_width(editor_font_size);

    (character_count as f32 * char_width)
        .max(EDITOR_MIN_TEXT_WIDTH)
        .ceil()
}

fn editor_text_width(content_width: f32, wrap_lines: bool) -> f32 {
    let minimum_width = if wrap_lines {
        EDITOR_MIN_WRAP_TEXT_WIDTH
    } else {
        EDITOR_MIN_TEXT_WIDTH
    };

    (content_width - EDITOR_GUTTER_WIDTH - EDITOR_GUTTER_GAP).max(minimum_width)
}

fn editor_content_width_for_text(
    text: &str,
    available_width: f32,
    wrap_lines: bool,
    editor_font_size: f32,
) -> f32 {
    let minimum_width = if wrap_lines {
        EDITOR_GUTTER_WIDTH + EDITOR_GUTTER_GAP + EDITOR_MIN_WRAP_TEXT_WIDTH
    } else {
        EDITOR_MIN_TEXT_WIDTH
    };
    let available_width = if available_width.is_finite() {
        available_width.max(minimum_width)
    } else {
        minimum_width
    };

    if wrap_lines {
        return available_width;
    }

    let text_width =
        editor_text_width_for_characters(longest_line_character_count(text), editor_font_size);

    (EDITOR_GUTTER_WIDTH + EDITOR_GUTTER_GAP + text_width).max(available_width)
}

fn editor_line_row_height_for_ui(
    ui: &egui::Ui,
    text: &str,
    text_width: f32,
    row_height: f32,
    editor_font_size: f32,
) -> f32 {
    let galley_height = ui.fonts(|fonts| {
        fonts
            .layout_job(editor_line_layout_job(text, text_width, editor_font_size))
            .size()
            .y
    });

    galley_height.max(row_height).ceil()
}

fn editor_line_layout_job(text: &str, wrap_width: f32, editor_font_size: f32) -> LayoutJob {
    editor_highlight_layout_job(
        text,
        wrap_width,
        editor_font_size,
        "",
        SearchOptions::default(),
        None,
        &[],
    )
}

fn editor_font_id(editor_font_size: f32) -> FontId {
    FontId::new(editor_font_size, FontFamily::Monospace)
}

fn editor_widget(
    text: &mut String,
    width: f32,
    min_height: f32,
    editor_font_size: f32,
) -> TextEdit<'_> {
    TextEdit::multiline(text)
        .font(editor_font_id(editor_font_size))
        .text_color(VSCODE_TEXT)
        .desired_width(width)
        .desired_rows(32)
        .min_size(Vec2::new(
            width,
            min_height.max(editor_row_height(editor_font_size)),
        ))
        .lock_focus(true)
        .margin(Margin::ZERO)
        .frame(false)
}

fn add_line_editor(
    ui: &mut egui::Ui,
    text: &mut String,
    width: f32,
    height: f32,
    wrap_lines: bool,
    editor_font_size: f32,
    id: egui::Id,
) -> egui::Response {
    if wrap_lines {
        let mut layouter = |ui: &egui::Ui, text: &str, wrap_width: f32| {
            ui.fonts(|fonts| {
                fonts.layout_job(editor_line_layout_job(text, wrap_width, editor_font_size))
            })
        };

        return ui.add_sized(
            Vec2::new(width, height),
            wrapped_line_editor_widget(text, width, editor_font_size, id).layouter(&mut layouter),
        );
    }

    ui.add_sized(
        Vec2::new(width, height),
        line_editor_widget(text, width, editor_font_size, id),
    )
}

fn wrapped_line_editor_widget(
    text: &mut String,
    width: f32,
    editor_font_size: f32,
    id: egui::Id,
) -> TextEdit<'_> {
    TextEdit::multiline(text)
        .id(id)
        .font(editor_font_id(editor_font_size))
        .text_color(VSCODE_TEXT)
        .desired_width(width)
        .desired_rows(1)
        .return_key(None)
        .lock_focus(true)
        .margin(Margin::ZERO)
        .frame(false)
}

fn line_editor_widget(
    text: &mut String,
    width: f32,
    editor_font_size: f32,
    id: egui::Id,
) -> TextEdit<'_> {
    TextEdit::singleline(text)
        .id(id)
        .font(editor_font_id(editor_font_size))
        .text_color(VSCODE_TEXT)
        .desired_width(width)
        .clip_text(false)
        .margin(Margin::ZERO)
        .frame(false)
}

fn parent_directory(path: &Path) -> Option<PathBuf> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

fn help_row(ui: &mut egui::Ui, action: &str, shortcut: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(action).color(VSCODE_TEXT));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(shortcut).color(VSCODE_TEXT_DIM).monospace());
        });
    });
}

fn status_text(value: impl Into<String>) -> RichText {
    RichText::new(value.into()).color(Color32::WHITE).size(12.0)
}

fn apply_vscode_theme(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    visuals.panel_fill = VSCODE_BG;
    visuals.window_fill = VSCODE_PANEL;
    visuals.extreme_bg_color = VSCODE_BG;
    visuals.faint_bg_color = VSCODE_PANEL;
    visuals.code_bg_color = VSCODE_BG;
    visuals.override_text_color = Some(VSCODE_TEXT);
    visuals.hyperlink_color = VSCODE_ACCENT;
    visuals.selection.bg_fill = Color32::from_rgb(0x26, 0x4f, 0x78);
    visuals.selection.stroke = Stroke::new(1.0, VSCODE_ACCENT);
    visuals.window_stroke = Stroke::new(1.0, VSCODE_BORDER);

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = VSCODE_PANEL;
    widgets.noninteractive.weak_bg_fill = VSCODE_PANEL;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, VSCODE_BORDER);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, VSCODE_TEXT);
    widgets.inactive.bg_fill = VSCODE_TAB;
    widgets.inactive.weak_bg_fill = VSCODE_TAB;
    widgets.inactive.bg_stroke = Stroke::new(1.0, VSCODE_BORDER);
    widgets.inactive.fg_stroke = Stroke::new(1.0, VSCODE_TEXT);
    widgets.hovered.bg_fill = Color32::from_rgb(0x3e, 0x3e, 0x42);
    widgets.hovered.weak_bg_fill = Color32::from_rgb(0x3e, 0x3e, 0x42);
    widgets.hovered.bg_stroke = Stroke::new(1.0, VSCODE_ACCENT);
    widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    widgets.active.bg_fill = VSCODE_ACCENT;
    widgets.active.weak_bg_fill = VSCODE_ACCENT;
    widgets.active.bg_stroke = Stroke::new(1.0, VSCODE_ACCENT);
    widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    context.set_visuals(visuals);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_dialog_file_name_defaults_to_untitled_text() {
        let app = PhantomApp::default();

        assert_eq!(app.save_dialog_file_name(), "Untitled.txt");
    }

    #[test]
    fn save_dialog_file_name_uses_existing_document_name() {
        let app = PhantomApp {
            document: ActiveDocument::Inline(EditorDocument::from_saved_text(
                PathBuf::from("notes.md"),
                String::new(),
            )),
            ..Default::default()
        };

        assert_eq!(app.save_dialog_file_name(), "notes.md");
    }

    #[test]
    fn dialog_start_directory_prefers_path_input_parent() {
        let directory = std::env::temp_dir();
        let app = PhantomApp {
            document: ActiveDocument::Inline(EditorDocument::from_saved_text(
                PathBuf::from("document.txt"),
                String::new(),
            )),
            path_input: directory.join("manual.txt").display().to_string(),
            ..Default::default()
        };

        assert_eq!(app.dialog_start_directory(), Some(directory));
    }

    #[test]
    fn save_target_path_prefers_existing_document_path() {
        let directory = std::env::temp_dir();
        let document_path = directory.join("opened.txt");
        let manual_path = directory.join("manual.txt");
        let app = PhantomApp {
            document: ActiveDocument::Inline(EditorDocument::from_saved_text(
                document_path.clone(),
                String::new(),
            )),
            path_input: manual_path.display().to_string(),
            ..Default::default()
        };

        assert_eq!(app.save_target_path(), Some(document_path));
    }

    #[test]
    fn parse_go_to_line_index_clamps_to_document_bounds() {
        assert_eq!(parse_go_to_line_index("1", 10).unwrap(), 0);
        assert_eq!(parse_go_to_line_index("999", 10).unwrap(), 9);
        assert_eq!(parse_go_to_line_index(" 3 ", 10).unwrap(), 2);
    }

    #[test]
    fn parse_go_to_line_index_rejects_invalid_input() {
        assert!(parse_go_to_line_index("", 10).is_err());
        assert!(parse_go_to_line_index("0", 10).is_err());
        assert!(parse_go_to_line_index("line", 10).is_err());
    }

    #[test]
    fn line_start_char_index_finds_target_line_start() {
        assert_eq!(line_start_char_index("alpha\nbeta\ngamma", 0), 0);
        assert_eq!(line_start_char_index("alpha\nbeta\ngamma", 1), 6);
        assert_eq!(line_start_char_index("alpha\nbeta\ngamma", 2), 11);
        assert_eq!(line_start_char_index("alpha\nbeta\ngamma", 99), 16);
    }

    #[test]
    fn open_go_to_line_prefills_current_line_and_requests_focus() {
        let mut app = PhantomApp::default();

        app.open_go_to_line();

        assert!(app.go_to_line.visible);
        assert_eq!(app.go_to_line.input, "1");
        assert!(app.go_to_line.request_focus);
        assert!(app.go_to_line.error.is_none());
    }

    #[test]
    fn add_next_occurrence_command_tracks_multiple_selections() {
        let mut app = PhantomApp {
            document: ActiveDocument::Inline(EditorDocument::from_saved_text(
                PathBuf::from("sample.txt"),
                "cat dog cat".to_owned(),
            )),
            current_inline_selection: Some(TextSelection::new(0, 3)),
            ..Default::default()
        };

        app.add_next_occurrence_selection();

        assert_eq!(
            app.inline_selections,
            vec![TextSelection::new(0, 3), TextSelection::new(8, 11)]
        );
    }

    #[test]
    fn case_conversion_command_updates_inline_document() {
        let mut app = PhantomApp {
            document: ActiveDocument::Inline(EditorDocument::from_saved_text(
                PathBuf::from("sample.txt"),
                "alpha beta".to_owned(),
            )),
            current_inline_selection: Some(TextSelection::new(6, 10)),
            ..Default::default()
        };

        app.convert_selected_case(CaseTransform::Upper);

        let ActiveDocument::Inline(document) = app.document else {
            panic!("test document should be inline");
        };
        assert_eq!(document.text(), "alpha BETA");
    }

    #[test]
    fn highlight_layout_marks_search_and_selection_backgrounds() {
        let layout_job = editor_highlight_layout_job(
            "let alpha = \"alpha\";",
            320.0,
            DEFAULT_EDITOR_FONT_SIZE,
            "alpha",
            SearchOptions::default(),
            Some(TextSelection::new(0, 3)),
            &[TextSelection::new(0, 3)],
        );

        assert_eq!(layout_job.text, "let alpha = \"alpha\";");
        assert!(layout_job
            .sections
            .iter()
            .any(|section| section.format.background == VSCODE_HIGHLIGHT));
        assert!(layout_job
            .sections
            .iter()
            .any(|section| section.format.background == VSCODE_SELECTION_HIGHLIGHT));
        assert!(layout_job
            .sections
            .iter()
            .any(|section| section.format.color == SYNTAX_KEYWORD));
        assert!(layout_job
            .sections
            .iter()
            .any(|section| section.format.color == SYNTAX_STRING));
    }

    #[test]
    fn matching_bracket_pair_finds_nested_pair() {
        assert_eq!(matching_bracket_pair("a({b})", 2), Some((2, 4)));
        assert_eq!(matching_bracket_pair("a({b})", 6), Some((1, 5)));
    }

    #[test]
    fn editor_zoom_scales_row_height_and_width_estimates() {
        assert!(editor_row_height(DEFAULT_EDITOR_FONT_SIZE * 2.0) > EDITOR_ROW_HEIGHT);
        assert!(
            editor_text_width_for_characters(80, DEFAULT_EDITOR_FONT_SIZE * 2.0)
                > editor_text_width_for_characters(80, DEFAULT_EDITOR_FONT_SIZE)
        );
    }

    #[test]
    fn inline_editor_min_height_covers_viewport_and_document_lines() {
        assert_eq!(
            inline_editor_min_height(480.0, 2, DEFAULT_EDITOR_FONT_SIZE),
            480.0
        );
        assert_eq!(
            inline_editor_min_height(40.0, 10, DEFAULT_EDITOR_FONT_SIZE),
            EDITOR_ROW_HEIGHT * 10.0
        );
    }

    #[test]
    fn inline_editor_widget_reserves_minimum_drag_selection_height() {
        egui::__run_test_ui(|ui| {
            let mut text = "alpha\nbeta".to_owned();
            let output = editor_widget(&mut text, 320.0, 360.0, DEFAULT_EDITOR_FONT_SIZE).show(ui);

            assert!(
                output.response.rect.height() >= 360.0,
                "inline editor height {} should cover the requested selection area",
                output.response.rect.height()
            );
        });
    }

    #[test]
    fn large_line_block_edit_updates_multiple_visible_lines() -> std::io::Result<()> {
        let path = unique_main_temp_path("large_line_block_edit.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma")?;
        let mut document = phantom::open_large_document_with_viewport(&path, 64)?;

        let message = apply_large_line_block_edit(&mut document, 0..2, "ALPHA\nBETA");

        assert!(matches!(message, Some(AppMessage::Info(_))));
        assert_eq!(document.file_line_text(0), Some("ALPHA"));
        assert_eq!(document.file_line_text(1), Some("BETA"));
        assert_eq!(document.file_line_text(2), Some("gamma"));

        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn large_line_block_edit_rejects_line_count_changes() -> std::io::Result<()> {
        let path = unique_main_temp_path("large_line_block_edit_line_count.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma")?;
        let mut document = phantom::open_large_document_with_viewport(&path, 64)?;

        let message = apply_large_line_block_edit(&mut document, 0..2, "ALPHA\nBETA\nEXTRA");

        assert!(matches!(message, Some(AppMessage::Error(_))));
        assert_eq!(document.file_line_text(0), Some("alpha"));
        assert_eq!(document.file_line_text(1), Some("beta"));

        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn large_line_block_layout_uses_full_text_width_to_prevent_wrapping() {
        assert_eq!(large_line_block_layout_width(320.0, 2_400.0), 2_400.0);
        assert_eq!(large_line_block_layout_width(640.0, 320.0), 640.0);
    }

    #[test]
    fn large_non_wrapped_rows_remove_implicit_spacing() {
        egui::__run_test_ui(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;

            configure_large_non_wrapped_row_spacing(ui);

            assert_eq!(ui.spacing().item_spacing.y, 0.0);
        });
    }

    #[test]
    fn highlighted_layout_uses_virtual_editor_row_height() {
        let layout_job = editor_highlight_layout_job(
            "alpha\nbeta",
            320.0,
            DEFAULT_EDITOR_FONT_SIZE,
            "",
            SearchOptions::default(),
            None,
            &[],
        );

        assert!(layout_job
            .sections
            .iter()
            .all(|section| section.format.line_height == Some(EDITOR_ROW_HEIGHT)));
    }

    fn unique_main_temp_path(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after Unix epoch")
            .as_nanos();

        std::env::temp_dir().join(format!("phantom-main-{unique}-{name}"))
    }

    #[test]
    fn should_block_close_for_dirty_document() {
        let mut document = EditorDocument::untitled();
        document.replace_text("draft".to_owned());
        let app = PhantomApp {
            document: ActiveDocument::Inline(document),
            ..Default::default()
        };

        assert!(app.should_block_close());
    }

    #[test]
    fn should_block_close_while_viewport_worker_is_running() {
        let (_sender, receiver) = mpsc::channel();
        let app = PhantomApp {
            viewport_receiver: Some(receiver),
            ..Default::default()
        };

        assert!(app.should_block_close());
    }

    #[test]
    fn sidebar_path_wrap_width_uses_current_sidebar_width() {
        assert_eq!(sidebar_path_wrap_width(180.0), 180.0);
        assert_eq!(sidebar_path_wrap_width(12.0), SIDEBAR_PATH_MIN_WRAP_WIDTH);
        assert_eq!(
            sidebar_path_wrap_width(f32::INFINITY),
            SIDEBAR_PATH_MIN_WRAP_WIDTH
        );
    }

    #[test]
    fn path_label_layout_job_wraps_long_paths_anywhere() {
        let layout_job = path_label_layout_job(
            "/very/long/path/without/spaces/that/should/not/expand/the/sidebar.txt",
            96.0,
        );

        assert_eq!(
            layout_job.text,
            "/very/long/path/without/spaces/that/should/not/expand/the/sidebar.txt"
        );
        assert_eq!(layout_job.wrap.max_width, 96.0);
        assert!(layout_job.wrap.break_anywhere);
    }

    #[test]
    fn editor_content_width_expands_for_long_lines_when_not_wrapping() {
        let long_line = "x".repeat(1_000);
        let width =
            editor_content_width_for_text(&long_line, 480.0, false, DEFAULT_EDITOR_FONT_SIZE);

        assert!(width > 480.0);
        assert_eq!(
            width,
            EDITOR_GUTTER_WIDTH
                + EDITOR_GUTTER_GAP
                + editor_text_width_for_characters(long_line.len(), DEFAULT_EDITOR_FONT_SIZE)
        );
    }

    #[test]
    fn editor_content_width_stays_within_available_width_when_wrapping() {
        let long_line = "x".repeat(1_000);

        assert_eq!(
            editor_content_width_for_text(&long_line, 480.0, true, DEFAULT_EDITOR_FONT_SIZE),
            480.0
        );
    }

    #[test]
    fn editor_text_width_uses_wrap_minimum_in_wrap_mode() {
        let content_width = EDITOR_GUTTER_WIDTH + EDITOR_GUTTER_GAP + 120.0;

        assert_eq!(editor_text_width(content_width, true), 120.0);
        assert_eq!(
            editor_content_width_for_text("x", 10.0, true, DEFAULT_EDITOR_FONT_SIZE),
            EDITOR_GUTTER_WIDTH + EDITOR_GUTTER_GAP + EDITOR_MIN_WRAP_TEXT_WIDTH
        );
    }

    #[test]
    fn editor_line_layout_job_wraps_at_requested_text_width() {
        let layout_job = editor_line_layout_job("abcdef", 123.0, DEFAULT_EDITOR_FONT_SIZE);

        assert_eq!(layout_job.wrap.max_width, 123.0);
        assert!(layout_job.wrap.break_anywhere);
    }

    #[test]
    fn editor_line_row_height_uses_actual_galley_height_for_long_wrapped_lines() {
        let long_json_line = format!(r#"{{"line":78218,"payload":"{}"}}"#, "x".repeat(2_000));
        let text_width = EDITOR_MONOSPACE_CHAR_WIDTH * 40.0;

        egui::__run_test_ui(|ui| {
            let expected_height = ui.fonts(|fonts| {
                fonts
                    .layout_job(editor_line_layout_job(
                        &long_json_line,
                        text_width,
                        DEFAULT_EDITOR_FONT_SIZE,
                    ))
                    .size()
                    .y
                    .max(EDITOR_ROW_HEIGHT)
                    .ceil()
            });
            let measured_height = editor_line_row_height_for_ui(
                ui,
                &long_json_line,
                text_width,
                EDITOR_ROW_HEIGHT,
                DEFAULT_EDITOR_FONT_SIZE,
            );

            assert_eq!(measured_height, expected_height);
        });
    }

    #[test]
    fn wrapped_measurement_line_range_uses_visible_rows_with_overscan() {
        let viewport = egui::Rect::from_min_max(
            egui::pos2(0.0, EDITOR_ROW_HEIGHT * 500.0),
            egui::pos2(200.0, EDITOR_ROW_HEIGHT * 510.0),
        );

        let range =
            wrapped_measurement_line_range(viewport, 10_000, EDITOR_ROW_HEIGHT, &BTreeMap::new());

        assert_eq!(range.start, 500 - WRAP_MEASURE_OVERSCAN_LINES);
        assert_eq!(range.end, 510 + WRAP_MEASURE_OVERSCAN_LINES + 1);
    }

    #[test]
    fn virtual_line_positions_include_only_known_wrapped_extras() {
        let line_height_extras = BTreeMap::from([(2, EDITOR_ROW_HEIGHT * 2.0)]);

        assert_eq!(
            virtual_editor_total_height(5, EDITOR_ROW_HEIGHT, &line_height_extras),
            EDITOR_ROW_HEIGHT * 7.0
        );
        assert_eq!(
            virtual_line_top(1, EDITOR_ROW_HEIGHT, &line_height_extras),
            EDITOR_ROW_HEIGHT
        );
        assert_eq!(
            virtual_line_top(3, EDITOR_ROW_HEIGHT, &line_height_extras),
            EDITOR_ROW_HEIGHT * 5.0
        );
        assert_eq!(
            virtual_line_height(2, EDITOR_ROW_HEIGHT, &line_height_extras),
            EDITOR_ROW_HEIGHT * 3.0
        );
        assert_eq!(
            virtual_line_height(3, EDITOR_ROW_HEIGHT, &line_height_extras),
            EDITOR_ROW_HEIGHT
        );
    }

    #[test]
    fn line_index_at_virtual_offset_accounts_for_wrapped_line_height() {
        let line_height_extras = BTreeMap::from([(2, EDITOR_ROW_HEIGHT * 2.0)]);

        assert_eq!(
            line_index_at_virtual_offset(
                EDITOR_ROW_HEIGHT * 2.5,
                5,
                EDITOR_ROW_HEIGHT,
                &line_height_extras,
            ),
            2
        );
        assert_eq!(
            line_index_at_virtual_offset(
                EDITOR_ROW_HEIGHT * 5.1,
                5,
                EDITOR_ROW_HEIGHT,
                &line_height_extras,
            ),
            3
        );
        assert_eq!(
            line_index_at_virtual_offset(9_999.0, 5, EDITOR_ROW_HEIGHT, &line_height_extras),
            4
        );
    }

    #[test]
    fn positioned_row_ui_does_not_advance_parent_layout() {
        egui::__run_test_ui(|ui| {
            ui.set_height(100.0);
            let parent_min_rect = ui.min_rect();
            let row_rect = egui::Rect::from_min_size(
                egui::pos2(ui.max_rect().left(), ui.max_rect().top() + 40.0),
                Vec2::new(200.0, 80.0),
            );

            with_positioned_row_ui(ui, row_rect, |row_ui| {
                row_ui.allocate_exact_size(Vec2::new(200.0, 80.0), Sense::hover());
            });

            assert_eq!(ui.min_rect(), parent_min_rect);
        });
    }

    #[test]
    fn update_line_height_extra_removes_short_lines_from_cache() {
        let mut line_height_extras = BTreeMap::new();

        update_line_height_extra(
            &mut line_height_extras,
            7,
            EDITOR_ROW_HEIGHT * 3.0,
            EDITOR_ROW_HEIGHT,
        );
        assert_eq!(line_height_extras.get(&7), Some(&(EDITOR_ROW_HEIGHT * 2.0)));

        update_line_height_extra(
            &mut line_height_extras,
            7,
            EDITOR_ROW_HEIGHT,
            EDITOR_ROW_HEIGHT,
        );
        assert!(!line_height_extras.contains_key(&7));
    }

    #[test]
    fn line_number_position_uses_row_top() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), Vec2::new(72.0, 88.0));

        assert_eq!(line_number_top_pos(rect), egui::pos2(82.0, 22.0));
    }

    #[test]
    fn remove_line_breaks_keeps_large_line_edits_single_line() {
        let mut text = "alpha\r\nbeta\ngamma".to_owned();

        remove_line_breaks(&mut text);

        assert_eq!(text, "alphabetagamma");
    }

    #[test]
    fn reset_line_height_extras_clears_cache_when_wrap_width_changes() {
        let mut line_height_extras = BTreeMap::from([(7, EDITOR_ROW_HEIGHT * 2.0)]);
        let mut measured_lines = BTreeSet::from([7]);
        let mut cached_text_width = Some(320.0);
        let mut cached_line_range = Some(0..10);

        assert!(!reset_line_height_extras_if_layout_changed(
            &mut line_height_extras,
            &mut measured_lines,
            &mut cached_text_width,
            &mut cached_line_range,
            320.25,
            0..10,
        ));
        assert!(line_height_extras.contains_key(&7));
        assert!(measured_lines.contains(&7));

        assert!(reset_line_height_extras_if_layout_changed(
            &mut line_height_extras,
            &mut measured_lines,
            &mut cached_text_width,
            &mut cached_line_range,
            340.0,
            0..10,
        ));
        assert!(line_height_extras.is_empty());
        assert!(measured_lines.is_empty());
        assert_eq!(cached_text_width, Some(340.0));
        assert_eq!(cached_line_range, Some(0..10));
    }

    #[test]
    fn reset_line_height_extras_clears_cache_when_rounded_wrap_width_changes() {
        let mut line_height_extras = BTreeMap::from([(7, EDITOR_ROW_HEIGHT * 2.0)]);
        let mut measured_lines = BTreeSet::from([7]);
        let mut cached_text_width = Some(320.4);
        let mut cached_line_range = Some(0..10);

        assert!(reset_line_height_extras_if_layout_changed(
            &mut line_height_extras,
            &mut measured_lines,
            &mut cached_text_width,
            &mut cached_line_range,
            320.6,
            0..10,
        ));

        assert!(line_height_extras.is_empty());
        assert!(measured_lines.is_empty());
    }

    #[test]
    fn reset_line_height_extras_clears_cache_when_viewport_range_changes() {
        let mut line_height_extras = BTreeMap::from([(7, EDITOR_ROW_HEIGHT * 2.0)]);
        let mut measured_lines = BTreeSet::from([7]);
        let mut cached_text_width = Some(320.0);
        let mut cached_line_range = Some(0..10);

        assert!(reset_line_height_extras_if_layout_changed(
            &mut line_height_extras,
            &mut measured_lines,
            &mut cached_text_width,
            &mut cached_line_range,
            320.0,
            10..20,
        ));

        assert!(line_height_extras.is_empty());
        assert!(measured_lines.is_empty());
        assert_eq!(cached_line_range, Some(10..20));
    }

    #[test]
    fn first_missing_visible_line_finds_missing_tail_rows() {
        let missing_line = first_missing_visible_line(8..14, |line_index| line_index < 11);

        assert_eq!(missing_line, Some(11));
        assert_eq!(first_missing_visible_line(8..11, |_| true), None);
    }

    #[test]
    fn sidebar_path_label_wraps_inside_horizontal_layout() {
        let long_path = "/very/long/path/without/spaces/that/should/not/expand/the/sidebar.txt";

        egui::__run_test_ui(|ui| {
            ui.set_width(132.0);
            ui.horizontal(|ui| {
                ui.add_space(SIDEBAR_CONTENT_INDENT);
                let wrap_width = sidebar_path_wrap_width(ui.available_width());
                let response = ui.add(sidebar_path_label(long_path, wrap_width));

                assert!(
                    response.rect.width() <= wrap_width + 1.0,
                    "path label width {} should stay within wrap width {}",
                    response.rect.width(),
                    wrap_width
                );
            });
        });
    }
}
