//! Pure offset math: no memory reading, no files, no osu! needed.
//!
//! Hit errors are in milliseconds of *song time*, exactly as osu! stable
//! stores them: negative = early, positive = late.

/// osu! stable mod bits (the ones that affect this calculation).
pub mod mods {
    pub const EASY: u32 = 1 << 1;
    pub const HARD_ROCK: u32 = 1 << 4;
    pub const DOUBLE_TIME: u32 = 1 << 6;
    pub const HALF_TIME: u32 = 1 << 8;
    /// Nightcore is always sent together with the DoubleTime bit.
    pub const NIGHTCORE: u32 = 1 << 9;
}

/// Plays with fewer hits than this (after removing 50s) are discarded.
pub const MIN_HITS: usize = 50;

/// UR at or above this starts damping the median (lazer's constant).
const DAMPING_UR_CUTOFF: f64 = 90.0;
/// Exponential damping factor (lazer's constant).
const DAMPING_FACTOR: f64 = -0.0116;

/// What one play contributes to the history.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayStats {
    /// Median hit error, song time (left unconverted by design).
    pub median: f64,
    /// Unstable rate, converted to real time.
    pub unstable_rate: f64,
    /// Hits left after removing 50s.
    pub hits: usize,
}

/// OD after HR (x1.4, capped at 10) or EZ (x0.5).
pub fn effective_od(od: f64, mod_bits: u32) -> f64 {
    if mod_bits & mods::HARD_ROCK != 0 {
        (od * 1.4).min(10.0)
    } else if mod_bits & mods::EASY != 0 {
        od * 0.5
    } else {
        od
    }
}

/// Playback rate: DT/NC = 1.5, HT = 0.75, otherwise 1.0.
pub fn rate(mod_bits: u32) -> f64 {
    if mod_bits & (mods::DOUBLE_TIME | mods::NIGHTCORE) != 0 {
        1.5
    } else if mod_bits & mods::HALF_TIME != 0 {
        0.75
    } else {
        1.0
    }
}

/// Edge of the 100 window in ms, truncated like the game does.
pub fn cutoff_100(od: f64) -> i32 {
    (140.0 - 8.0 * od).floor() as i32
}

/// Edge of the 300 window in ms (for display).
pub fn window_300(od: f64) -> i32 {
    (80.0 - 6.0 * od).floor() as i32
}

/// Edge of the 50 window in ms; beyond it is a miss (for display).
pub fn window_50(od: f64) -> i32 {
    (200.0 - 10.0 * od).floor() as i32
}

/// Keep only hits strictly inside the 100 window. A hit exactly on the
/// cutoff is a 50 in-game, so it is dropped too.
pub fn filter_50s(errors: &[i32], cutoff: i32) -> Vec<i32> {
    errors.iter().copied().filter(|e| e.abs() < cutoff).collect()
}

/// Median; an even count averages the two middle values.
pub fn median(errors: &[i32]) -> Option<f64> {
    if errors.is_empty() {
        return None;
    }
    let mut sorted = errors.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    Some(if sorted.len().is_multiple_of(2) {
        (f64::from(sorted[mid - 1]) + f64::from(sorted[mid])) / 2.0
    } else {
        f64::from(sorted[mid])
    })
}

/// 10 x population standard deviation, divided by the rate so it is in
/// real time (same as stable's UR display, tosu and lazer).
pub fn unstable_rate(errors: &[i32], rate: f64) -> Option<f64> {
    if errors.is_empty() {
        return None;
    }
    let n = errors.len() as f64;
    let mean = errors.iter().map(|&e| f64::from(e)).sum::<f64>() / n;
    let variance = errors
        .iter()
        .map(|&e| (f64::from(e) - mean).powi(2))
        .sum::<f64>()
        / n;
    Some(10.0 * variance.sqrt() / rate)
}

