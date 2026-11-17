use egui_extras::{Column, TableBuilder};

use crate::app::Action;
use crate::data::{Db, SessionRow};
use crate::format::{format_datetime, format_duration};

pub struct SessionsView {
    rows: Vec<SessionRow>,
    load_error: Option<String>,
    /// `(session_id, in-progress label text)` — only one row editable at a time.
    renaming: Option<(i64, String)>,
    /// Session id awaiting a second click to actually delete.
    delete_confirm: Option<i64>,
}

impl SessionsView {
    pub fn new(db: &Db) -> Self {
        let mut view = Self::empty();
        view.refresh(db);
        view
    }

    pub fn empty() -> Self {
        Self {
            rows: Vec::new(),
            load_error: None,
            renaming: None,
            delete_confirm: None,
        }
    }

    fn refresh(&mut self, db: &Db) {
        match db.list_session_rows() {
            Ok(rows) => {
                self.rows = rows;
                self.load_error = None;
            }
            Err(e) => self.load_error = Some(format!("{e:#}")),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, db: &Db) -> Option<Action> {
        if let Some(err) = &self.load_error {
            ui.colored_label(egui::Color32::RED, err);
        }
        if self.rows.is_empty() && self.load_error.is_none() {
            ui.label("No sessions recorded yet. Start one from the tray.");
            return None;
        }

        let mut action = None;
        let mut dirty = false;

        TableBuilder::new(ui)
            .striped(true)
            .column(Column::auto().at_least(140.0)) // date
            .column(Column::auto().at_least(70.0)) // duration
            .column(Column::remainder().at_least(120.0)) // label
            .column(Column::auto().at_least(70.0)) // avg cpu
            .column(Column::auto().at_least(70.0)) // peak cpu
            .column(Column::auto().at_least(90.0)) // peak mem
            .column(Column::auto().at_least(60.0)) // events
            .column(Column::auto().at_least(170.0)) // actions
            .header(20.0, |mut header| {
                for label in [
                    "Started", "Duration", "Label", "Avg CPU", "Peak CPU", "Peak Mem", "Events", "",
                ] {
                    header.col(|ui| {
                        ui.strong(label);
                    });
                }
            })
            .body(|mut body| {
                for row in &self.rows {
                    let id = row.session.id;
                    body.row(22.0, |mut tr| {
                        tr.col(|ui| {
                            ui.label(format_datetime(row.session.started_at));
                        });
                        tr.col(|ui| {
                            ui.label(format_duration(
                                row.session.started_at,
                                row.session.ended_at,
                            ));
                        });
                        tr.col(|ui| {
                            if let Some((editing_id, text)) = &mut self.renaming {
                                if *editing_id == id {
                                    let resp = ui.text_edit_singleline(text);
                                    if resp.lost_focus()
                                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                    {
                                        let label = (!text.trim().is_empty())
                                            .then(|| text.trim().to_string());
                                        if let Err(e) = db.rename(id, label.as_deref()) {
                                            log::error!("rename failed: {e:#}");
                                        }
                                        self.renaming = None;
                                        dirty = true;
                                    }
                                    return;
                                }
                            }
                            ui.label(row.session.label.as_deref().unwrap_or("—"));
                        });
                        tr.col(|ui| {
                            ui.label(
                                row.summary
                                    .avg_cpu_pct
                                    .map_or_else(|| "—".into(), |v| format!("{v:.1}%")),
                            );
                        });
                        tr.col(|ui| {
                            ui.label(
                                row.summary
                                    .peak_cpu_pct
                                    .map_or_else(|| "—".into(), |v| format!("{v:.1}%")),
                            );
                        });
                        tr.col(|ui| {
                            ui.label(
                                row.summary
                                    .peak_mem_used_mb
                                    .map_or_else(|| "—".into(), |v| format!("{v:.0} MB")),
                            );
                        });
                        tr.col(|ui| {
                            let total: i64 = row.summary.event_counts.iter().map(|(_, c)| c).sum();
                            ui.label(total.to_string());
                        });
                        tr.col(|ui| {
                            ui.horizontal(|ui| {
                                if ui.button("Open").clicked() {
                                    action = Some(Action::OpenDetail(id));
                                }
                                if ui.button("Rename").clicked() {
                                    self.renaming =
                                        Some((id, row.session.label.clone().unwrap_or_default()));
                                }
                                if self.delete_confirm == Some(id) {
                                    if ui.button("Confirm delete").clicked() {
                                        if let Err(e) = db.delete(id) {
                                            log::error!("delete failed: {e:#}");
                                        }
                                        self.delete_confirm = None;
                                        dirty = true;
                                    }
                                    if ui.button("Cancel").clicked() {
                                        self.delete_confirm = None;
                                    }
                                } else if ui.button("Delete").clicked() {
                                    self.delete_confirm = Some(id);
                                }
                            });
                        });
                    });
                }
            });

        if dirty {
            self.refresh(db);
        }
        action
    }
}
