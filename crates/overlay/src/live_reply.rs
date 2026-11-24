//! Parses the agent's `live` control-channel reply (see
//! `crates/agent/src/ipc.rs`): `OK cpu_pct=12.3 mem_used_pct=45.6
//! gpu_pct=- cpu_temp_c=- gpu_temp_c=61.5 board_temp_c=27.9 age_ms=123`,
//! or `OK no_data_yet` before the sampler has ticked since this lease
//! started. The temperature fields came later (Phase 8): an older agent
//! leaves them out, and they read as unavailable.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveReading {
    pub cpu_pct: f32,
    pub mem_used_pct: Option<f32>,
    pub gpu_pct: Option<f32>,
    /// Hottest sensor of each kind, °C.
    pub cpu_temp_c: Option<f32>,
    pub gpu_temp_c: Option<f32>,
    pub board_temp_c: Option<f32>,
    pub age_ms: i64,
}

impl LiveReading {
    /// The overlay's four text rows. Always four, with `—` for anything
    /// unavailable, so the window keeps one size whatever the machine has.
    pub fn rows(&self) -> [String; 4] {
        let pct = |v: Option<f32>| v.map_or_else(|| "    —".to_string(), |v| format!("{v:>5.1}%"));
        let temp = |v: Option<f32>| v.map_or_else(String::new, |t| format!("  {t:.0} °C"));
        [
            format!("CPU  {}{}", pct(Some(self.cpu_pct)), temp(self.cpu_temp_c)),
            format!("RAM  {}", pct(self.mem_used_pct)),
            format!("GPU  {}{}", pct(self.gpu_pct), temp(self.gpu_temp_c)),
            // An ACPI zone: a motherboard or chassis sensor, often a fixed
            // placeholder value (see `storage::SensorKind::Board`).
            match self.board_temp_c {
                Some(t) => format!("Board  {t:.0} °C"),
                None => "Board  —".to_string(),
            },
        ]
    }
}

/// `None` for `no_data_yet`, an `ERR ...` reply, or anything else
/// unparsable — the caller treats all of those as "nothing to show yet",
/// not a crash.
pub fn parse(reply: &str) -> Option<LiveReading> {
    let rest = reply.strip_prefix("OK ")?;
    if rest == "no_data_yet" {
        return None;
    }

    let mut cpu_pct = None;
    let mut mem_used_pct = None;
    let mut gpu_pct = None;
    let mut cpu_temp_c = None;
    let mut gpu_temp_c = None;
    let mut board_temp_c = None;
    let mut age_ms = None;
    for field in rest.split(' ') {
        let (key, value) = field.split_once('=')?;
        match key {
            "cpu_pct" => cpu_pct = value.parse().ok(),
            "mem_used_pct" => mem_used_pct = value.parse().ok(),
            "gpu_pct" => gpu_pct = value.parse().ok(),
            "cpu_temp_c" => cpu_temp_c = value.parse().ok(),
            "gpu_temp_c" => gpu_temp_c = value.parse().ok(),
            "board_temp_c" => board_temp_c = value.parse().ok(),
            "age_ms" => age_ms = value.parse().ok(),
            _ => {}
        }
    }

    Some(LiveReading {
        cpu_pct: cpu_pct?,
        mem_used_pct,
        gpu_pct,
        cpu_temp_c,
        gpu_temp_c,
        board_temp_c,
        age_ms: age_ms?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_reading() {
        let r = parse("OK cpu_pct=12.3 mem_used_pct=45.6 gpu_pct=7.0 age_ms=123").unwrap();
        assert_eq!(r.cpu_pct, 12.3);
        assert_eq!(r.mem_used_pct, Some(45.6));
        assert_eq!(r.gpu_pct, Some(7.0));
        assert_eq!(r.age_ms, 123);
    }

    #[test]
    fn dash_fields_are_none_not_zero() {
        let r = parse("OK cpu_pct=1.0 mem_used_pct=- gpu_pct=- age_ms=0").unwrap();
        assert_eq!(r.mem_used_pct, None);
        assert_eq!(r.gpu_pct, None);
    }

    #[test]
    fn no_data_yet_is_none() {
        assert_eq!(parse("OK no_data_yet"), None);
    }

    #[test]
    fn an_error_reply_is_none() {
        assert_eq!(parse("ERR unknown command 'live'"), None);
    }

    #[test]
    fn garbage_is_none_not_a_panic() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("OK"), None);
        assert_eq!(parse("OK cpu_pct=notanumber age_ms=1"), None);
    }

    #[test]
    fn parses_temperatures_and_shows_them_in_the_rows() {
        let r = parse(
            "OK cpu_pct=12.3 mem_used_pct=45.6 gpu_pct=7.0 cpu_temp_c=- gpu_temp_c=61.5 \
             board_temp_c=27.9 age_ms=1",
        )
        .unwrap();
        assert_eq!(
            (r.cpu_temp_c, r.gpu_temp_c, r.board_temp_c),
            (None, Some(61.5), Some(27.9))
        );
        assert_eq!(
            r.rows(),
            [
                "CPU   12.3%",
                "RAM   45.6%",
                "GPU    7.0%  62 °C",
                "Board  28 °C"
            ]
        );
    }

    #[test]
    fn an_agent_without_temperatures_still_parses_with_dashes() {
        // What an agent from before Phase 8 sends.
        let r = parse("OK cpu_pct=1.0 mem_used_pct=- gpu_pct=- age_ms=0").unwrap();
        assert_eq!(r.gpu_temp_c, None);
        assert_eq!(
            r.rows(),
            ["CPU    1.0%", "RAM      —", "GPU      —", "Board  —"]
        );
    }

    #[test]
    fn field_order_does_not_matter() {
        let r = parse("OK age_ms=5 gpu_pct=1.0 cpu_pct=2.0 mem_used_pct=3.0").unwrap();
        assert_eq!(r.cpu_pct, 2.0);
        assert_eq!(r.age_ms, 5);
    }
}
