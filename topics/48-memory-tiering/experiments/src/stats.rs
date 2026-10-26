//! Small summaries. Every timed figure in this crate is a median of a few
//! repetitions of one long run, not a mean: one descheduled run on a busy
//! machine moves a mean and leaves a median of three alone.

/// Median; sorts in place. Even lengths average the middle two.
pub fn median(v: &mut [f64]) -> f64 {
    assert!(!v.is_empty());
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let m = v.len() / 2;
    if v.len() % 2 == 1 {
        v[m]
    } else {
        (v[m - 1] + v[m]) / 2.0
    }
}

/// The 1-, 5- and 15-minute load averages, read from the OS so every
/// measurement carries the machine's busyness next to it (other benchmarks
/// may be running). Returns None where neither source exists.
pub fn loadavg() -> Option<String> {
    if let Ok(s) = std::fs::read_to_string("/proc/loadavg") {
        return Some(s.split_whitespace().take(3).collect::<Vec<_>>().join(" "));
    }
    let out = std::process::Command::new("sysctl").args(["-n", "vm.loadavg"]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim().trim_start_matches('{').trim_end_matches('}').trim();
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medians() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
    }
}
