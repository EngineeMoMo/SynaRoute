pub(crate) fn parse(value: &str) -> Result<jiff::Timestamp, jiff::Error> { value.parse() }

pub(crate) fn from_millis(value: i64) -> Option<jiff::Timestamp> { jiff::Timestamp::from_millisecond(value).ok() }

pub(crate) fn utc_date() -> jiff::civil::Date { jiff::Timestamp::now().to_zoned_datetime(jiff::tz::TimeZone::UTC).date() }

pub(crate) fn rfc3339(timestamp: jiff::Timestamp) -> String {
    let nanos = timestamp.subsec_nanosecond();
    let precision = if nanos == 0 { 0 } else if nanos % 1_000_000 == 0 { 3 } else if nanos % 1000 == 0 { 6 } else { 9 };
    jiff::fmt::temporal::DateTimePrinter::new().precision(Some(precision))
        .timestamp_with_offset_to_string(&timestamp, jiff::tz::Offset::UTC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_keep_existing_persisted_precision_and_offsets() {
        for value in ["2026-11-23T10:11:12+00:00", "2026-11-23T10:11:12.123+00:00", "2026-11-23T10:11:12.123456+00:00", "2026-11-23T10:11:12.123456789+00:00"] {
            assert_eq!(rfc3339(parse(value).unwrap()), value);
        }
        assert_eq!(rfc3339(parse("2026-11-23T18:11:12.123+08:00").unwrap()), "2026-11-23T10:11:12.123+00:00");
        assert!(parse("not-a-timestamp").is_err());
        assert!(from_millis(i64::MAX).is_none());
    }

    #[test]
    fn local_calendar_days_remain_correct_across_daylight_saving() {
        let zone = jiff::tz::TimeZone::get("America/New_York").unwrap();
        let before = parse("2026-03-08T06:59:00Z").unwrap().to_zoned_datetime(zone.clone());
        let after = parse("2026-03-08T07:01:00Z").unwrap().to_zoned_datetime(zone);
        assert_eq!(before.date(), after.date());
        assert_eq!((before.hour(), after.hour()), (1, 3));
    }
}
