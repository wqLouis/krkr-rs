//! The image decode path on the engine's job pool.
//!
//! The pool decodes images on worker threads while the frame thread keeps
//! working. A decode that has not produced its pixels yet is a
//! [`PendingImage`]: the bitmap that will hold those pixels carries the
//! handle, and every read of the pixels waits for it.
//!
//! The pool itself is general: it runs the CPU-bound jobs the engine
//! submits, and a job is a closure over a value the caller casts back (see
//! [`crate::jobpool`]). This module is the image decode path on top of it:
//! the codec the workers decode with, the pass a load runs over the decoded
//! pixels, and the typed handle a pending bitmap carries.
//!
//! The pool owns the workers, the queue, the byte accounting and the job
//! accounting ([`JobStats`]). The codec it calls is supplied by the
//! composition root, because the concrete decoder lives in the bottom media
//! layer. Nothing here is a process global: two pools in one process do not
//! interact.

use std::sync::Arc;

use api::media::{DecodedImage, ImageInfo};

use crate::jobpool::{
    JobId, JobOutcome, JobPool, JobPoolConfig, JobPoolError, JobState, JobStats, JobValue, JobWork,
    PendingJob,
};
use crate::scene::DecodedImageKey;

/// The failure of an image decode.
///
/// The text is the bottom codec's own message, so a script that sees a
/// decode failure sees the same words a synchronous decode would have
/// produced.
#[derive(Debug, thiserror::Error)]
pub enum ImageDecodeError {
    /// The decode failed.
    #[error("{message}")]
    Failed {
        /// The bottom codec's message.
        message: String,
    },
}

/// A probed image size that cannot be represented.
#[derive(Debug, thiserror::Error)]
pub enum ImageSizeError {
    /// The pixel count of the image does not fit in memory.
    #[error("an image of {width}x{height} pixels is too large to address")]
    TooLarge {
        /// The probed width.
        width: u32,
        /// The probed height.
        height: u32,
    },
}

/// The image codec the pool drives.
///
/// The implementation lives in the bottom media layer; the middle layer owns
/// the value and calls it. A decoder is stateless and thread-safe: the pool
/// calls it from several worker threads at once, so the implementation holds
/// no per-call state.
pub trait ImageDecoder: Send + Sync {
    /// Probes the header of `bytes` without decoding the pixels.
    ///
    /// `hint` is the storage name the bytes were read from. It steers the
    /// format choice and is never trusted over the content.
    fn probe(&self, bytes: &[u8], hint: Option<&str>) -> Result<ImageInfo, ImageDecodeError>;

    /// Decodes `bytes` into tightly packed RGBA8.
    ///
    /// `hint` is the storage name the bytes were read from, exactly as for
    /// [`ImageDecoder::probe`].
    fn decode(&self, bytes: &[u8], hint: Option<&str>) -> Result<DecodedImage, ImageDecodeError>;
}

/// What a pending image produced.
#[derive(Clone, Debug)]
pub enum ImageOutcome {
    /// The decode produced these pixels.
    Ready(Arc<DecodedImage>),
    /// The decode failed; the message names the cause.
    Failed(String),
    /// The job was cancelled before it produced a result.
    Cancelled,
}

/// The state of a pending image.
#[derive(Clone, Debug)]
pub enum ImageState {
    /// The decode has not produced a result yet.
    Pending,
    /// The decode produced a result.
    Done(ImageOutcome),
}

/// A decode that has not produced its pixels yet.
///
/// The handle is created by [`DecodePool::submit`] or
/// [`DecodePool::submit_cached`] and handed to the bitmap that will hold the
/// pixels. Every read of those pixels waits for it: the wait is on this
/// image alone, never on the pool.
///
/// A cancelled image reports [`ImageOutcome::Cancelled`] to every waiter, and
/// a worker that finishes a cancelled job discards its result.
#[derive(Clone, Debug)]
pub struct PendingImage {
    /// The handle the decode's result lands in.
    job: PendingJob,
    /// The decoded-image cache entry this decode fills, when the caller
    /// named one.
    cache_key: Option<DecodedImageKey>,
}

impl PendingImage {
    /// Creates a pending image over the job its result lands in.
    ///
    /// `cache_key` names the decoded-image cache entry the result fills,
    /// when there is one.
    fn new(job: PendingJob, cache_key: Option<DecodedImageKey>) -> Self {
        Self { job, cache_key }
    }

    /// The decoded-image cache entry this decode fills, when the caller named
    /// one.
    ///
    /// The scene's cache takes the decoded pixels under this key when the
    /// image resolves, so a later load of the same image is a cache hit
    /// instead of a second decode.
    pub fn cache_key(&self) -> Option<DecodedImageKey> {
        self.cache_key.clone()
    }

