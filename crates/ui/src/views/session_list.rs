use egui_extras::{Column, TableBuilder};

use crate::app::Action;
use crate::data::{Db, SessionRow};
use crate::format::{format_celsius, format_datetime, format_duration};

/// How often the list checks whether the agent has written to the
/// database. egui only repaints on input, so without a scheduled repaint the
/// check would only run when the mouse moves over the window.
const CHANGE_POLL: std::time::Duration = std::time::Duration::from_secs(1);

pub struct SessionsView {
    rows: Vec<SessionRow>,
    /// `Db::data_version` as of the last `refresh`. The list used to load
    /// only when the view was opened, so a session recorded while it was on
    /// screen didn't appear until you navigated away and back.
    seen_version: Option<i64>,
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
            seen_version: None,
            load_error: None,
            renaming: None,
            delete_confirm: None,
        }
    }

    fn refresh(&mut self, db: &Db) {
        // Read before listing: a commit landing between the two then shows
        // up as a change on the next check, rather than being missed.
        self.seen_version = db.data_version().ok();
        match db.list_session_rows() {
            Ok(rows) => {
                self.rows = rows;
                self.load_error = None;
            }
            Err(e) => self.load_error = Some(format!("{e:#}")),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, db: &Db) -> Option<Action> {
        // The agent writes a session row when recording starts, its samples
        // at every flush (10 s by default), and its end on stop; each of
        // those changes the version. Checking it is a single PRAGMA, so the
        // list (one summary query per session) is only reloaded on a change.
        ui.ctx().request_repaint_after(CHANGE_POLL);
        if db.data_version().ok() != self.seen_version {
            self.refresh(db);
        }

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
            .column(Column::auto().at_least(130.0)) // peak mem: "22848 MB (35%)"
            .column(Column::auto().at_least(150.0)) // peak temp: "GPU 72 °C, Board 28 °C"
            .column(Column::auto().at_least(60.0)) // events
            // Actions. Wide enough up front for the widest state (Open,
            // Rename, Confirm delete, Cancel ≈ 295 pt): sized only for the
            // normal three buttons, this auto column grew when a row entered
            // delete-confirm, squeezing the Label column and reflowing the
            // whole table under the cursor mid-delete.
            .column(Column::auto().at_least(300.0))
            .header(20.0, |mut header| {
                for label in [
                    "Started",
                    "Duration",
                    "Label",
                    "Avg CPU",
                    "Peak CPU",
                    "Peak Mem",
                    "Peak Temp",
                    "Events",
                    "",
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
                                match (row.summary.peak_mem_used_mb, row.summary.peak_mem_used_pct)
                                {
                                    (Some(mb), Some(pct)) => format!("{mb:.0} MB ({pct:.0}%)"),
                                    (Some(mb), None) => format!("{mb:.0} MB"),
                                    (None, _) => "—".into(),
                                },
                            );
                        });
                        tr.col(|ui| {
                            ui.label(format_peak_temps(&row.summary.peak_temps));
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

/// `GPU 72 °C, Board 28 °C`, hottest kind first (the order the summary
/// query returns), or `—` for a session without temperatures.
fn format_peak_temps(peaks: &[(storage::SensorKind, f64)]) -> String {
    if peaks.is_empty() {
        return "—".into();
    }
    peaks
        .iter()
        .map(|(kind, c)| format!("{} {}", kind.label(), format_celsius(*c)))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::SensorKind;

    #[test]
    fn peak_temps_list_each_kind_or_a_dash() {
        assert_eq!(format_peak_temps(&[]), "—");
        assert_eq!(
            format_peak_temps(&[(SensorKind::Gpu, 71.6), (SensorKind::Board, 27.85)]),
            "GPU 72 °C, Board 28 °C"
        );
    }
}
