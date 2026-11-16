// Only consumed by `windows_impl`, which isn't compiled on this platform —
// but its logic (date math, XML field extraction) is pure and is unit
// tested here on every platform, which is as close to "tested" as the
// Windows Event Log integration gets without a Windows machine.
#![allow(dead_code)]

/// RFC3339 UTC timestamp, the format the Windows Event Log XPath
/// `TimeCreated[@SystemTime...]` filter expects.
pub(super) fn iso8601_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86400);
    let secs_of_day = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z")
}

/// Inverse of `iso8601_utc`, tolerant of the variable-precision fractional
/// seconds (up to 7 digits) that `EvtRender` actually produces.
pub(super) fn parse_iso8601_to_ms(s: &str) -> Option<i64> {
    let s = s.trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;

    let mut date_parts = date.split('-');
    let y: i64 = date_parts.next()?.parse().ok()?;
    let m: u32 = date_parts.next()?.parse().ok()?;
    let d: u32 = date_parts.next()?.parse().ok()?;

    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut time_parts = hms.split(':');
    let hh: i64 = time_parts.next()?.parse().ok()?;
    let mm: i64 = time_parts.next()?.parse().ok()?;
    let ss: i64 = time_parts.next()?.parse().ok()?;

    let frac_digits = &frac[..frac.len().min(3)];
    let frac_ms: i64 = format!("{frac_digits:0<3}").parse().ok()?;

    let days = days_from_civil(y, m, d);
    Some(days * 86_400_000 + hh * 3_600_000 + mm * 60_000 + ss * 1000 + frac_ms)
}

/// Howard Hinnant's `civil_from_days` (public domain), days since the Unix
/// epoch -> proleptic Gregorian (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Inverse of `civil_from_days`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let mp = (if m > 2 { m - 3 } else { m + 9 }) as i64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Extracts the text content of the first `<tag>...</tag>` element.
/// Deliberately not a real XML parser — Event Log XML is small and
/// well-formed, so substring search is enough and avoids a new dependency.
pub(super) fn extract_text(xml: &str, tag: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag}"))?;
    let tag_end = xml[start..].find('>')? + start;
    if xml.as_bytes().get(tag_end.wrapping_sub(1)) == Some(&b'/') {
        return None; // self-closing, no text content
    }
    let close = format!("</{tag}>");
    let end = xml[tag_end..].find(&close)? + tag_end;
    Some(xml[tag_end + 1..end].trim().to_string())
}