/// Full per-play pipeline: mods -> cutoff -> drop 50s -> min hits -> M, UR.
/// Returns `None` when the play must be discarded.
pub fn analyze_play(errors: &[i32], od: f64, mod_bits: u32) -> Option<PlayStats> {
    analyze_with_effective_od(errors, effective_od(od, mod_bits), rate(mod_bits))
}

/// Same as [`analyze_play`], for an OD that already has HR/EZ applied.
pub fn analyze_with_effective_od(errors: &[i32], effective_od: f64, rate: f64) -> Option<PlayStats> {
    let kept = filter_50s(errors, cutoff_100(effective_od));
    if kept.len() < MIN_HITS {
        return None;
    }
    Some(PlayStats {
        median: median(&kept)?,
        unstable_rate: unstable_rate(&kept, rate)?,
        hits: kept.len(),
    })
}

/// Median scaled down for inconsistent plays (UR >= 90).
pub fn dampened_median(median: f64, unstable_rate: f64) -> f64 {
    if unstable_rate >= DAMPING_UR_CUTOFF {
        median * (DAMPING_FACTOR * (unstable_rate - DAMPING_UR_CUTOFF)).exp()
    } else {
        median
    }
}

/// S for one play: late (positive M) -> push the global offset negative.
pub fn play_suggestion(global_offset: i32, median: f64, unstable_rate: f64) -> f64 {
    f64::from(global_offset) - dampened_median(median, unstable_rate)
}

/// How several plays' suggestions are combined into one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Aggregation {
    #[default]
    Mean,
    Median,
}

/// Local (per-beatmap) suggestion for one play, in stable's convention:
/// late (positive M) -> positive local offset. Independent of the global offset.
pub fn local_play_suggestion(local_offset: i32, median: f64, unstable_rate: f64) -> f64 {
    f64::from(local_offset) + dampened_median(median, unstable_rate)
}

/// Mean or median of `values`; `None` when empty.
pub fn combine(values: &[f64], how: Aggregation) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(match how {
        Aggregation::Mean => values.iter().sum::<f64>() / values.len() as f64,
        Aggregation::Median => {
            let mut sorted = values.to_vec();
            sorted.sort_by(f64::total_cmp);
            let mid = sorted.len() / 2;
            if sorted.len().is_multiple_of(2) {
                (sorted[mid - 1] + sorted[mid]) / 2.0
            } else {
                sorted[mid]
            }
        }
    })
}

/// The global suggestion over the history.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlobalSuggestion {
    /// Unrounded mean of S, for display.
    pub mean: f64,
    /// Rounded to a whole ms, the value to set in osu! stable.
    pub apply: i32,
}

