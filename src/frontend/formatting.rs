use chrono::TimeDelta;

pub fn format_timedelta_timecode(timedelta: &TimeDelta) -> String {
    return format!(
        "{}:{:0>2}",
        timedelta.num_minutes(),
        timedelta.num_seconds() % 60,
    );
}

pub fn format_timer_timecode(progress: TimeDelta, full_length_in_secs: i64) -> String {
    match TimeDelta::seconds(full_length_in_secs).checked_sub(&progress) {
        Some(timedelta) => format_timedelta_timecode(&timedelta),
        None => {
            return format!("Now");
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::format_timedelta_timecode;

    #[test]
    fn format_timedelta_below_60s() {
        assert_eq!(
            format_timedelta_timecode(&TimeDelta::seconds(29)),
            String::from("0:29")
        );
    }

    #[test]
    fn format_timedelta_above_60s() {
        assert_eq!(
            format_timedelta_timecode(&TimeDelta::seconds(474)),
            String::from("7:54")
        );
    }
}
