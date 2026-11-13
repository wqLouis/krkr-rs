//! The run's frame-time history and the distribution it reports at the end.
//!
//! Every frame the run steps has a measured wall-clock gap: the headless
//! loop measures the gap between the frames it paces, and the windowed loop
//! takes the platform's own measurement. The history keeps the recent gaps
//! in a fixed ring, so a run of any length can report the distribution it
//! measured without its memory growing with its length.
//!
//! The report is the run's last line. It names the number of frames, the
//! fastest and the slowest frame, the percentiles, and the **1% low** — the
//! rate the slowest 1% of the frames run at.

/// The number of frame gaps the report's history keeps.
///
/// The history is a ring, so it holds the most recent gaps: a run longer
/// than this reports the distribution of its last frames. 65 536 gaps are
/// half a megabyte, and about eighteen minutes of a sixty-frame run.
pub(crate) const REPORT_FRAMES: usize = 65_536;

/// How the run paced its frames.
///
/// A paced run waits for a frame deadline between the frames, so the gap it
/// measures includes that wait: the gap is the frame's cost plus the time
/// the loop spent outside the frame, and not the engine's cost alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pacing {
    /// Whether the run waited for a frame deadline between the frames.
    pub paced: bool,
    /// The frame period the run paced at, in milliseconds.
    pub period_ms: f64,
}

/// The measured distribution of a run's frame gaps.
///
/// Every number in it is measured: the gaps the run read from its clock.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameTimeReport {
    /// The number of frames the run stepped.
    pub frames: u64,
    /// The number of frame-to-frame gaps the run measured.
    pub gaps: u64,
    /// The number of gaps the percentiles cover.
    pub kept: usize,
    /// Whether the run waited for a frame deadline between the frames.
    ///
    /// A paced run's gaps include that wait, so they name the presentation
    /// cadence and not the engine's cost alone.
    pub paced: bool,
    /// The frame period the run paced at, in milliseconds.
    pub period_ms: Option<f64>,
    /// The run's start-up, in milliseconds: the gap from the run's start to
    /// its first frame.
    ///
    /// That gap spans the engine load and not a frame, so it is named apart
    /// from the frame-to-frame gaps.
    pub startup_ms: Option<f64>,
    /// The fastest frame-to-frame gap of the run, in milliseconds.
    pub fastest_ms: Option<f64>,
    /// The slowest frame-to-frame gap of the run, in milliseconds.
    pub slowest_ms: Option<f64>,
    /// The median frame time of the kept gaps, in milliseconds.
    pub p50_ms: Option<f64>,
    /// The 90th percentile frame time of the kept gaps, in milliseconds.
    pub p90_ms: Option<f64>,
    /// The 99th percentile frame time of the kept gaps, in milliseconds.
    pub p99_ms: Option<f64>,
    /// The mean frame time of the slowest 1% of the kept gaps, in
    /// milliseconds.
    pub one_percent_low_ms: Option<f64>,
    /// The rate the slowest 1% of the kept gaps run at, in frames per
    /// second.
    pub one_percent_low_fps: Option<f64>,
}

impl FrameTimeReport {
    /// The report's line: the run's last line.
    ///
    /// The line names the frames the run stepped, the gaps it measured, how
    /// many of them the percentiles cover, whether the run paced itself,
    /// the run's start-up, the fastest and the slowest frame, the
    /// percentiles and the 1% low.
    pub fn line(&self) -> String {
        let mut line = format!(
            "frame times: frames={} gaps={} kept={} paced={}",
            self.frames,
            self.gaps,
            self.kept,
            if self.paced { "yes" } else { "no" }
        );
        if self.paced
            && let Some(period) = self.period_ms
        {
            line.push_str(&format!("({period:.1}ms)"));
        }
        if let Some(startup) = self.startup_ms {
            line.push_str(&format!(" startup={startup:.1}ms"));
        }
        if let Some(ms) = self.fastest_ms {
            line.push_str(&format!(" fastest={}", frame_time(ms)));
        }
        if let Some(ms) = self.p50_ms {
            line.push_str(&format!(" p50={}", frame_time(ms)));
        }
        if let Some(ms) = self.p90_ms {
            line.push_str(&format!(" p90={}", frame_time(ms)));
        }
        if let Some(ms) = self.p99_ms {
            line.push_str(&format!(" p99={}", frame_time(ms)));
        }
        if let Some(ms) = self.slowest_ms {
            line.push_str(&format!(" slowest={}", frame_time(ms)));
        }
        if let Some(fps) = self.one_percent_low_fps {
            line.push_str(&format!(" 1%low={fps:.1}fps"));
            if let Some(ms) = self.one_percent_low_ms {
                line.push_str(&format!("({ms:.1}ms)"));
            }
        }
        line
    }
}

