use egui_plot::{HoverPosition, Legend, Line, Plot, PlotPoints, VLine};
use storage::{Event, MetricSample, Session};

use crate::app::Action;
use crate::data::Db;
use crate::export;
use crate::format::{
    format_celsius, format_datetime, format_duration, format_mb, format_mhz, format_pct,
    format_queue, format_rate,
};
use crate::process_agg::aggregate_processes;
use crate::temps;

/// One y-axis width (ui points) for every chart *and* the overview strip.
/// `egui_plot` otherwise sizes each plot's y-axis to its own tick labels
/// ("100" vs "20000000"), so each plot area started at a different x and
/// the same timestamp was drawn up to ~50 px apart from one chart to the
/// next (measured on the first Windows run), which defeats reading down
/// the stack. The axis still grows if a label is wider than this, so the
/// `format_*` label functions are kept short enough to fit (see the
/// `format` tests).
const Y_AXIS_WIDTH: f32 = 72.0;
const PLOT_HEIGHT: f32 = 140.0;
const OVERVIEW_HEIGHT: f32 = 60.0;
/// Width of the title column left of every plot. Fixed, and the same for
/// all of them including the overview strip, for the same reason as
/// `Y_AXIS_WIDTH`: the plots must start at the same x. Wide enough for the
/// longest title ("Temperature") on one line.
const TITLE_WIDTH: f32 = 96.0;
/// Height of the one-line row shown in place of a chart with no data.
const UNAVAILABLE_HEIGHT: f32 = 24.0;

const DISK_QUEUE_HINT: &str = "Average number of disk requests waiting or in service, \
    all physical disks. Sustained values above about 2 per disk mean I/O is backing up, \
    even when the byte rates above look low.";
const CPU_PERF_HINT: &str = "Performance: the CPU's speed relative to its base clock \
    (over 100% is turbo; the clock is on hover). Limit: the most the CPU is currently \
    allowed, 100% when nothing holds it back. A limit dropping under load is the CPU \
    throttling for heat or power (marked as cpu_throttle). Windows doesn't say which.";
const TEMP_HINT: &str = "Every temperature sensor Windows exposes without admin rights: \
    GPUs (as Task Manager reads them) and ACPI thermal zones (a motherboard or chassis \
    sensor, never the CPU die). The CPU's own temperature isn't readable this way.";

const PERF_SERIES: &str = "Performance";

/// Lays out one plot with its title in a column to its left, vertically
/// centered, with `hint` on hover. The title used to be only the legend's
/// series name, inside the plot's corner, which is easy to miss when
/// scanning down the stack.
fn titled(
    ui: &mut egui::Ui,
    title: &str,
    hint: &str,
    height: f32,
    add_plot: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(TITLE_WIDTH, height),
            egui::Layout::centered_and_justified(egui::Direction::TopDown),
            |ui| ui.strong(title).on_hover_text(hint),
        );
        add_plot(ui);
    });
}

/// Y grid step sizes for `egui_plot::uniform_grid_spacer`: the smallest of
/// 1, 2, 10, 20, 100, 200, … that's at least `base_step_size` (the plot-unit
/// size of `egui_plot`'s minimum grid spacing), plus the next two.
///
/// Replaces `egui_plot`'s default powers-of-10 grid, whose levels are 10×
/// apart. Tick labels are only drawn where marks are > 20 pt apart, so at
/// some zoom levels the 10s were too dense to label and the 100s were all
/// out of view, and the axis showed no labels at all (box-zooming the CPU
/// chart to ~15–88% did exactly that on the first Windows run). Here
/// consecutive levels are at most 5× apart, so one of them always lands in
/// a labelable spacing. Each step is a multiple of the previous one, so the
/// grid lines still nest evenly.
fn nice_grid_steps(input: egui_plot::GridInput) -> [f64; 3] {
    let base = input.base_step_size.abs().max(f64::MIN_POSITIVE);
    let next = |s: f64| {
        let mantissa = s / 10f64.powi(s.log10().floor() as i32);
        if mantissa < 1.5 {
            s * 2.0
        } else {
            s * 5.0
        }
    };
    let mut step = 10f64.powi(base.log10().floor() as i32);
    while step < base {
        step = next(step);
    }
    let s1 = next(step);
    [step, s1, next(s1)]
}

