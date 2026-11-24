//! Pure helpers for the temperature sources in `windows_impl`/`windows_gpu`
//! and `linux_impl`, split out so they're unit-tested on every platform.
// Each backend uses only some of these.
#![allow(dead_code)]

/// Readings outside this range are a broken or unsupported sensor, not a
/// temperature: an unset ACPI zone can read 0 K (−273 °C). (D3DKMT's raw 0
/// for "not supported" is 0 °C, inside the range, so `windows_gpu` drops
/// that one itself.)
const PLAUSIBLE_C: std::ops::RangeInclusive<f32> = -40.0..=150.0;

pub fn plausible(celsius: f32) -> Option<f32> {
    PLAUSIBLE_C.contains(&celsius).then_some(celsius)
}

/// ACPI `High Precision Temperature` is in tenths of a kelvin (3010 is
/// 27.85 °C); the plain `Temperature` counter is whole kelvins.
pub fn kelvin_to_celsius(kelvin: f64) -> f32 {
    (kelvin - 273.15) as f32
}

/// `D3DKMT_ADAPTER_PERFDATA::Temperature` is in tenths of a degree Celsius.
pub fn deci_celsius(raw: u32) -> f32 {
    raw as f32 / 10.0
}

/// The chart legend for an ACPI thermal zone: its instance name as Windows
/// reports it (`\_TZ.TZ00`), marked as ACPI so nobody reads it as a CPU or
/// a named motherboard sensor.
pub fn acpi_sensor_name(instance: &str) -> String {
    format!("ACPI {instance}")
}

/// Sensor names must be unique (each is one chart line), but two identical
/// GPUs report the same adapter name. The first keeps its name; repeats get
/// ` #2`, ` #3`, … in the order given.
pub fn unique_names(names: Vec<String>) -> Vec<String> {
    let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    names
        .into_iter()
        .map(|name| {
            let n = seen.entry(name.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                name
            } else {
                format!("{name} #{n}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_convert_to_celsius() {
        // This dev machine's ACPI zone: 3010 tenths of a kelvin.
        assert!((kelvin_to_celsius(3010.0 / 10.0) - 27.85).abs() < 1e-4);
        // D3DKMT on the RTX 3060 Ti: 321 → 32.1 °C.
        assert_eq!(deci_celsius(321), 32.1);
    }

    #[test]
    fn implausible_readings_are_dropped() {
        assert_eq!(plausible(deci_celsius(0)), Some(0.0), "0 °C itself is fine");
        assert_eq!(plausible(kelvin_to_celsius(0.0)), None);
        assert_eq!(plausible(200.0), None);
        assert_eq!(plausible(85.5), Some(85.5));
    }

    #[test]
    fn repeated_names_get_numbered() {
        let names = unique_names(vec![
            "RTX 3060 Ti".into(),
            "Intel UHD".into(),
            "RTX 3060 Ti".into(),
            "RTX 3060 Ti".into(),
        ]);
        assert_eq!(
            names,
            [
                "RTX 3060 Ti",
                "Intel UHD",
                "RTX 3060 Ti #2",
                "RTX 3060 Ti #3"
            ]
        );
    }

    #[test]
    fn acpi_zones_are_labelled_as_such() {
        assert_eq!(acpi_sensor_name(r"\_TZ.TZ00"), r"ACPI \_TZ.TZ00");
    }
}
