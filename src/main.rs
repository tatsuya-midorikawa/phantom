use std::path::{Path, PathBuf};

use eframe::egui::{self, Align, Layout, TextEdit};
use phantom::{load_document, replacement_guard, save_document, EditorDocument, ReplacementGuard};
use rfd::FileDialog;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("phantom")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };

    eframe::run_native(
        "phantom",
        native_options,
        Box::new(|_creation_context| Ok(Box::<PhantomApp>::default())),
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

struct PhantomApp {
    document: EditorDocument,
    path_input: String,
    message: AppMessage,
}

impl Default for PhantomApp {
    fn default() -> Self {
        Self {
            document: EditorDocument::untitled(),
            path_input: String::new(),
            message: AppMessage::Ready,
        }
    }
}

impl eframe::App for PhantomApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.guard_close_request(context);
        context.send_viewport_cmd(egui::ViewportCommand::Title(self.document.window_title()));

        egui::TopBottomPanel::top("toolbar").show(context, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("New").clicked() {
                    self.new_document();
                }

                if ui.button("Open").clicked() {
                    self.open_document_with_dialog();
                }

                if ui.button("Save").clicked() {
                    self.save_document();
                }

                if ui.button("Save As").clicked() {
                    self.save_document_with_dialog();
                }

                ui.separator();
                ui.label("Path");
                ui.add(
                    TextEdit::singleline(&mut self.path_input)
                        .desired_width(420.0)
                        .hint_text("/path/to/file.txt"),
                );
            });
        });

        egui::CentralPanel::default().show(context, |ui| {
            let editor = TextEdit::multiline(self.document.text_mut())
                .desired_width(f32::INFINITY)
                .lock_focus(true)
                .code_editor();

            if ui.add_sized(ui.available_size(), editor).changed() {
                self.document.record_text_change();
            }
        });

        egui::TopBottomPanel::bottom("status_bar").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.label(self.document_label());
                ui.separator();

                let metrics = self.document.metrics();
                ui.label(format!(
                    "{} lines | {} chars | {} bytes",
                    metrics.visual_lines, metrics.characters, metrics.bytes
                ));

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.message.is_error() {
                        ui.colored_label(ui.visuals().error_fg_color, self.message.text());
                    } else {
                        ui.label(self.message.text());
                    }
                });
            });
        });
    }
}

impl PhantomApp {
    fn new_document(&mut self) {
        if !self.can_replace_document("creating a new document") {
            return;
        }

        self.document = EditorDocument::untitled();
        self.path_input.clear();
        self.message = AppMessage::Info("New document".to_owned());
    }

    fn open_document_with_dialog(&mut self) {
        if !self.can_replace_document("opening another file") {
            return;
        }

        match self.open_file_dialog().pick_file() {
            Some(path) => self.open_document_at(path),
            None => {
                self.message = AppMessage::Info("Open canceled".to_owned());
            }
        }
    }

    fn open_document_at(&mut self, path: PathBuf) {
        match load_document(&path) {
            Ok(document) => {
                self.document = document;
                self.path_input = path.display().to_string();
                self.message = AppMessage::Info("Opened".to_owned());
            }
            Err(error) => {
                self.message = AppMessage::Error(format!("Open failed: {error}"));
            }
        }
    }

    fn save_document(&mut self) {
        if let Some(path) = self.save_target_path() {
            self.save_document_at(path);
        } else {
            self.save_document_with_dialog();
        }
    }

    fn save_document_with_dialog(&mut self) {
        match self.save_file_dialog().save_file() {
            Some(path) => self.save_document_at(path),
            None => {
                self.message = AppMessage::Info("Save canceled".to_owned());
            }
        }
    }

    fn save_document_at(&mut self, path: PathBuf) {
        match save_document(&mut self.document, &path) {
            Ok(()) => {
                self.path_input = path.display().to_string();
                self.message = AppMessage::Info("Saved".to_owned());
            }
            Err(error) => {
                self.message = AppMessage::Error(format!("Save failed: {error}"));
            }
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

    fn document_path(&self) -> Option<PathBuf> {
        self.document.path().map(Path::to_path_buf)
    }

    fn save_target_path(&self) -> Option<PathBuf> {
        self.document_path().or_else(|| self.path_from_input())
    }

    fn document_label(&self) -> String {
        let name = self
            .document
            .path()
            .and_then(Path::file_name)
            .and_then(|file_name| file_name.to_str())
            .unwrap_or("Untitled");

        if self.document.is_dirty() {
            format!("{name} *")
        } else {
            name.to_owned()
        }
    }

    fn can_replace_document(&mut self, action: &str) -> bool {
        match replacement_guard(&self.document) {
            ReplacementGuard::SafeToReplace => true,
            ReplacementGuard::BlockedByUnsavedChanges => {
                self.message =
                    AppMessage::Error(format!("Save the current document before {action}"));
                false
            }
        }
    }

    fn should_block_close(&self) -> bool {
        matches!(
            replacement_guard(&self.document),
            ReplacementGuard::BlockedByUnsavedChanges
        )
    }

    fn guard_close_request(&mut self, context: &egui::Context) {
        if context.input(|input| input.viewport().close_requested()) && self.should_block_close() {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.message = AppMessage::Error("Save the current document before closing".to_owned());
        }
    }
}

fn parent_directory(path: &Path) -> Option<PathBuf> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
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
            document: EditorDocument::from_saved_text(PathBuf::from("notes.md"), String::new()),
            ..Default::default()
        };

        assert_eq!(app.save_dialog_file_name(), "notes.md");
    }

    #[test]
    fn dialog_start_directory_prefers_path_input_parent() {
        let directory = std::env::temp_dir();
        let app = PhantomApp {
            document: EditorDocument::from_saved_text(PathBuf::from("document.txt"), String::new()),
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
            document: EditorDocument::from_saved_text(document_path.clone(), String::new()),
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
            document,
            ..Default::default()
        };

        assert!(app.should_block_close());
    }
}
