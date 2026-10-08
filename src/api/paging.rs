//! The `limit` query parameter every list endpoint accepts.

/// `requested` (the caller's `?limit=`, if any) clamped to `1..=max`, or
/// `default` when absent. A zero or negative limit is raised to 1 and an
/// oversized one lowered to `max`, so a list endpoint can never be asked for
/// nothing or for the whole table.
pub(crate) fn clamp_limit(requested: Option<i64>, default: i64, max: i64) -> i64 {
    requested.unwrap_or(default).clamp(1, max)
}

#[cfg(test)]
mod tests {
    use super::clamp_limit;

    #[test]
    fn absent_means_the_default() {
        assert_eq!(clamp_limit(None, 50, 200), 50);
    }

    #[test]
    fn zero_and_negative_become_one() {
        assert_eq!(clamp_limit(Some(0), 50, 200), 1);
        assert_eq!(clamp_limit(Some(-7), 50, 200), 1);
    }

    #[test]
    fn oversized_becomes_the_maximum() {
        assert_eq!(clamp_limit(Some(10_000), 50, 200), 200);
    }

    #[test]
    fn an_in_range_value_is_kept() {
        assert_eq!(clamp_limit(Some(75), 50, 200), 75);
    }
}
