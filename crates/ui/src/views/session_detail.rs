use egui_plot::{Legend, Line, Plot, PlotPoints, VLine};
use storage::{Event, MetricSample, Session};

use crate::app::Action;
use crate::data::Db;
use crate::export;
use crate::format::{format_datetime, format_duration};
use crate::process_agg::aggregate_processes;

/// Color per event `kind`, shared between the chart markers and the legend
/// so a stall always looks like a stall across the view.
fn event_color(kind: &str) -> egui::Color32 {
    match kind {
        "stall" => egui::Color32::from_rgb(255, 165, 0),
        "cpu_spike" => egui::Color32::RED,
        "mem_spike" => egui::Color32::from_rgb(200, 0, 200),
        k if k.starts_with("eventlog_") => egui::Color32::GRAY,
        _ => egui::Color32::LIGHT_BLUE,
    }
}

pub struct DetailView {
    session: Session,
    metrics: Vec<MetricSample>,
    events: Vec<Event>,
    /// Brush-selected window, in seconds-since-session-start (the same unit
    /// used for the plots' x axis) — `None` means "whole session".
    selection: Option<(f64, f64)>,
    drag_start: Option<f64>,
    label_edit: String,
    export_path: String,
    export_as_json: bool,
    status: Option<String>,
    delete_confirm: bool,
}

impl DetailView {
    pub fn load(db: &Db, id: i64) -> anyhow::Result<Self> {
        let session = db
            .session(id)?
            .ok_or_else(|| anyhow::anyhow!("session {id} not found"))?;
        let metrics = db.metrics(id)?;
        let events = db.events(id)?;
        let label_edit = session.label.clone().unwrap_or_default();
        let default_ext = "csv";
        let export_path = agent_config::data_dir()
            .join("exports")
            .join(format!("session-{id}.{default_ext}"))
            .display()
            .to_string();
        Ok(Self {
            session,
            metrics,
            events,
            selection: None,
            drag_start: None,
            label_edit,
            export_path,
            export_as_json: false,
            status: None,
            delete_confirm: false,
        })
    }

    /// Selection (seconds-since-start) converted back to an absolute
    /// millisecond `ts` range for filtering samples.
    fn selection_ts_range(&self) -> Option<(i64, i64)> {
        self.selection.map(|(a, b)| {
            (
                self.session.started_at + (a * 1000.0) as i64,
                self.session.started_at + (b * 1000.0) as i64,
            )
        })
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, db: &Db) -> Option<Action> {
        let mut action = None;

        ui.horizontal(|ui| {
            if ui.button("← Sessions").clicked() {
                action = Some(Action::BackToSessions);
            }
            ui.separator();
            ui.label(format!("Session #{}", self.session.id));
            ui.label(format_datetime(self.session.started_at));
            ui.label(format_duration(
                self.session.started_at,
                self.session.ended_at,
            ));
        });

        ui.horizontal(|ui| {
            ui.label("Label:");
            ui.text_edit_singleline(&mut self.label_edit);
            if ui.button("Save").clicked() {
                let label = (!self.label_edit.trim().is_empty())
                    .then(|| self.label_edit.trim().to_string());
                if let Err(e) = db.rename(self.session.id, label.as_deref()) {
                    self.status = Some(format!("rename failed: {e:#}"));
                } else {
                    self.session.label = label;
                }
            }
            ui.separator();
            if self.delete_confirm {
                if ui.button("Confirm delete").clicked() {
                    if let Err(e) = db.delete(self.session.id) {
                        self.status = Some(format!("delete failed: {e:#}"));
                        self.delete_confirm = false;
                    } else {
                        action = Some(Action::BackToSessions);
                    }
                }
                if ui.button("Cancel").clicked() {
                    self.delete_confirm = false;
                }
            } else if ui.button("Delete session").clicked() {
                self.delete_confirm = true;
            }
        });

        if let Some(status) = &self.status {
            ui.colored_label(egui::Color32::YELLOW, status);
        }

        if self.metrics.is_empty() {
            ui.label("No samples recorded for this session.");
            return action;
        }

        ui.separator();
        self.charts(ui);

        ui.separator();
        ui.horizontal(|ui| {
            if let Some((a, b)) = self.selection {
                ui.label(format!("Selected: {a:.1}s – {b:.1}s"));
                if ui.button("Clear selection").clicked() {
                    self.selection = None;
                }
            } else {
                ui.weak("Drag on the overview strip below the charts to select a time range.");
            }
        });

        ui.separator();
        self.process_table(ui);

        ui.separator();
        self.export_controls(ui);

        action
    }

