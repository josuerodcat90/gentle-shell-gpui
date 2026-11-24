//! One-line text for summaries, titles and chips.

/// The first line of `text`, cut to `max_chars` with `…` when it is longer
/// or when more lines follow.
pub fn first_line(text: &str, max_chars: usize) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() > max_chars {
        let cut: String = line.chars().take(max_chars).collect();
        format!("{cut}…")
    } else if text.lines().nth(1).is_some() {
        format!("{line} …")
    } else {
        line.to_string()
    }
}

/// Elapsed seconds as a clock: `0:07`, `1:42`, `1:02:03`.
pub fn elapsed_clock(seconds: u64) -> String {
    let (hours, minutes, secs) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_clock_shows_minutes_then_hours() {
        assert_eq!(elapsed_clock(0), "0:00");
        assert_eq!(elapsed_clock(7), "0:07");
        assert_eq!(elapsed_clock(102), "1:42");
        assert_eq!(elapsed_clock(3599), "59:59");
        assert_eq!(elapsed_clock(3600), "1:00:00");
        assert_eq!(elapsed_clock(3601), "1:00:01");
        assert_eq!(elapsed_clock(3723), "1:02:03");
    }

    #[test]
    fn first_line_marks_truncation() {
        assert_eq!(first_line("abcdef", 3), "abc…");
        assert_eq!(first_line("ab\ncd", 10), "ab …");
        assert_eq!(first_line("ab", 10), "ab");
    }
}