    /// Whether this decode fills the decoded-image cache entry `key`.
    pub fn fills_cache_entry(&self, key: &DecodedImageKey) -> bool {
        self.cache_key.as_ref() == Some(key)
    }

    /// Whether `other` waits for the same decode as this one.
    ///
    /// Two clones of one pending image share the result slot: the pixels
    /// land in every holder, and cancelling the decode takes them all.
    pub fn same_decode(&self, other: &PendingImage) -> bool {
        self.job.same_job(&other.job)
    }

    /// Marks the image demanded: a worker prefers a demanded job over a
    /// normal one.
    pub fn demand(&self) {
        self.job.demand();
    }

    /// Whether a caller is waiting for the image.
    pub fn is_demanded(&self) -> bool {
        self.job.is_demanded()
    }

    /// Whether the decode has produced a result.
    pub fn is_ready(&self) -> bool {
        self.job.is_ready()
    }

    /// The current state, without waiting.
    pub fn state(&self) -> ImageState {
        match self.job.state() {
            JobState::Pending => ImageState::Pending,
            JobState::Done(outcome) => ImageState::Done(image_outcome(outcome)),
        }
    }

    /// Waits until the decode produced a result and returns it.
    ///
    /// The wait releases the lock while it sleeps, and a result published
    /// between the check and the sleep is not missed.
    pub fn await_ready(&self) -> ImageOutcome {
        image_outcome(self.job.await_ready())
    }

    /// Cancels the image.
    ///
    /// A waiter sees [`ImageOutcome::Cancelled`] and a worker discards the
    /// result it would have published. Returns whether the image was still
    /// waiting.
    pub fn cancel(&self) -> bool {
        self.job.cancel()
    }
}

/// Turns the outcome of a decode job into the outcome of the image.
///
/// A decode that produced a value of another type is a failed decode: the
/// message names the type the job produced and the type the image needs.
fn image_outcome(outcome: JobOutcome) -> ImageOutcome {
    match outcome {
        JobOutcome::Ready(value) => match value.downcast::<DecodedImage>() {
            Ok(image) => ImageOutcome::Ready(image),
            Err(error) => ImageOutcome::Failed(error.to_string()),
        },
        JobOutcome::Failed(message) => ImageOutcome::Failed(message),
        JobOutcome::Cancelled => ImageOutcome::Cancelled,
    }
}

/// A pass over decoded pixels that a worker runs before it publishes the
/// result.
///
/// The load that submits a job names the pass; the pool carries it opaquely,
/// so a caller can transform the pixels without the pool knowing what the
/// transform means. A pass runs on a worker thread, so it is `Send + Sync`
/// and borrows no frame-thread state. A pass that fails publishes its message
/// as the job's failure: the load already returned, so there is no call left
/// to raise at.
pub type DecodePass = dyn Fn(DecodedImage) -> Result<DecodedImage, String> + Send + Sync;

/// The engine's decode pool: the job pool and the codec its workers decode
/// with.
///
/// The pool owns the worker threads, the queue and the byte accounting. It
/// is middle-layer state: the engine owns it, the composition root installs
/// the codec, and the compatibility layer submits decodes through it.
///
/// A decode nobody waits for is *normal*; a decode a caller or the frame
/// barrier waits for is *demanded*. A worker takes a demanded job first, so
/// a script's load is never delayed behind a prefetch backlog.
///
/// The pool also carries the jobs that are not decodes: a caller submits
/// them with [`DecodePool::submit_job`], and casts the value back when the
/// job lands.
pub struct DecodePool {
    /// The pool the jobs run on.
    jobs: JobPool,
    /// The codec the workers decode with.
    decoder: Arc<dyn ImageDecoder>,
}

impl DecodePool {
    /// Starts the pool.
    ///
    /// # Errors
    ///
    /// Returns [`JobPoolError::Config`] when the configuration asks for no
    /// worker or no queue, and [`JobPoolError::Worker`] when a thread
    /// cannot be started.
    pub fn new(
        decoder: Arc<dyn ImageDecoder>,
        config: JobPoolConfig,
    ) -> Result<Self, JobPoolError> {
        Ok(Self {
            jobs: JobPool::new(config)?,
            decoder,
        })
    }

