//! Temperature series for the session detail view (Phase 8), kept out of
//! the egui code so they're testable without a GUI.

use storage::{MetricSample, SensorKind};

/// One chart line per sensor, in the order sensors first appear, as
/// `(sensor, kind, [x, °C] points)`. A sensor missing from some samples
/// (unreadable for a moment) just has fewer points there.
pub fn series(
    metrics: &[MetricSample],
    x_of: impl Fn(i64) -> f64,
) -> Vec<(String, SensorKind, Vec<[f64; 2]>)> {
    let mut out: Vec<(String, SensorKind, Vec<[f64; 2]>)> = Vec::new();
    for m in metrics {
        let x = x_of(m.ts);
        for t in &m.temps {
            let point = [x, t.celsius as f64];
            match out.iter_mut().find(|(sensor, _, _)| *sensor == t.sensor) {
                Some((_, _, points)) => points.push(point),
                None => out.push((t.sensor.clone(), t.kind, vec![point])),
            }
        }
    }
    out
}

/// Below this many readings a flat line is unremarkable: a real sensor can
/// hold steady for a short recording.
const MIN_READINGS_FOR_CONSTANT: usize = 60;

/// Sensors that read exactly the same value for the whole session, with
/// that value. Many desktop boards report a fixed placeholder through ACPI
/// instead of a real sensor: the dev machine's one zone reads 27.85 °C even
/// under full load. The agent can't tell (it only sees one reading at a
/// time); over a whole recording it shows, and the UI says so rather than
/// presenting the number as the motherboard's temperature.
pub fn constant_sensors(metrics: &[MetricSample]) -> Vec<(String, f32)> {
    let mut seen: Vec<(String, f32, usize, bool)> = Vec::new(); // sensor, first, count, constant
    for t in metrics.iter().flat_map(|m| &m.temps) {
        match seen.iter_mut().find(|(sensor, ..)| *sensor == t.sensor) {
            Some((_, first, count, constant)) => {
                *count += 1;
                *constant &= t.celsius == *first;
            }
            None => seen.push((t.sensor.clone(), t.celsius, 1, true)),
        }
    }
    seen.into_iter()
        .filter(|(_, _, count, constant)| *constant && *count >= MIN_READINGS_FOR_CONSTANT)
        .map(|(sensor, value, ..)| (sensor, value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::TempReading;

    fn sample(ts: i64, temps: &[(&str, SensorKind, f32)]) -> MetricSample {
        MetricSample {
            ts,
            cpu_pct: 0.0,
            cpu_per_core_pct: Vec::new(),
            mem_used_mb: 0.0,
            mem_available_mb: 0.0,
            disk_read_bps: 0.0,
            disk_write_bps: 0.0,
            net_rx_bps: 0.0,
            net_tx_bps: 0.0,
            context_switches_per_sec: 0.0,
            page_faults_per_sec: 0.0,
            gpu_pct: None,
            disk_queue_len: None,
            temps: temps
                .iter()
                .map(|&(sensor, kind, celsius)| TempReading {
                    sensor: sensor.into(),
                    kind,
                    celsius,
                })
                .collect(),
            cpu_perf: None,
            top_processes: Vec::new(),
        }
    }

    #[test]
    fn one_series_per_sensor_in_order_of_appearance() {
        let metrics = [
            sample(1000, &[("GPU", SensorKind::Gpu, 40.0)]),
            sample(
                2000,
                &[
                    ("ACPI", SensorKind::Board, 28.0),
                    ("GPU", SensorKind::Gpu, 41.0),
                ],
            ),
            sample(3000, &[]),
        ];
        let s = series(&metrics, |ts| ts as f64 / 1000.0);
        assert_eq!(
            s,
            vec![
                (
                    "GPU".to_string(),
                    SensorKind::Gpu,
                    vec![[1.0, 40.0], [2.0, 41.0]]
                ),
                ("ACPI".to_string(), SensorKind::Board, vec![[2.0, 28.0]]),
            ]
        );
        assert!(series(&[sample(1, &[])], |ts| ts as f64).is_empty());
    }

    #[test]
    fn a_sensor_stuck_at_one_value_for_a_whole_session_is_flagged() {
        let metrics: Vec<_> = (0..MIN_READINGS_FOR_CONSTANT as i64)
            .map(|i| {
                sample(
                    i,
                    &[
                        ("ACPI", SensorKind::Board, 27.85),
                        ("GPU", SensorKind::Gpu, 40.0 + (i % 3) as f32),
                    ],
                )
            })
            .collect();
        assert_eq!(
            constant_sensors(&metrics),
            vec![("ACPI".to_string(), 27.85)]
        );
        // A short recording proves nothing either way.
        assert!(constant_sensors(&metrics[..10]).is_empty());
    }
}
