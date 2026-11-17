use std::path::PathBuf;

use crate::data::Db;
use crate::views::{session_detail, session_list, settings};

pub enum View {
    Sessions(session_list::SessionsView),
    Detail(session_detail::DetailView),
    Settings(settings::SettingsView),
}

/// What a view asked the shell to do after handling a frame's input —
/// views don't mutate `App::view` directly so they don't need a reference
/// back to the enclosing enum.
pub enum Action {
    OpenDetail(i64),
    OpenSettings,
    BackToSessions,
}

pub struct GhostPerfApp {
    db: Option<Db>,
    db_path: PathBuf,
    db_open_error: Option<String>,
    view: View,
}

impl GhostPerfApp {
    pub fn new(db_path: PathBuf) -> Self {
        let (db, db_open_error) = match Db::open(&db_path) {
            Ok(db) => (Some(db), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        let view = match &db {
            Some(db) => View::Sessions(session_list::SessionsView::new(db)),
            None => View::Sessions(session_list::SessionsView::empty()),
        };
        Self {
            db,
            db_path,
            db_open_error,
            view,
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::OpenDetail(id) => {
                if let Some(db) = &self.db {
                    match session_detail::DetailView::load(db, id) {
                        Ok(detail) => self.view = View::Detail(detail),
                        Err(e) => log::error!("failed to open session {id}: {e:#}"),
                    }
                }
            }
            Action::OpenSettings => {
                self.view = View::Settings(settings::SettingsView::load());
            }
            Action::BackToSessions => {
                self.view = View::Sessions(match &self.db {
                    Some(db) => session_list::SessionsView::new(db),
                    None => session_list::SessionsView::empty(),
                });
            }
        }
    }
}

impl eframe::App for GhostPerfApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("GhostPerf");
                ui.separator();
                if ui.button("Sessions").clicked() {
                    self.apply(Action::BackToSessions);
                }
                if ui.button("Settings").clicked() {
                    self.apply(Action::OpenSettings);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(self.db_path.display().to_string());
                });
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(err) = &self.db_open_error {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("Couldn't open {}: {err}", self.db_path.display()),
                );
                ui.label("Start a recording from the tray first, or check Settings for the configured DB location.");
                return;
            }

            let Some(db) = &self.db else { return };
            let action = egui::ScrollArea::vertical()
                .show(ui, |ui| match &mut self.view {
                    View::Sessions(v) => v.ui(ui, db),
                    View::Detail(v) => v.ui(ui, db),
                    View::Settings(v) => v.ui(ui),
                })
                .inner;
            if let Some(action) = action {
                self.apply(action);
            }
        });
    }
}
