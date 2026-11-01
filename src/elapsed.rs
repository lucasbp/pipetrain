use std::time::Duration;

pub fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

pub fn format(d: Duration) -> String {
    let ms = millis(d);
    let tenths = ms.saturating_add(50) / 100;
    if tenths < 600 {
        return format!("{}.{} s", tenths / 10, tenths % 10);
    }
    let seconds = ms.saturating_add(500) / 1000;
    format!("{}m {:02}s", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(ms: u64) -> String {
        format(Duration::from_millis(ms))
    }

    #[test]
    fn below_a_minute_shows_tenths_of_a_second() {
        assert_eq!(shown(0), "0.0 s");
        assert_eq!(shown(400), "0.4 s");
        assert_eq!(shown(14_600), "14.6 s");
        assert_eq!(shown(59_900), "59.9 s");
    }

    #[test]
    fn rounding_to_tenths_uses_the_nearest_tenth() {
        assert_eq!(shown(49), "0.0 s");
        assert_eq!(shown(50), "0.1 s");
        assert_eq!(shown(149), "0.1 s");
    }

    #[test]
    fn a_value_that_rounds_to_sixty_seconds_uses_minutes() {
        assert_eq!(shown(59_949), "59.9 s");
        assert_eq!(shown(59_950), "1m 00s");
        assert_eq!(shown(59_960), "1m 00s");
    }

    #[test]
    fn from_a_minute_on_it_shows_minutes_and_padded_seconds() {
        assert_eq!(format(Duration::from_secs(60)), "1m 00s");
        assert_eq!(format(Duration::from_secs(65)), "1m 05s");
        assert_eq!(format(Duration::from_secs(3600)), "60m 00s");
        assert_eq!(shown(65_499), "1m 05s");
        assert_eq!(shown(65_500), "1m 06s");
    }

    #[test]
    fn millis_saturates_instead_of_overflowing() {
        assert_eq!(millis(Duration::from_millis(1_234)), 1_234);
        assert_eq!(millis(Duration::MAX), u64::MAX);
        assert!(format(Duration::MAX).ends_with('s'));
    }
}