/// Extracts an attribute value from the first `<tag ...>` element. Accepts
/// either quote style: `EvtRender` emits single-quoted attributes
/// (`<Provider Name='nhi'/>`), while hand-written fixtures often use double.
pub(super) fn extract_attr(xml: &str, tag: &str, attr: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag} "))?;
    let tag_end = xml[start..].find('>')? + start;
    let tag_str = &xml[start..tag_end];
    // Leading space so `Name=` doesn't match inside `EventSourceName=`.
    let attr_prefix = format!(" {attr}=");
    let eq_end = tag_str.find(&attr_prefix)? + attr_prefix.len();
    let quote = tag_str[eq_end..]
        .chars()
        .next()
        .filter(|c| *c == '"' || *c == '\'')?;
    let attr_start = eq_end + 1;
    let attr_end = tag_str[attr_start..].find(quote)? + attr_start;
    Some(tag_str[attr_start..attr_end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_formats_correctly() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn negative_ms_before_epoch_formats_correctly() {
        assert_eq!(iso8601_utc(-1000), "1969-12-31T23:59:59.000Z");
    }

    #[test]
    fn known_date_formats_correctly() {
        assert_eq!(iso8601_utc(1_794_832_496_789), "2026-11-16T12:34:56.789Z");
    }

    #[test]
    fn leap_day_roundtrips() {
        let ms = 951_782_400_000; // 2000-02-29T00:00:00Z
        assert_eq!(iso8601_utc(ms), "2000-02-29T00:00:00.000Z");
        assert_eq!(parse_iso8601_to_ms(&iso8601_utc(ms)), Some(ms));
    }

    #[test]
    fn parses_seven_digit_fractional_seconds_like_real_evtrender_output() {
        // EvtRender commonly emits 100ns-resolution fractional seconds.
        assert_eq!(
            parse_iso8601_to_ms("2026-11-16T12:34:56.7891234Z"),
            Some(1_794_832_496_789),
        );
    }

    #[test]
    fn roundtrips_many_timestamps() {
        for ms in [0, 1, -1, 86_399_999, 1_794_832_496_789, -86_400_000] {
            assert_eq!(parse_iso8601_to_ms(&iso8601_utc(ms)), Some(ms), "ms={ms}");
        }
    }

    #[test]
    fn extract_text_reads_element_body() {
        let xml = "<Event><System><EventID>7034</EventID></System></Event>";
        assert_eq!(extract_text(xml, "EventID"), Some("7034".to_string()));
    }

    #[test]
    fn extract_text_returns_none_for_missing_tag() {
        let xml = "<Event><System></System></Event>";
        assert_eq!(extract_text(xml, "EventID"), None);
    }

    #[test]
    fn extract_attr_reads_attribute_value() {
        let xml = r#"<Event><System><Provider Name="Service Control Manager" Guid="{abc}" /></System></Event>"#;
        assert_eq!(
            extract_attr(xml, "Provider", "Name"),
            Some("Service Control Manager".to_string())
        );
    }

    #[test]
    fn parses_realistic_event_xml_fragment() {
        let xml = r#"<Event xmlns="http://schemas.microsoft.com/win/2004/08/events/event">
  <System>
    <Provider Name="Service Control Manager" Guid="{555908d1-a6d7-4695-8e1e-26931d2012f4}" />
    <EventID>7034</EventID>
    <Level>2</Level>
    <TimeCreated SystemTime="2026-11-16T12:34:56.7891234Z" />
  </System>
</Event>"#;
        assert_eq!(extract_text(xml, "Level"), Some("2".to_string()));
        assert_eq!(extract_text(xml, "EventID"), Some("7034".to_string()));
        assert_eq!(
            extract_attr(xml, "Provider", "Name"),
            Some("Service Control Manager".to_string())
        );
        assert_eq!(
            extract_attr(xml, "TimeCreated", "SystemTime"),
            Some("2026-11-16T12:34:56.7891234Z".to_string())
        );
    }

    /// Verbatim `EvtRender(EvtRenderEventXml)` output captured on Windows 11:
    /// single-quoted attributes, no whitespace, attributes on `EventID`.
    #[test]
    fn parses_actual_evtrender_output_with_single_quotes() {
        let xml = "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System>\
<Provider Name='Netwtw10'/><EventID Qualifiers='32768'>6062</EventID><Version>0</Version>\
<Level>3</Level><Task>0</Task><Opcode>0</Opcode><Keywords>0x80000000000000</Keywords>\
<TimeCreated SystemTime='2026-11-16T21:11:51.7353742Z'/><EventRecordID>33609</EventRecordID>\
<Correlation/><Execution ProcessID='4' ThreadID='26388'/><Channel>System</Channel>\
<Computer>host</Computer><Security/></System></Event>";
        assert_eq!(extract_text(xml, "Level"), Some("3".to_string()));
        assert_eq!(extract_text(xml, "EventID"), Some("6062".to_string()));
        assert_eq!(
            extract_attr(xml, "Provider", "Name"),
            Some("Netwtw10".to_string())
        );
        assert_eq!(
            extract_attr(xml, "TimeCreated", "SystemTime"),
            Some("2026-11-16T21:11:51.7353742Z".to_string())
        );
    }

    #[test]
    fn extract_attr_does_not_match_attribute_name_suffix() {
        let xml = "<Provider EventSourceName='wrong' Name='right'/>";
        assert_eq!(
            extract_attr(xml, "Provider", "Name"),
            Some("right".to_string())
        );
    }
}