/// The bounded history of a run's frame gaps.
///
/// The gaps live in a fixed ring: the ring writes the oldest gap over when
/// it is full, so the history never grows and a run of any length reports
/// the distribution of its most recent frames.
pub(crate) struct FrameTimes {
    /// The retained frame-to-frame gaps, in milliseconds.
    gaps: Vec<f64>,
    /// The slot the next gap is written to.
    next: usize,
    /// The number of gaps the ring holds.
    len: usize,
    /// The number of frames the run stepped.
    frames: u64,
    /// The run's start-up: the gap from the run's start to its first frame.
    startup_ms: Option<f64>,
    /// The fastest frame-to-frame gap of the run, in milliseconds.
    fastest_ms: Option<f64>,
    /// The slowest frame-to-frame gap of the run, in milliseconds.
    slowest_ms: Option<f64>,
}

impl FrameTimes {
    /// Creates a history that keeps the most recent `capacity` gaps.
    pub(crate) fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            gaps: vec![0.0; capacity],
            next: 0,
            len: 0,
            frames: 0,
            startup_ms: None,
            fastest_ms: None,
            slowest_ms: None,
        }
    }

    /// Records one measured gap, in milliseconds.
    ///
    /// The first gap a run measures spans the run's start-up — the time
    /// from the run's start to its first frame — and not a frame, so it is
    /// kept apart from the frame-to-frame gaps.
    pub(crate) fn push(&mut self, gap_ms: f64) {
        if self.frames == 0 {
            self.startup_ms = Some(gap_ms);
        } else {
            self.fastest_ms = Some(
                self.fastest_ms
                    .map_or(gap_ms, |fastest| fastest.min(gap_ms)),
            );
            self.slowest_ms = Some(
                self.slowest_ms
                    .map_or(gap_ms, |slowest| slowest.max(gap_ms)),
            );
            self.gaps[self.next] = gap_ms;
            self.next = (self.next + 1) % self.gaps.len();
            self.len = (self.len + 1).min(self.gaps.len());
        }
        self.frames = self.frames.saturating_add(1);
    }

    /// The measured distribution of the run.
    pub(crate) fn report(&self, pacing: Pacing) -> FrameTimeReport {
        let mut sorted: Vec<f64> = self.gaps[..self.len].to_vec();
        sorted.sort_by(f64::total_cmp);
        let one_percent_low_ms = slowest_one_percent_ms(&sorted);
        FrameTimeReport {
            frames: self.frames,
            gaps: self.frames.saturating_sub(1),
            kept: self.len,
            paced: pacing.paced,
            period_ms: pacing.paced.then_some(pacing.period_ms),
            startup_ms: self.startup_ms,
            fastest_ms: self.fastest_ms,
            slowest_ms: self.slowest_ms,
            p50_ms: percentile(&sorted, 50),
            p90_ms: percentile(&sorted, 90),
            p99_ms: percentile(&sorted, 99),
            one_percent_low_ms,
            one_percent_low_fps: one_percent_low_ms
                .filter(|ms| *ms > 0.0)
                .map(|ms| 1000.0 / ms),
        }
    }
}

/// The rate the slowest 1% of `frame_times` run at.
///
/// The slowest 1% of the frames are averaged, and the rate is their
/// inverse. A history of fewer than a hundred frames still averages its
/// slowest frame, so the number is defined for every history that holds a
/// frame.
pub(crate) fn one_percent_low_fps(frame_times: impl IntoIterator<Item = f64>) -> Option<f64> {
    let mut sorted: Vec<f64> = frame_times.into_iter().collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let mean_ms = slowest_one_percent_ms(&sorted)?;
    (mean_ms > 0.0).then_some(1000.0 / mean_ms)
}

/// The mean frame time of the slowest 1% of the ascending `sorted` frames.
///
/// The slowest frames are the last ones, so the mean is taken over the tail
/// of the sorted values.
fn slowest_one_percent_ms(sorted: &[f64]) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let count = (sorted.len() / 100).max(1);
    let slowest = sorted.get(sorted.len() - count..)?;
    Some(slowest.iter().sum::<f64>() / count as f64)
}

/// The `percent`th percentile of the ascending `sorted` frames.
///
/// The percentile is the nearest rank: the smallest value at least
/// `percent` of the values are at or below it. With a hundred frames the
/// 99th percentile is the 99th slowest of them.
fn percentile(sorted: &[f64], percent: usize) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = percent.saturating_mul(sorted.len()).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

