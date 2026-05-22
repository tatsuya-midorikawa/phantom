use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui::{
    self, text::LayoutJob, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Layout,
    Margin, RichText, Sense, Stroke, TextEdit, TextFormat, Vec2,
};
use phantom::{
    open_text_document, save_document, save_large_document, EditorDocument, LargeDocument,
    OpenedDocument,
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
const SIDEBAR_CONTENT_INDENT: f32 = 12.0;
const SIDEBAR_PATH_MIN_WRAP_WIDTH: f32 = 48.0;
const EDITOR_GUTTER_WIDTH: f32 = 72.0;
const EDITOR_GUTTER_GAP: f32 = 10.0;
const EDITOR_MIN_TEXT_WIDTH: f32 = 360.0;
const EDITOR_MONOSPACE_CHAR_WIDTH: f32 = 9.0;
const EDITOR_ROW_HEIGHT: f32 = 22.0;

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

#[derive(Debug, Clone, PartialEq, Eq)]
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

struct PhantomApp {
    document: ActiveDocument,
    path_input: String,
    message: AppMessage,
    active_view: SidebarView,
    sidebar_visible: bool,
    wrap_lines: bool,
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
        context.send_viewport_cmd(egui::ViewportCommand::Title(self.document.window_title()));

        self.show_menu_bar(context);
        self.show_status_bar(context);
        self.show_activity_bar(context);

        if self.sidebar_visible {
            self.show_sidebar(context);
        }

        self.show_tab_bar(context);
        self.show_editor(context);
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

                        ui.menu_button("View", |ui| {
                            ui.checkbox(&mut self.sidebar_visible, "Show Sidebar");
                            ui.checkbox(&mut self.wrap_lines, "Wrap Lines");
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
                    SidebarView::Search => self.show_search_placeholder(ui),
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

    fn show_search_placeholder(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new("Search indexing runs outside the editor thread.")
                    .color(VSCODE_TEXT_DIM),
            );
        });
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

                let large_editor_action = match &mut self.document {
                    ActiveDocument::Inline(document) => {
                        let wrap_lines = self.wrap_lines;
                        let available_width = ui.available_width();
                        let editor_width = editor_content_width_for_text(
                            document.text(),
                            available_width,
                            wrap_lines,
                        );
                        let scroll_area = if wrap_lines {
                            egui::ScrollArea::vertical()
                        } else {
                            egui::ScrollArea::both()
                        };

                        scroll_area.auto_shrink([false, false]).show(ui, |ui| {
                            ui.set_min_width(editor_width);
                            let editor = editor_widget(document.text_mut(), editor_width);

                            if ui.add_sized(ui.available_size(), editor).changed() {
                                document.record_text_change();
                            }
                        });
                        None
                    }
                    ActiveDocument::Large(document) => {
                        show_large_virtual_editor(ui, document, self.wrap_lines)
                    }
                };

                if let Some(action) = large_editor_action {
                    self.apply_large_editor_action(action);
                }
            });
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
            LargeEditorAction::Message(message) => self.message = message,
            LargeEditorAction::LoadLine(line_index) => self.move_large_viewport_to_line(line_index),
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
    let content_width =
        editor_content_width_for_text(document.viewport_text(), available_width, wrap_lines)
            .max(available_width);
    let text_width = editor_text_width(content_width);
    let row_height = editor_row_height(document.viewport_text(), text_width, wrap_lines);
    let line_count = document.file_line_count().max(1);
    let mut action = None;
    let scroll_area = if wrap_lines {
        egui::ScrollArea::vertical()
    } else {
        egui::ScrollArea::both()
    };

    scroll_area.auto_shrink([false, false]).show_rows(
        ui,
        row_height,
        line_count,
        |ui, row_range| {
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

            for line_index in row_range {
                if !document.contains_file_line(line_index) {
                    show_loading_line(ui, line_index, row_height, text_width);
                    continue;
                }

                if let Some(line_message) =
                    show_virtual_line(ui, document, line_index, row_height, text_width, wrap_lines)
                {
                    action = Some(LargeEditorAction::Message(line_message));
                }
            }
        },
    );

    action
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
            line_number_rect.right_center(),
            Align2::RIGHT_CENTER,
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