    fn charts(&mut self, ui: &mut egui::Ui) {
        let link_group = egui::Id::unique(("ghostperf-link", self.session.id));
        let events = &self.events;
        let seconds_of = |ts: i64| (ts - self.session.started_at) as f64 / 1000.0;

        let cpu_points: PlotPoints = self
            .metrics
            .iter()
            .map(|m| [seconds_of(m.ts), m.cpu_pct as f64])
            .collect();
        let mem_points: PlotPoints = self
            .metrics
            .iter()
            .map(|m| [seconds_of(m.ts), m.mem_used_mb as f64])
            .collect();
        let disk_read_points: PlotPoints = self
            .metrics
            .iter()
            .map(|m| [seconds_of(m.ts), m.disk_read_bps])
            .collect();
        let disk_write_points: PlotPoints = self
            .metrics
            .iter()
            .map(|m| [seconds_of(m.ts), m.disk_write_bps])
            .collect();
        let net_rx_points: PlotPoints = self
            .metrics
            .iter()
            .map(|m| [seconds_of(m.ts), m.net_rx_bps])
            .collect();
        let net_tx_points: PlotPoints = self
            .metrics
            .iter()
            .map(|m| [seconds_of(m.ts), m.net_tx_bps])
            .collect();

        let plot_height = 140.0;
        // Scroll-wheel is left to the page's outer ScrollArea, not the
        // individual plots: this whole view can be taller than the window,
        // and a plot capturing the wheel while the user scrolls past it
        // pans the plot into a nonsensical range instead of scrolling the
        // page. Left-drag-to-pan and right-drag-to-box-zoom (both on by
        // default) still cover "zoom/pan on charts" without the wheel.
        let add_events = |plot_ui: &mut egui_plot::PlotUi| {
            for e in events {
                plot_ui.vline(
                    VLine::new(e.kind.clone(), seconds_of(e.ts)).color(event_color(&e.kind)),
                );
            }
        };

        Plot::new(("cpu", self.session.id))
            .height(plot_height)
            .allow_scroll(false)
            .legend(Legend::default())
            .link_axis(link_group, [true, false])
            .link_cursor(link_group, [true, false])
            .show(ui, |plot_ui| {
                plot_ui.line(Line::new("CPU %", cpu_points));
                add_events(plot_ui);
            });

        Plot::new(("mem", self.session.id))
            .height(plot_height)
            .allow_scroll(false)
            .legend(Legend::default())
            .link_axis(link_group, [true, false])
            .link_cursor(link_group, [true, false])
            .show(ui, |plot_ui| {
                plot_ui.line(Line::new("Mem used (MB)", mem_points));
                add_events(plot_ui);
            });

        Plot::new(("disk", self.session.id))
            .height(plot_height)
            .allow_scroll(false)
            .legend(Legend::default())
            .link_axis(link_group, [true, false])
            .link_cursor(link_group, [true, false])
            .show(ui, |plot_ui| {
                plot_ui.line(Line::new("Read B/s", disk_read_points));
                plot_ui.line(Line::new("Write B/s", disk_write_points));
                add_events(plot_ui);
            });

        Plot::new(("net", self.session.id))
            .height(plot_height)
            .allow_scroll(false)
            .legend(Legend::default())
            .link_axis(link_group, [true, false])
            .link_cursor(link_group, [true, false])
            .show(ui, |plot_ui| {
                plot_ui.line(Line::new("Rx B/s", net_rx_points));
                plot_ui.line(Line::new("Tx B/s", net_tx_points));
                add_events(plot_ui);
            });

        // Overview/selector strip: drag is reserved entirely for brush
        // selection (see `drag_start`/`selection`), so pan/zoom/box-zoom are
        // all disabled here — the 4 charts above already provide those via
        // their native egui_plot interactions.
        let overview_points: PlotPoints = self
            .metrics
            .iter()
            .map(|m| [seconds_of(m.ts), m.cpu_pct as f64])
            .collect();
        let response = Plot::new(("overview", self.session.id))
            .height(60.0)
            .show_axes([true, false])
            .show_grid([true, false])
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .link_axis(link_group, [true, false])
            .show(ui, |plot_ui| {
                plot_ui.line(Line::new("CPU %", overview_points));
                if let Some((a, b)) = self.selection {
                    plot_ui.vline(VLine::new("start", a).color(egui::Color32::YELLOW));
                    plot_ui.vline(VLine::new("end", b).color(egui::Color32::YELLOW));
                }

                let plot_response = plot_ui.response();
                if plot_response.drag_started() {
                    self.drag_start = plot_ui.pointer_coordinate().map(|p| p.x);
                }
                if plot_response.dragged() {
                    if let (Some(start), Some(cur)) =
                        (self.drag_start, plot_ui.pointer_coordinate())
                    {
                        self.selection = Some(if start <= cur.x {
                            (start, cur.x)
                        } else {
                            (cur.x, start)
                        });
                    }
                }
                if plot_response.drag_stopped() {
                    self.drag_start = None;
                }
                if plot_response.double_clicked() {
                    self.selection = None;
                }
            });
        response
            .response
            .on_hover_text("Drag to select a time range");
    }