/// A frame time in milliseconds and the rate it runs at.
fn frame_time(ms: f64) -> String {
    format!("{ms:.1}ms/{:.1}fps", 1000.0 / ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A history of the given gaps, keeping all of them.
    fn history(gaps: &[f64]) -> FrameTimes {
        let mut times = FrameTimes::new(gaps.len().max(1));
        for gap in gaps {
            times.push(*gap);
        }
        times
    }

    /// The pacing of a run that waits for a frame deadline.
    fn pacing() -> Pacing {
        Pacing {
            paced: true,
            period_ms: 16.7,
        }
    }

    /// The ring keeps the most recent gaps: a history that is full writes
    /// the oldest gap over.
    #[test]
    fn the_history_keeps_the_most_recent_gaps() {
        let mut times = FrameTimes::new(4);
        for gap in [1.0, 2.0, 3.0, 4.0, 5.0] {
            times.push(gap);
        }
        let report = times.report(pacing());
        assert_eq!(report.frames, 5);
        assert_eq!(report.gaps, 4);
        assert_eq!(report.kept, 4);
        // The oldest gap is gone: the fastest frame is the second one.
        assert_eq!(report.fastest_ms, Some(2.0));
        assert_eq!(report.slowest_ms, Some(5.0));
    }

    /// The first gap spans the run's start-up, and it is not a frame: it is
    /// named apart from the frame-to-frame gaps.
    #[test]
    fn the_first_gap_is_the_startup() {
        let times = history(&[500.0, 16.0, 17.0]);
        let report = times.report(pacing());
        assert_eq!(report.startup_ms, Some(500.0));
        assert_eq!(report.fastest_ms, Some(16.0));
        assert_eq!(report.slowest_ms, Some(17.0));
        assert_eq!(report.gaps, 2);
    }

    /// The percentiles are the nearest ranks of the sorted gaps.
    #[test]
    fn the_percentiles_are_nearest_ranks() {
        let mut gaps = vec![0.0];
        gaps.extend((1..=100).map(|index| index as f64));
        let report = history(&gaps).report(pacing());
        assert_eq!(report.p50_ms, Some(50.0));
        assert_eq!(report.p90_ms, Some(90.0));
        assert_eq!(report.p99_ms, Some(99.0));
    }

    /// A run with no frame has no distribution.
    #[test]
    fn an_empty_history_reports_nothing() {
        let report = FrameTimes::new(8).report(pacing());
        assert_eq!(report.frames, 0);
        assert_eq!(report.gaps, 0);
        assert_eq!(report.kept, 0);
        assert_eq!(report.p50_ms, None);
        assert_eq!(report.one_percent_low_fps, None);
    }

    /// The 1% low is the rate the slowest 1% of the frames run at.
    #[test]
    fn the_one_percent_low_averages_the_slowest_hundredth() {
        let mut frame_times = vec![10.0; 99];
        frame_times.push(100.0);
        // 100 frames, 1% is the single slowest frame.
        let low = one_percent_low_fps(frame_times.iter().copied()).expect("low value");
        assert!((low - 10.0).abs() < 1e-9, "got {low}");
        assert_eq!(one_percent_low_fps(std::iter::empty()), None);

        let mut wide = vec![10.0; 198];
        wide.push(20.0);
        wide.push(100.0);
        // 200 frames, 1% is the two slowest frames: (100 + 20) / 2 ms.
        let low = one_percent_low_fps(wide.iter().copied()).expect("low value");
        assert!((low - 1000.0 / 60.0).abs() < 1e-9, "got {low}");
    }

    /// The report's line names the frames, the pacing, the extremes, the
    /// percentiles and the 1% low.
    #[test]
    fn the_line_names_the_distribution() {
        let times = history(&[
            500.0, 16.0, 17.0, 18.0, 19.0, 20.0, 21.0, 22.0, 23.0, 24.0, 100.0,
        ]);
        let line = times.report(pacing()).line();
        assert!(
            line.starts_with("frame times: frames=11 gaps=10 kept=10 paced=yes(16.7ms)"),
            "{line}"
        );
        assert!(line.contains("startup=500.0ms"), "{line}");
        assert!(line.contains("fastest=16.0ms/62.5fps"), "{line}");
        assert!(line.contains("p50=20.0ms/50.0fps"), "{line}");
        assert!(line.contains("p90=24.0ms/41.7fps"), "{line}");
        assert!(line.contains("p99=100.0ms/10.0fps"), "{line}");
        assert!(line.contains("slowest=100.0ms/10.0fps"), "{line}");
        assert!(line.contains("1%low=10.0fps(100.0ms)"), "{line}");
    }
}