fn show_virtual_line(
    ui: &mut egui::Ui,
    document: &mut LargeDocument,
    line_index: usize,
    row_height: f32,
    text_width: f32,
    wrap_lines: bool,
) -> Option<AppMessage> {
    let mut message = None;

    ui.horizontal(|ui| {
        let (line_number_rect, _) =
            ui.allocate_exact_size(Vec2::new(EDITOR_GUTTER_WIDTH, row_height), Sense::hover());
        ui.painter().text(
            line_number_rect.right_center(),
            Align2::RIGHT_CENTER,
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

        let response = ui
            .add_enabled_ui(editable, |ui| {
                add_line_editor(ui, &mut line_text, text_width, row_height, wrap_lines)
            })
            .inner;

        if editable && response.changed() {
            message = match document.replace_file_line(line_index, &line_text) {
                Ok(true) => Some(AppMessage::Info(format!("Edited line {}", line_index + 1))),
                Ok(false) => None,
                Err(error) => Some(AppMessage::Error(format!("Edit failed: {error}"))),
            };
        }
    });

    message
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

fn longest_line_character_count(text: &str) -> usize {
    text.lines()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
}

fn editor_text_width_for_characters(character_count: usize) -> f32 {
    (character_count as f32 * EDITOR_MONOSPACE_CHAR_WIDTH)
        .max(EDITOR_MIN_TEXT_WIDTH)
        .ceil()
}

fn editor_text_width(content_width: f32) -> f32 {
    (content_width - EDITOR_GUTTER_WIDTH - EDITOR_GUTTER_GAP).max(EDITOR_MIN_TEXT_WIDTH)
}

fn editor_content_width_for_text(text: &str, available_width: f32, wrap_lines: bool) -> f32 {
    let available_width = if available_width.is_finite() {
        available_width.max(EDITOR_MIN_TEXT_WIDTH)
    } else {
        EDITOR_MIN_TEXT_WIDTH
    };

    if wrap_lines {
        return available_width;
    }

    let text_width = editor_text_width_for_characters(longest_line_character_count(text));

    (EDITOR_GUTTER_WIDTH + EDITOR_GUTTER_GAP + text_width).max(available_width)
}

fn wrapped_visual_rows(character_count: usize, text_width: f32) -> usize {
    let columns = (text_width / EDITOR_MONOSPACE_CHAR_WIDTH).floor().max(1.0) as usize;

    character_count.div_ceil(columns).max(1)
}

fn editor_row_height(text: &str, text_width: f32, wrap_lines: bool) -> f32 {
    if !wrap_lines {
        return EDITOR_ROW_HEIGHT;
    }

    let row_count = text
        .lines()
        .map(|line| wrapped_visual_rows(line.chars().count(), text_width))
        .max()
        .unwrap_or(1);

    EDITOR_ROW_HEIGHT * row_count as f32
}

fn editor_widget(text: &mut String, width: f32) -> TextEdit<'_> {
    TextEdit::multiline(text)
        .font(FontId::new(14.0, FontFamily::Monospace))
        .text_color(VSCODE_TEXT)
        .desired_width(width)
        .desired_rows(32)
        .lock_focus(true)
        .frame(false)
}

fn add_line_editor(
    ui: &mut egui::Ui,
    text: &mut String,
    width: f32,
    height: f32,
    wrap_lines: bool,
) -> egui::Response {
    if wrap_lines {
        let mut layouter = |ui: &egui::Ui, text: &str, wrap_width: f32| {
            let mut layout_job = LayoutJob::single_section(
                text.to_owned(),
                TextFormat::simple(FontId::new(14.0, FontFamily::Monospace), VSCODE_TEXT),
            );
            layout_job.wrap.max_width = wrap_width.max(EDITOR_MIN_TEXT_WIDTH);
            layout_job.wrap.break_anywhere = true;

            ui.fonts(|fonts| fonts.layout_job(layout_job))
        };

        return ui.add_sized(
            Vec2::new(width, height),
            line_editor_widget(text, width).layouter(&mut layouter),
        );
    }

    ui.add_sized(Vec2::new(width, height), line_editor_widget(text, width))
}

fn line_editor_widget(text: &mut String, width: f32) -> TextEdit<'_> {
    TextEdit::singleline(text)
        .font(FontId::new(14.0, FontFamily::Monospace))
        .text_color(VSCODE_TEXT)
        .desired_width(width)
        .clip_text(false)
        .frame(false)
}

fn parent_directory(path: &Path) -> Option<PathBuf> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
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
        let width = editor_content_width_for_text(&long_line, 480.0, false);

        assert!(width > 480.0);
        assert_eq!(
            width,
            EDITOR_GUTTER_WIDTH
                + EDITOR_GUTTER_GAP
                + editor_text_width_for_characters(long_line.len())
        );
    }

    #[test]
    fn editor_content_width_stays_within_available_width_when_wrapping() {
        let long_line = "x".repeat(1_000);

        assert_eq!(
            editor_content_width_for_text(&long_line, 480.0, true),
            480.0
        );
    }

    #[test]
    fn editor_row_height_grows_for_wrapped_long_lines() {
        let long_line = "x".repeat(120);
        let text_width = EDITOR_MONOSPACE_CHAR_WIDTH * 40.0;

        assert_eq!(wrapped_visual_rows(long_line.len(), text_width), 3);
        assert_eq!(
            editor_row_height(&long_line, text_width, true),
            EDITOR_ROW_HEIGHT * 3.0
        );
        assert_eq!(
            editor_row_height(&long_line, text_width, false),
            EDITOR_ROW_HEIGHT
        );
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