    fn process_table(&self, ui: &mut egui::Ui) {
        let mut procs = aggregate_processes(&self.metrics, self.selection_ts_range());
        procs.sort_by(|a, b| b.peak_cpu_pct.total_cmp(&a.peak_cpu_pct));

        ui.label("Per-process breakdown (CPU normalized to whole-machine scale):");
        egui_extras::TableBuilder::new(ui)
            .striped(true)
            .column(egui_extras::Column::auto().at_least(60.0))
            .column(egui_extras::Column::remainder().at_least(150.0))
            .column(egui_extras::Column::auto().at_least(80.0))
            .column(egui_extras::Column::auto().at_least(80.0))
            .column(egui_extras::Column::auto().at_least(90.0))
            .column(egui_extras::Column::auto().at_least(70.0))
            .column(egui_extras::Column::auto().at_least(70.0))
            .header(20.0, |mut header| {
                for label in [
                    "PID", "Name", "Avg CPU", "Peak CPU", "Peak Mem", "Handles", "Threads",
                ] {
                    header.col(|ui| {
                        ui.strong(label);
                    });
                }
            })
            .body(|mut body| {
                for p in &procs {
                    body.row(20.0, |mut tr| {
                        tr.col(|ui| {
                            ui.label(p.pid.to_string());
                        });
                        tr.col(|ui| {
                            ui.label(&p.name);
                        });
                        tr.col(|ui| {
                            ui.label(format!("{:.1}%", p.avg_cpu_pct));
                        });
                        tr.col(|ui| {
                            ui.label(format!("{:.1}%", p.peak_cpu_pct));
                        });
                        tr.col(|ui| {
                            ui.label(format!("{:.0} MB", p.peak_mem_mb));
                        });
                        tr.col(|ui| {
                            ui.label(
                                p.peak_handle_count
                                    .map_or_else(|| "—".into(), |v| v.to_string()),
                            );
                        });
                        tr.col(|ui| {
                            ui.label(
                                p.peak_thread_count
                                    .map_or_else(|| "—".into(), |v| v.to_string()),
                            );
                        });
                    });
                }
            });
    }

    fn export_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Export to:");
            ui.text_edit_singleline(&mut self.export_path);
            ui.radio_value(&mut self.export_as_json, false, "CSV");
            ui.radio_value(&mut self.export_as_json, true, "JSON");
            if ui.button("Export").clicked() {
                self.status = Some(self.do_export());
            }
        });
    }

    fn do_export(&self) -> String {
        let path = std::path::Path::new(&self.export_path);
        let result = (|| -> anyhow::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::File::create(path)?;
            if self.export_as_json {
                export::export_json(&self.session, &self.metrics, &self.events, file)?;
            } else {
                export::export_csv(&self.metrics, file)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => format!("Exported to {}", path.display()),
            Err(e) => format!("Export failed: {e:#}"),
        }
    }
}