/// Hover text: series name and value in its unit, then the time. Event
/// markers (`VLine`s, named after their kind) get just their name and time:
/// the "value" of a vertical line is only wherever the cursor happens to be.
fn hover_label(
    pos: &HoverPosition<'_>,
    fmt: fn(f64) -> String,
    event_kinds: &[String],
) -> Option<String> {
    Some(match pos {
        HoverPosition::NearDataPoint {
            plot_name,
            position,
            ..
        } if event_kinds.iter().any(|k| k == plot_name) => {
            format!("{plot_name}\n{:.1} s", position.x)
        }
        HoverPosition::NearDataPoint {
            plot_name,
            position,
            ..
        } => format!("{plot_name}: {}\n{:.1} s", fmt(position.y), position.x),
        HoverPosition::Elsewhere { position } => {
            format!("{}\n{:.1} s", fmt(position.y), position.x)
        }
    })
}

const MEM_SERIES: &str = "Mem used";

/// The memory chart plots percent of total, so the absolute figures that
/// used to be its y-axis live in its hover text instead:
/// `Mem used: 35% (22848 MB of 65329 MB)`. `mem_mb[i]` is `(used, total)`
/// for the `i`th plotted point, which is what `index` refers to.
fn mem_hover_label(
    pos: &HoverPosition<'_>,
    mem_mb: &[(f32, f32)],
    event_kinds: &[String],
) -> Option<String> {
    if let HoverPosition::NearDataPoint {
        plot_name,
        position,
        index,
    } = pos
    {
        if *plot_name == MEM_SERIES {
            if let Some(&(used, total)) = mem_mb.get(*index) {
                return Some(format!(
                    "{MEM_SERIES}: {} ({} of {})\n{:.1} s",
                    format_pct(position.y),
                    format_mb(used as f64),
                    format_mb(total as f64),
                    position.x
                ));
            }
        }
    }
    hover_label(pos, format_pct, event_kinds)
}

/// The CPU performance chart's hover adds the clock behind the percentage:
/// `Performance: 107% (3.85 GHz)`. `freq_mhz[i]` belongs to the `i`th
/// plotted point, as in [`mem_hover_label`].
fn perf_hover_label(
    pos: &HoverPosition<'_>,
    freq_mhz: &[f32],
    event_kinds: &[String],
) -> Option<String> {
    if let HoverPosition::NearDataPoint {
        plot_name,
        position,
        index,
    } = pos
    {
        if *plot_name == PERF_SERIES {
            if let Some(&mhz) = freq_mhz.get(*index) {
                return Some(format!(
                    "{PERF_SERIES}: {} ({})\n{:.1} s",
                    format_pct(position.y),
                    format_mhz(mhz as f64),
                    position.x
                ));
            }
        }
    }
    hover_label(pos, format_pct, event_kinds)
}

/// A one-line row in place of a chart that has nothing to show, so a
/// missing sensor reads as "not available" rather than as a recording fault.
fn unavailable(ui: &mut egui::Ui, title: &str, hint: &str, message: &str) {
    titled(ui, title, hint, UNAVAILABLE_HEIGHT, |ui| {
        ui.weak(message);
    });
}

