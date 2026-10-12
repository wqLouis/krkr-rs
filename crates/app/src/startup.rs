//! Start-up cost counters shared by the installed decoder seams.
//!
//! The composition root installs the image, audio and video decoders as
//! closures and then runs the startup scripts. Those closures are the only
//! place the number of decodes and their cost is observable without a hook in
//! the codec crates, so the counters live in one `Rc` shared with the
//! closures and are read once after the startup sequence.
//!
//! The counters are single-threaded and start empty; the composition root
//! snapshots them after startup and never reads them on the frame path.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

/// Image, audio and video decoding observed since the composition root
/// installed the decoder seams.
#[derive(Default)]
pub(crate) struct StartupStats {
    /// Full image decodes that reached the installed codec.
    images_decoded: Cell<u64>,
    /// Accumulated image decode time.
    image_decode: Cell<Duration>,
    /// Audio decoders the installed seam opened successfully.
    audio_decoders_opened: Cell<u64>,
    /// Video streams the installed seam opened successfully.
    video_decoders_opened: Cell<u64>,
    /// Scripts the startup sequence ran through `exec_storage`.
    startup_scripts: Cell<u64>,
    /// Accumulated `exec_storage` time.
    startup_script_time: Cell<Duration>,
}

/// The snapshot read after the startup sequence completes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct StartupSnapshot {
    /// Number of full image decodes.
    pub(crate) images_decoded: u64,
    /// Total image decode time.
    pub(crate) image_decode: Duration,
    /// Number of audio decoders opened.
    pub(crate) audio_decoders_opened: u64,
    /// Number of video streams opened.
    pub(crate) video_decoders_opened: u64,
    /// Number of startup scripts run through `exec_storage`.
    pub(crate) startup_scripts: u64,
    /// Total `exec_storage` time.
    pub(crate) startup_script_time: Duration,
}

impl StartupStats {
    /// Creates an empty counter set.
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// Records one full image decode that took `elapsed`.
    pub(crate) fn record_image_decode(&self, elapsed: Duration) {
        self.images_decoded.set(self.images_decoded.get() + 1);
        self.image_decode
            .set(self.image_decode.get().saturating_add(elapsed));
    }

    /// Records one audio decoder that opened successfully.
    pub(crate) fn record_audio_decoder(&self) {
        self.audio_decoders_opened
            .set(self.audio_decoders_opened.get() + 1);
    }

    /// Records one video stream that opened successfully.
    pub(crate) fn record_video_decoder(&self) {
        self.video_decoders_opened
            .set(self.video_decoders_opened.get() + 1);
    }

    /// Records one startup script run through `exec_storage` that took
    /// `elapsed`.
    pub(crate) fn record_startup_script(&self, elapsed: Duration) {
        self.startup_scripts.set(self.startup_scripts.get() + 1);
        self.startup_script_time
            .set(self.startup_script_time.get().saturating_add(elapsed));
    }

    /// Reads the current counters.
    pub(crate) fn snapshot(&self) -> StartupSnapshot {
        StartupSnapshot {
            images_decoded: self.images_decoded.get(),
            image_decode: self.image_decode.get(),
            audio_decoders_opened: self.audio_decoders_opened.get(),
            video_decoders_opened: self.video_decoders_opened.get(),
            startup_scripts: self.startup_scripts.get(),
            startup_script_time: self.startup_script_time.get(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::StartupStats;

    /// The snapshot moves every counter once, and the durations accumulate.
    #[test]
    fn snapshot_reports_every_recorded_event() {
        let stats = StartupStats::new();
        stats.record_image_decode(Duration::from_millis(3));
        stats.record_image_decode(Duration::from_millis(4));
        stats.record_audio_decoder();
        stats.record_video_decoder();
        stats.record_startup_script(Duration::from_millis(5));

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.images_decoded, 2);
        assert_eq!(snapshot.image_decode, Duration::from_millis(7));
        assert_eq!(snapshot.audio_decoders_opened, 1);
        assert_eq!(snapshot.video_decoders_opened, 1);
        assert_eq!(snapshot.startup_scripts, 1);
        assert_eq!(snapshot.startup_script_time, Duration::from_millis(5));
    }
}
