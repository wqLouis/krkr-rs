//! Wiring for the L1 `media` video decoder into the L3 `compat` host.
//!
//! `compat` never links `media`: the `VideoOverlay` native and the
//! `LayerExMovie` members read decoded streams through the [`compat::VideoSource`]
//! seam. This module is the composition root that installs the L1-backed
//! source: it reads each stream's bytes through the L2 [`Storage`], opens them
//! with [`media::open_video`] and serves frames with [`media::present_video`],
//! caching one decoder per path.
//!
//! One source is shared by the `VideoOverlay` seam
//! ([`compat::set_video_source`]) and the movie seam
//! ([`compat::set_movie_source`]), so a file opened through either surface uses
//! the same decoder.
//!
//! # Headless
//!
//! Decoding and layer compositing need no display, so the source is installed
//! for headless and windowed hosts alike. The one video mode with no
//! display-independent meaning is `vomOverlay`, whose frames the reference
//! presents through an OS overlay window; leaving it alone is the faithful
//! behaviour rather than fabricating a surface.
//!
//! # Storage lifetime
//!
//! [`compat::VideoSource`] is stored in the compat state and is therefore
//! `'static`, while the bytes come from the L2 [`Storage`] the game host owns.
//! The source holds the storage address; [`install_video_source`] documents the
//! ordering invariant that keeps it valid.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use api::media::{RgbaFrame, VideoSpec};
use compat::{CompatError, CompatHost, VideoSource, set_movie_source, set_video_source};
use engine::Storage;
use media::{Media, VideoDecoderState, open_video, present_video};

/// One opened stream: its spec and its decoder.
struct OpenStream {
    /// The stream description read when the decoder was opened.
    spec: VideoSpec,
    /// The stateless decoder for this stream.
    decoder: VideoDecoderState,
}

/// The source's shared decoder cache.
struct Inner {
    /// The stateless L1 media backend.
    media: Media,
    /// The game storage the streams are read from.
    storage: *const Storage,
    /// Open decoders, keyed by the storage path they were opened from.
    streams: HashMap<String, OpenStream>,
}

impl Inner {
    /// Open `path` if it is not cached, returning the cached stream.
    fn open(&mut self, path: &str) -> Option<&OpenStream> {
        if !self.streams.contains_key(path) {
            // SAFETY: `install_video_source` requires the storage to outlive
            // the compat host that owns this source.
            let storage = unsafe { &*self.storage };
            let bytes = match storage.read(path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    log::warn!("video: cannot read {path:?}: {error}");
                    return None;
                }
            };
            let media = self.media.clone();
            match open_video(&media, Arc::from(bytes.into_boxed_slice()), Some(path)) {
                Ok((spec, decoder)) => {
                    self.streams
                        .insert(path.to_string(), OpenStream { spec, decoder });
                }
                Err(error) => {
                    log::warn!("video: cannot open {path:?}: {error}");
                    return None;
                }
            }
        }
        self.streams.get(path)
    }

    /// Decode the frame at `at_ms` for `path`, opening the stream if needed.
    fn frame(&mut self, path: &str, at_ms: i64) -> Option<RgbaFrame> {
        if self.open(path).is_none() {
            return None;
        }
        let media = self.media.clone();
        let stream = self.streams.get_mut(path)?;
        let at = at_ms.max(0) as u64;
        match present_video(&media, &mut stream.decoder, at) {
            Ok(frame) => Some(frame),
            Err(error) => {
                log::warn!("video: cannot decode {path:?} at {at} ms: {error}");
                None
            }
        }
    }
}

/// A [`VideoSource`] backed by the L1 `media` decoder and the L2 storage.
///
/// Cloning shares the same decoder cache, so the `VideoOverlay` and movie
/// seams decode a file once.
#[derive(Clone)]
struct MediaVideoSource {
    /// The shared decoder cache.
    inner: Rc<RefCell<Inner>>,
}

impl VideoSource for MediaVideoSource {
    fn spec(&self, path: &str) -> Option<VideoSpec> {
        self.inner.borrow_mut().open(path).map(|stream| stream.spec)
    }

    fn frame_at(&mut self, path: &str, at_ms: i64) -> Option<RgbaFrame> {
        self.inner.borrow_mut().frame(path, at_ms)
    }
}

/// Installs the `media`-backed video source on `host` for both the
/// `VideoOverlay` and `LayerExMovie` seams.
///
/// # Safety
///
/// `storage` must stay alive for as long as `host` remains installed: the
/// installed source reads through its address. [`crate::GameHost`] declares the
/// VM host before the storage, so the source drops first and the invariant
/// holds.
///
/// # Errors
///
/// Returns [`CompatError::Engine`] when `host` is not installed (there is no
/// bound context to install the source on).
pub unsafe fn install_video_source(
    host: &CompatHost,
    media: &Media,
    storage: &Storage,
) -> Result<(), CompatError> {
    let ctx = host.ctx()?;
    let source = MediaVideoSource {
        inner: Rc::new(RefCell::new(Inner {
            media: media.clone(),
            storage: storage as *const Storage,
            streams: HashMap::new(),
        })),
    };
    set_video_source(&ctx, Box::new(source.clone()));
    set_movie_source(&ctx, Box::new(source));
    Ok(())
}