/// Color per event `kind`, shared between the chart markers and the legend
/// so a stall always looks like a stall across the view.
fn event_color(kind: &str) -> egui::Color32 {
    match kind {
        "stall" => egui::Color32::from_rgb(255, 165, 0),
        "cpu_spike" => egui::Color32::RED,
        "mem_spike" => egui::Color32::from_rgb(200, 0, 200),
        "temp_spike" => egui::Color32::from_rgb(255, 90, 60),
        "cpu_throttle" => egui::Color32::from_rgb(0, 190, 190),
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
    /// Result of the last Export click (`Ok(path)` / `Err(message)`), shown
    /// next to the export controls rather than in `status`; see
    /// `export_controls`.
    export_status: Option<Result<String, String>>,
    /// Rename/delete failures, shown under the header next to those buttons.
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
            export_status: None,
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

        // Export controls aren't drawn here: they live in a bottom panel,
        // always visible like a status bar; see `app.rs`.
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
        // Percent of total (same scale as CPU, and the same definition as the
        // agent's `mem_spike` threshold, so its markers sit where this curve
        // crosses it). Samples without memory figures are skipped, so the
        // absolute (used, total) MB list is built from the same filtered
        // sequence to keep hover indices aligned with plotted points.
        let (mem_points, mem_mb): (Vec<[f64; 2]>, Vec<(f32, f32)>) = self
            .metrics
            .iter()
            .filter_map(|m| {
                let pct = m.mem_used_pct()?;
                Some((
                    [seconds_of(m.ts), pct as f64],
                    (m.mem_used_mb, m.mem_total_mb()),
                ))
            })
            .unzip();
        let mem_points = PlotPoints::from(mem_points);
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
        let disk_queue_points: Vec<[f64; 2]> = self
            .metrics
            .iter()
            .filter_map(|m| Some([seconds_of(m.ts), m.disk_queue_len? as f64]))
            .collect();
        // Phase 8. The clocks are collected from the same filtered sequence
        // as the performance points, so hover indices line up (as for memory).
        let (perf_points, perf_freq_mhz): (Vec<[f64; 2]>, Vec<f32>) = self
            .metrics
            .iter()
            .filter_map(|m| {
                let p = m.cpu_perf?;
                Some(([seconds_of(m.ts), p.perf_pct as f64], p.freq_mhz))
            })
            .unzip();
        let limit_points: Vec<[f64; 2]> = self
            .metrics
            .iter()
            .filter_map(|m| Some([seconds_of(m.ts), m.cpu_perf?.limit_pct as f64]))
            .collect();
        let temp_series = temps::series(&self.metrics, seconds_of);
        let constant_sensors = temps::constant_sensors(&self.metrics);

        let add_events = |plot_ui: &mut egui_plot::PlotUi| {
            for e in events {
                plot_ui.vline(
                    VLine::new(e.kind.clone(), seconds_of(e.ts)).color(event_color(&e.kind)),
                );
            }
        };
        let event_kinds: Vec<String> = events.iter().map(|e| e.kind.clone()).collect();

        // Everything the 4 stacked charts share. Scroll-wheel is left to the
        // page's outer ScrollArea, not the individual plots: this whole view
        // can be taller than the window, and a plot capturing the wheel while
        // the user scrolls past it pans the plot into a nonsensical range
        // instead of scrolling the page. Left-drag-to-pan and
        // right-drag-to-box-zoom (both on by default) still cover "zoom/pan
        // on charts" without the wheel.
        let chart = |id: &'static str, fmt: fn(f64) -> String| {
            let event_kinds = event_kinds.clone();
            Plot::new((id, self.session.id))
                .height(PLOT_HEIGHT)
                .allow_scroll(false)
                .legend(Legend::default())
                .link_axis(link_group, [true, false])
                .link_cursor(link_group, [true, false])
                .y_axis_min_width(Y_AXIS_WIDTH)
                .y_axis_formatter(move |mark, _range| fmt(mark.value))
                .y_grid_spacer(egui_plot::uniform_grid_spacer(nice_grid_steps))
                .label_formatter(move |pos| hover_label(pos, fmt, &event_kinds))
        };

        titled(ui, "CPU", "Whole-machine CPU use", PLOT_HEIGHT, |ui| {
            chart("cpu", format_pct).show(ui, |plot_ui| {
                plot_ui.line(Line::new("CPU", cpu_points));
                add_events(plot_ui);
            });
        });
        if perf_points.is_empty() {
            unavailable(
                ui,
                "CPU speed",
                CPU_PERF_HINT,
                "Not available: Windows' processor performance counters couldn't be read, \
                 or this session was recorded before GhostPerf recorded them.",
            );
        } else {
            let perf_kinds = event_kinds.clone();
            titled(ui, "CPU speed", CPU_PERF_HINT, PLOT_HEIGHT, |ui| {
                chart("cpu_perf", format_pct)
                    .label_formatter(move |pos| perf_hover_label(pos, &perf_freq_mhz, &perf_kinds))
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new(PERF_SERIES, PlotPoints::from(perf_points)));
                        plot_ui.line(Line::new("Limit", PlotPoints::from(limit_points)));
                        add_events(plot_ui);
                    });
            });
        }
        let mem_kinds = event_kinds.clone();
        titled(
            ui,
            "Memory",
            "Memory in use, as a percentage of total (absolute figures on hover)",
            PLOT_HEIGHT,
            |ui| {
                chart("mem", format_pct)
                    .label_formatter(move |pos| mem_hover_label(pos, &mem_mb, &mem_kinds))
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new(MEM_SERIES, mem_points));
                        add_events(plot_ui);
                    });
            },
        );
        titled(
            ui,
            "Disk I/O",
            "Bytes read and written per second, all processes",
            PLOT_HEIGHT,
            |ui| {
                chart("disk", format_rate).show(ui, |plot_ui| {
                    plot_ui.line(Line::new("Disk read", disk_read_points));
                    plot_ui.line(Line::new("Disk write", disk_write_points));
                    add_events(plot_ui);
                });
            },
        );
        // Sessions recorded before the queue length existed (or on a machine
        // without the counter) have none, and an empty chart would only
        // look like a recording fault.
        if !disk_queue_points.is_empty() {
            let disk_queue_points = PlotPoints::from(disk_queue_points);
            titled(ui, "Disk queue", DISK_QUEUE_HINT, PLOT_HEIGHT, |ui| {
                chart("disk_queue", format_queue).show(ui, |plot_ui| {
                    plot_ui.line(Line::new("Disk queue", disk_queue_points));
                    add_events(plot_ui);
                });
            });
        }
        titled(
            ui,
            "Network",
            "Bytes received (rx) and sent (tx) per second, all adapters",
            PLOT_HEIGHT,
            |ui| {
                chart("net", format_rate).show(ui, |plot_ui| {
                    plot_ui.line(Line::new("Net rx", net_rx_points));
                    plot_ui.line(Line::new("Net tx", net_tx_points));
                    add_events(plot_ui);
                });
            },
        );
        if temp_series.is_empty() {
            unavailable(
                ui,
                "Temperature",
                TEMP_HINT,
                "Not available: no temperature sensor was readable while this session \
                 recorded, or it was recorded before GhostPerf recorded temperatures.",
            );
        } else {
            titled(ui, "Temperature", TEMP_HINT, PLOT_HEIGHT, |ui| {
                chart("temp", format_celsius).show(ui, |plot_ui| {
                    for (sensor, _, points) in temp_series {
                        plot_ui.line(Line::new(sensor, PlotPoints::from(points)));
                    }
                    add_events(plot_ui);
                });
            });
            // Under the chart, lined up with the plot area rather than the
            // title or the axis labels.
            for (sensor, value) in &constant_sensors {
                ui.horizontal(|ui| {
                    ui.add_space(TITLE_WIDTH + ui.spacing().item_spacing.x + Y_AXIS_WIDTH);
                    ui.weak(format!(
                        "{sensor} read {value:.2} °C for the whole session: probably a fixed \
                         value from the firmware, not a real sensor."
                    ));
                });
            }
        }

        // Overview/selector strip: drag is reserved entirely for brush
        // selection (see `drag_start`/`selection`), so pan/zoom/box-zoom are
        // all disabled here — the 4 charts above already provide those via
        // their native egui_plot interactions.
        //
        // Deliberately *not* in `link_group`: it's the whole-session map you
        // brush-select on, so it must keep showing the whole session while
        // the charts above are zoomed in (when it was linked, zooming the
        // charts zoomed it too, and you could then only select inside the
        // zoomed range). Its y-axis is shown but blank, at the same width as
        // the charts', purely so its x positions line up with theirs.
        let overview_points: PlotPoints = self
            .metrics
            .iter()
            .map(|m| [seconds_of(m.ts), m.cpu_pct as f64])
            .collect();
        titled(
            ui,
            "Select",
            "CPU over the whole session. Drag here to select a time range for the \
             per-process table; double-click to clear it.",
            OVERVIEW_HEIGHT,
            |ui| self.overview(ui, overview_points),
        );
    }

    fn overview(&mut self, ui: &mut egui::Ui, overview_points: PlotPoints) {
        let response = Plot::new(("overview", self.session.id))
            .height(OVERVIEW_HEIGHT)
            .show_axes([true, true])
            .show_grid([true, false])
            .y_axis_min_width(Y_AXIS_WIDTH)
            .y_axis_formatter(|_mark, _range| String::new())
            .label_formatter(|pos| {
                let x = match pos {
                    HoverPosition::NearDataPoint { position, .. }
                    | HoverPosition::Elsewhere { position } => position.x,
                };
                Some(format!("{x:.1} s"))
            })
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
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

    /// The export bar. Drawn by `app.rs` in a bottom panel outside the page's
    /// ScrollArea, so it's always on screen like a status bar. At the end of
    /// the scrolling page it was only reachable by scrolling past every
    /// chart and the whole process table.
    pub fn export_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Export to:");
            ui.add(egui::TextEdit::singleline(&mut self.export_path).desired_width(480.0));
            let was_json = self.export_as_json;
            ui.radio_value(&mut self.export_as_json, false, "CSV");
            ui.radio_value(&mut self.export_as_json, true, "JSON");
            if self.export_as_json != was_json {
                self.export_path = export::path_for_format(&self.export_path, self.export_as_json);
                self.export_status = None;
            }
            if ui.button("Export").clicked() {
                self.export_status = Some(self.do_export());
            }
            // In this same row, right of the button, so the bar keeps one
            // fixed height. The message used to go in the page's top status
            // line, which was off-screen when you clicked and pushed the
            // whole page down ~30 px under the cursor. Kept short (file name
            // only, truncated if needed) so it fits; the full path or error
            // is on hover.
            match &self.export_status {
                Some(Ok(path)) => {
                    let name = std::path::Path::new(path)
                        .file_name()
                        .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("✔ Saved {name}"))
                                .color(egui::Color32::LIGHT_GREEN),
                        )
                        .truncate(),
                    )
                    .on_hover_text(path);
                }
                Some(Err(e)) => {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("Export failed: {e}"))
                                .color(egui::Color32::RED),
                        )
                        .truncate(),
                    )
                    .on_hover_text(e);
                }
                None => {}
            }
        });
    }

    fn do_export(&self) -> Result<String, String> {
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
            Ok(()) => {
                log::info!("exported session {} to {}", self.session.id, path.display());
                Ok(path.display().to_string())
            }
            Err(e) => {
                log::error!(
                    "export of session {} to {} failed: {e:#}",
                    self.session.id,
                    path.display()
                );
                Err(format!("{e:#}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(base_step_size: f64) -> [f64; 3] {
        nice_grid_steps(egui_plot::GridInput {
            bounds: (0.0, 100.0),
            base_step_size,
        })
    }

    fn approx(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= y * 1e-9)
    }

    #[test]
    fn steps_walk_1_2_10_20_from_the_base() {
        // The first-Windows-run case: CPU zoomed to ~15–88% in a 140 pt
        // chart gives a base of ~4.3 → 10/20/100, so 20s get labels.
        assert!(approx(steps(4.3), [10.0, 20.0, 100.0]));
        assert!(approx(steps(10.0), [10.0, 20.0, 100.0]));
        assert!(approx(steps(11.0), [20.0, 100.0, 200.0]));
        assert!(approx(steps(1.3e6), [2e6, 1e7, 2e7]));
        assert!(approx(steps(0.03), [0.1, 0.2, 1.0]));
    }

    #[test]
    fn consecutive_steps_nest_and_are_at_most_5x_apart() {
        for base in [0.003, 0.7, 1.0, 4.3, 18.9, 250.0, 7.7e5, 1.3e6] {
            let [a, b, c] = steps(base);
            assert!(a >= base, "{base}: first step {a} below base");
            for (lo, hi) in [(a, b), (b, c)] {
                let ratio = hi / lo;
                assert!(ratio > 1.0 && ratio <= 5.0 + 1e-9, "{base}: ratio {ratio}");
                assert!(
                    (ratio - ratio.round()).abs() < 1e-9,
                    "{base}: {hi} not a multiple of {lo}"
                );
            }
        }
    }

    #[test]
    fn memory_hover_shows_percent_and_absolute_megabytes() {
        let mem_mb = [(20_000.0, 65_329.0), (22_848.0, 65_329.0)];
        let pos = HoverPosition::NearDataPoint {
            plot_name: MEM_SERIES,
            position: egui_plot::PlotPoint::new(17.4, 34.97),
            index: 1,
        };
        assert_eq!(
            mem_hover_label(&pos, &mem_mb, &[]).as_deref(),
            Some("Mem used: 35% (22848 MB of 65329 MB)\n17.4 s")
        );
        // Event markers on the memory chart still hover as name + time.
        let kinds = vec!["mem_spike".to_string()];
        let pos = HoverPosition::NearDataPoint {
            plot_name: "mem_spike",
            position: egui_plot::PlotPoint::new(3.0, 50.0),
            index: 0,
        };
        assert_eq!(
            mem_hover_label(&pos, &mem_mb, &kinds).as_deref(),
            Some("mem_spike\n3.0 s")
        );
    }

    #[test]
    fn cpu_performance_hover_shows_the_clock() {
        let freqs = [3600.0, 3850.1];
        let pos = HoverPosition::NearDataPoint {
            plot_name: PERF_SERIES,
            position: egui_plot::PlotPoint::new(12.0, 106.9),
            index: 1,
        };
        assert_eq!(
            perf_hover_label(&pos, &freqs, &[]).as_deref(),
            Some("Performance: 107% (3.85 GHz)\n12.0 s")
        );
        // The limit line is a plain percentage.
        let pos = HoverPosition::NearDataPoint {
            plot_name: "Limit",
            position: egui_plot::PlotPoint::new(12.0, 100.0),
            index: 1,
        };
        assert_eq!(
            perf_hover_label(&pos, &freqs, &[]).as_deref(),
            Some("Limit: 100%\n12.0 s")
        );
    }

    #[test]
    fn event_markers_hover_with_name_and_time_only() {
        let kinds = vec!["cpu_spike".to_string()];
        let pos = HoverPosition::NearDataPoint {
            plot_name: "cpu_spike",
            position: egui_plot::PlotPoint::new(17.4, 55.0),
            index: 0,
        };
        assert_eq!(
            hover_label(&pos, format_pct, &kinds).as_deref(),
            Some("cpu_spike\n17.4 s")
        );
        let pos = HoverPosition::NearDataPoint {
            plot_name: "Disk read",
            position: egui_plot::PlotPoint::new(16.0, 21_700_000.0),
            index: 3,
        };
        assert_eq!(
            hover_label(&pos, format_rate, &kinds).as_deref(),
            Some("Disk read: 22 MB/s\n16.0 s")
        );
    }
}