    /// Submits a decode and returns the handle its result lands in.
    ///
    /// `expected_bytes` is the size of the pixels the decode will produce; it
    /// is what the job is charged against the memory limit. `hint` is the
    /// storage name the bytes were read from. `pass` is the pass the worker
    /// runs over the decoded pixels before it publishes them, when the load
    /// needs one.
    ///
    /// # Errors
    ///
    /// Returns [`JobPoolError::QueueFull`] when the queue is full,
    /// [`JobPoolError::MemoryLimit`] when the job would push the pool past
    /// its limit, [`JobPoolError::SizeOverflow`] when the job's byte
    /// accounting does not fit and [`JobPoolError::IdExhausted`] when the
    /// pool has no more job identities. The caller then decodes
    /// synchronously; the pool never drops a job silently.
    pub fn submit(
        &mut self,
        bytes: Arc<[u8]>,
        hint: Option<String>,
        expected_bytes: u64,
        pass: Option<Arc<DecodePass>>,
    ) -> Result<(JobId, PendingImage), JobPoolError> {
        self.enqueue(None, bytes, hint, expected_bytes, pass)
    }

    /// Submits a decode that fills the decoded-image cache entry `key`.
    ///
    /// The scene's cache takes the decoded pixels under `key` when the image
    /// resolves, so a later load of the same image is a cache hit instead of
    /// a second decode. `key` is the entry the synchronous load path stores
    /// under, so both paths share one cache entry. `pass` is the pass the
    /// worker runs over the decoded pixels, when the load needs one; the
    /// cached entry is the pass's output, the pixels the load would install.
    ///
    /// # Errors
    ///
    /// The same as [`DecodePool::submit`].
    pub fn submit_cached(
        &mut self,
        key: DecodedImageKey,
        bytes: Arc<[u8]>,
        hint: Option<String>,
        expected_bytes: u64,
        pass: Option<Arc<DecodePass>>,
    ) -> Result<(JobId, PendingImage), JobPoolError> {
        self.enqueue(Some(key), bytes, hint, expected_bytes, pass)
    }

    /// Submits a job that is not an image decode.
    ///
    /// `work` is the CPU-bound work the worker performs and `charge` is the
    /// number of bytes the job holds against the memory limit. The value the
    /// job produces is cast back by the caller, with
    /// [`JobValue::downcast`].
    ///
    /// # Errors
    ///
    /// The same as [`DecodePool::submit`], without the byte accounting of a
    /// decode: the charge is the number the caller computed.
    pub fn submit_job(
        &mut self,
        work: Arc<JobWork>,
        charge: u64,
    ) -> Result<(JobId, PendingJob), JobPoolError> {
        self.jobs.submit(work, charge)
    }

    /// Queues a decode and returns the handle its result lands in.
    ///
    /// `cache_key` names the decoded-image cache entry the result fills, when
    /// the caller wants the scene's cache to keep it; `pass` is the
    /// post-decode pass the worker runs, when the load needs one.
    fn enqueue(
        &mut self,
        cache_key: Option<DecodedImageKey>,
        bytes: Arc<[u8]>,
        hint: Option<String>,
        expected_bytes: u64,
        pass: Option<Arc<DecodePass>>,
    ) -> Result<(JobId, PendingImage), JobPoolError> {
        let byte_len = u64::try_from(bytes.len()).map_err(|_| JobPoolError::SizeOverflow)?;
        let charge = byte_len
            .checked_add(expected_bytes)
            .ok_or(JobPoolError::SizeOverflow)?;
        let decoder = Arc::clone(&self.decoder);
        let work: Arc<JobWork> = Arc::new(move || {
            let image = decoder
                .decode(&bytes, hint.as_deref())
                .map_err(|error| error.to_string())?;
            let image = match pass.as_deref() {
                Some(pass) => pass(image)?,
                None => image,
            };
            Ok(JobValue::new(image))
        });
        let (id, job) = self.jobs.submit(work, charge)?;
        Ok((id, PendingImage::new(job, cache_key)))
    }

    /// Cancels a queued job.
    ///
    /// A running job is not preempted; its worker discards the result when
    /// it lands. Returns whether the job was still queued.
    pub fn cancel(&mut self, id: JobId) -> bool {
        self.jobs.cancel(id)
    }

    /// The number of jobs waiting for a worker.
    pub fn queued(&self) -> usize {
        self.jobs.queued()
    }

    /// The number of bytes the pool currently holds.
    pub fn held_bytes(&self) -> u64 {
        self.jobs.held_bytes()
    }

    /// The job accounting: how many jobs the pool ran and how long they
    /// took.
    ///
    /// The counters cover every job the pool performed, the decodes and the
    /// other work alike.
    pub fn stats(&self) -> JobStats {
        self.jobs.stats()
    }
}
