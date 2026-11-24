//! Temperature and throttling events (Phase 8), from each recorded sample.
//!
//! Unlike `cpu_spike`/`mem_spike`, which are recorded for every sample over
//! their threshold, these fire once when a condition *starts*. A hot GPU or
//! a power-limited CPU stays that way for a whole gaming session, and one
//! event per second for an hour would bury every other marker on the chart.

use std::collections::HashSet;

use agent_config::AgentConfig;
use storage::{MetricSample, NewEvent, SensorKind};

/// How far below its threshold a sensor has to cool before crossing the
/// threshold again counts as a new spike. Without it, a reading hovering
/// around the threshold (84.9, 85.1, 84.9, …) fires on every other sample.
const HYSTERESIS_C: f32 = 3.0;

/// Per-session state; replaced with a fresh one when a session starts, so a
/// sensor that's already hot then is reported at the session's start.
#[derive(Default)]
pub struct ThermalWatch {
    /// Sensors currently at or above their threshold.
    hot: HashSet<String>,
    cpu_limited: bool,
}

fn threshold_c(config: &AgentConfig, kind: SensorKind) -> Option<f32> {
    match kind {
        SensorKind::Gpu => Some(config.gpu_temp_threshold_c),
        SensorKind::Board => Some(config.board_temp_threshold_c),
        // No Windows source for CPU temperature yet, so no setting for it.
        SensorKind::Cpu | SensorKind::Other => None,
    }
}

impl ThermalWatch {
    pub fn check(&mut self, sample: &MetricSample, config: &AgentConfig, out: &mut Vec<NewEvent>) {
        for t in &sample.temps {
            let Some(threshold) = threshold_c(config, t.kind) else {
                continue;
            };
            if t.celsius >= threshold {
                if self.hot.insert(t.sensor.clone()) {
                    out.push(NewEvent {
                        ts: sample.ts,
                        kind: "temp_spike".to_string(),
                        value: t.celsius as f64,
                        detail: Some(format!(
                            "{} reached {:.1} °C (threshold {threshold:.0} °C)",
                            t.sensor, t.celsius
                        )),
                    });
                }
            } else if t.celsius < threshold - HYSTERESIS_C {
                self.hot.remove(&t.sensor);
            }
        }

        if let Some(perf) = sample.cpu_perf {
            let limited = perf.is_limited();
            if limited && !self.cpu_limited {
                out.push(NewEvent {
                    ts: sample.ts,
                    kind: "cpu_throttle".to_string(),
                    value: perf.limit_pct as f64,
                    detail: Some(format!(
                        "CPU limited to {:.0}% of its performance (limit flags {:#x}), \
                         {:.0} MHz at {:.0}% load",
                        perf.limit_pct, perf.limit_flags, perf.freq_mhz, sample.cpu_pct
                    )),
                });
            }
            self.cpu_limited = limited;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::{CpuPerf, TempReading};

    fn sample(ts: i64, gpu_c: f32, limit_pct: f32) -> MetricSample {
        MetricSample {
            ts,
            cpu_pct: 90.0,
            cpu_per_core_pct: Vec::new(),
            mem_used_mb: 1.0,
            mem_available_mb: 1.0,
            disk_read_bps: 0.0,
            disk_write_bps: 0.0,
            net_rx_bps: 0.0,
            net_tx_bps: 0.0,
            context_switches_per_sec: 0.0,
            page_faults_per_sec: 0.0,
            gpu_pct: None,
            disk_queue_len: None,
            temps: vec![
                TempReading {
                    sensor: "GPU".into(),
                    kind: SensorKind::Gpu,
                    celsius: gpu_c,
                },
                TempReading {
                    sensor: "CPU".into(),
                    kind: SensorKind::Cpu,
                    celsius: 99.0,
                },
            ],
            cpu_perf: Some(CpuPerf {
                freq_mhz: 3000.0,
                perf_pct: 80.0,
                limit_pct,
                limit_flags: 0,
            }),
            top_processes: Vec::new(),
        }
    }

    fn kinds_over(watch: &mut ThermalWatch, samples: &[MetricSample]) -> Vec<(i64, String)> {
        let config = AgentConfig::default(); // GPU threshold 85 °C
        let mut out = Vec::new();
        for s in samples {
            watch.check(s, &config, &mut out);
        }
        out.into_iter().map(|e| (e.ts, e.kind)).collect()
    }

    #[test]
    fn a_temperature_spike_fires_once_per_crossing_with_hysteresis() {
        let mut watch = ThermalWatch::default();
        let events = kinds_over(
            &mut watch,
            &[
                sample(1, 80.0, 100.0),
                sample(2, 86.0, 100.0), // crosses: event
                sample(3, 88.0, 100.0), // still hot: nothing
                sample(4, 84.0, 100.0), // under 85 but not 3 °C under: stays hot
                sample(5, 85.5, 100.0), // so this isn't a new spike
                sample(6, 81.0, 100.0), // cooled off: re-armed
                sample(7, 85.0, 100.0), // at the threshold counts: event
            ],
        );
        assert_eq!(
            events,
            [(2, "temp_spike".to_string()), (7, "temp_spike".to_string())]
        );
    }

    #[test]
    fn cpu_temperatures_have_no_threshold_yet() {
        // The 99 °C CPU reading in every sample never fires.
        let mut watch = ThermalWatch::default();
        assert!(kinds_over(&mut watch, &[sample(1, 30.0, 100.0)]).is_empty());
    }

    #[test]
    fn throttling_fires_when_it_starts_not_while_it_lasts() {
        let mut watch = ThermalWatch::default();
        let events = kinds_over(
            &mut watch,
            &[
                sample(1, 30.0, 100.0),
                sample(2, 30.0, 70.0), // starts: event
                sample(3, 30.0, 60.0), // continues: nothing
                sample(4, 30.0, 100.0),
                sample(5, 30.0, 90.0), // starts again: event
            ],
        );
        assert_eq!(
            events,
            [
                (2, "cpu_throttle".to_string()),
                (5, "cpu_throttle".to_string())
            ]
        );
    }

    #[test]
    fn a_fresh_watch_reports_a_condition_already_present() {
        let mut watch = ThermalWatch::default();
        kinds_over(&mut watch, &[sample(1, 90.0, 50.0)]);
        // A new session starts with a new watch, and the GPU is still hot
        // and the CPU still limited: both show at the session's start.
        let mut watch = ThermalWatch::default();
        assert_eq!(kinds_over(&mut watch, &[sample(2, 90.0, 50.0)]).len(), 2);
    }
}