/// Mean of all S values; `None` with an empty history.
pub fn global_suggestion(suggestions: &[f64]) -> Option<GlobalSuggestion> {
    if suggestions.is_empty() {
        return None;
    }
    let mean = suggestions.iter().sum::<f64>() / suggestions.len() as f64;
    Some(GlobalSuggestion {
        mean,
        apply: mean.round() as i32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_rock_multiplies_od_and_caps_at_ten() {
        assert!((effective_od(5.0, mods::HARD_ROCK) - 7.0).abs() < 1e-9);
        assert_eq!(effective_od(9.0, mods::HARD_ROCK), 10.0);
        assert_eq!(effective_od(8.0, mods::EASY), 4.0);
        assert_eq!(effective_od(8.0, 0), 8.0);
    }

    #[test]
    fn rate_mods() {
        assert_eq!(rate(mods::DOUBLE_TIME), 1.5);
        assert_eq!(rate(mods::DOUBLE_TIME | mods::NIGHTCORE), 1.5);
        assert_eq!(rate(mods::HALF_TIME), 0.75);
        assert_eq!(rate(mods::HARD_ROCK), 1.0);
    }

    #[test]
    fn cutoff_is_truncated() {
        assert_eq!(cutoff_100(10.0), 60);
        assert_eq!(cutoff_100(8.5), 72);
        // 140 - 8 * 8.7 = 70.4 -> 70
        assert_eq!(cutoff_100(8.7), 70);
        assert_eq!(window_300(10.0), 20);
        assert_eq!(window_50(10.0), 100);
    }

    #[test]
    fn hit_exactly_on_cutoff_is_a_50() {
        assert_eq!(filter_50s(&[-60, -59, 0, 59, 60, 61], 60), vec![-59, 0, 59]);
    }

    #[test]
    fn median_odd_and_even() {
        assert_eq!(median(&[5, 1, 3]), Some(3.0));
        assert_eq!(median(&[7, 1, 5, 3]), Some(4.0));
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn unstable_rate_divides_by_rate() {
        // std dev of [-10, 10] is 10 -> UR 100
        assert_eq!(unstable_rate(&[-10, 10], 1.0), Some(100.0));
        assert!((unstable_rate(&[-15, 15], 1.5).unwrap() - 100.0).abs() < 1e-9);
        assert_eq!(unstable_rate(&[], 1.0), None);
    }

    #[test]
    fn needs_fifty_hits_left_after_removing_50s() {
        let mut errors = vec![5; 49];
        errors.push(100); // a 50 at OD 8 (cutoff 76)
        assert_eq!(analyze_play(&errors, 8.0, 0), None);

        errors.push(5);
        let stats = analyze_play(&errors, 8.0, 0).unwrap();
        assert_eq!(stats.hits, 50);
        assert_eq!(stats.median, 5.0);
    }

    #[test]
    fn median_stays_in_song_time_on_dt() {
        let errors: Vec<i32> = (0..50).map(|i| if i % 2 == 0 { 0 } else { 30 }).collect();
        let stats = analyze_play(&errors, 5.0, mods::DOUBLE_TIME).unwrap();
        assert_eq!(stats.median, 15.0);
        assert!((stats.unstable_rate - 100.0).abs() < 1e-9); // 150 / 1.5
    }

    #[test]
    fn damping_only_at_or_above_ur_90() {
        assert_eq!(dampened_median(10.0, 89.9), 10.0);
        assert_eq!(dampened_median(10.0, 90.0), 10.0);
        let damped = dampened_median(10.0, 100.0);
        assert!((damped - 10.0 * (-0.116_f64).exp()).abs() < 1e-12);
    }

    #[test]
    fn late_tapping_suggests_a_more_negative_global_offset() {
        assert_eq!(play_suggestion(0, 7.0, 80.0), -7.0);
        assert_eq!(play_suggestion(-5, -2.0, 80.0), -3.0);
    }

    #[test]
    fn late_tapping_suggests_a_positive_local_offset() {
        assert_eq!(local_play_suggestion(0, 5.0, 80.0), 5.0);
        assert_eq!(local_play_suggestion(10, -3.0, 80.0), 7.0);
        let damped = local_play_suggestion(0, 10.0, 100.0);
        assert!((damped - 10.0 * (-0.116_f64).exp()).abs() < 1e-12);
    }

    #[test]
    fn combine_mean_and_median() {
        assert_eq!(combine(&[1.0, 2.0, 9.0], Aggregation::Mean), Some(4.0));
        assert_eq!(combine(&[1.0, 2.0, 9.0], Aggregation::Median), Some(2.0));
        assert_eq!(combine(&[1.0, 2.0, 4.0, 9.0], Aggregation::Median), Some(3.0));
        assert_eq!(combine(&[], Aggregation::Mean), None);
    }

    #[test]
    fn global_suggestion_is_mean_and_rounded() {
        let s = global_suggestion(&[-7.0, -8.0, -6.5]).unwrap();
        assert!((s.mean - (-7.1666)).abs() < 1e-3);
        assert_eq!(s.apply, -7);
        assert_eq!(global_suggestion(&[]), None);
    }
}
