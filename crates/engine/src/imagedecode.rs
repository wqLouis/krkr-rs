//! The image decode pool and the pending-image future.
//!
//! The pool decodes images on worker threads while the frame thread keeps
//! working. A decode that has not produced its pixels yet is a
//! [`PendingImage`]: the bitmap that will hold those pixels carries the
//! future, and every read of the pixels waits for it.
//!
//! The pool owns the workers, the queue, the byte accounting and the decode
//! accounting ([`DecodeStats`]). The codec it calls is supplied by the
//! composition root, because the concrete decoder lives in the bottom media
//! layer. Nothing here is a process global: two pools in one process do not
//! interact.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use api::media::{DecodedImage, ImageInfo};

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

/// The result slot and the wake-up a pending image shares with its waiters.
#[derive(Debug)]
struct PendingShared {
    /// The result slot.
    state: Mutex<ImageState>,
    /// Signalled when the state leaves [`ImageState::Pending`].
    ready: Condvar,
    /// Whether a caller is waiting for the result.
    demanded: AtomicBool,
    /// The decoded-image cache entry this decode fills, when the caller named
    /// one.
    cache_key: Mutex<Option<DecodedImageKey>>,
}

impl PendingShared {
    /// Publishes `outcome` when the image has not produced one yet.
    ///
    /// Returns whether the outcome was published; a cancelled image already
    /// holds one and keeps it.
    fn complete(&self, outcome: ImageOutcome) -> bool {
        let mut state = lock(&self.state);
        if matches!(&*state, ImageState::Pending) {
            *state = ImageState::Done(outcome);
            self.ready.notify_all();
            true
        } else {
            false
        }
    }

    /// Whether the image already holds a result.
    fn is_finished(&self) -> bool {
        let state = lock(&self.state);
        matches!(&*state, ImageState::Done(_))
    }
}

/// A decode that has not produced its pixels yet.
///
/// The future is created by [`DecodePool::submit`] or
/// [`DecodePool::submit_cached`] and handed to the bitmap that will hold the
/// pixels. Every read of those pixels waits for it: the wait is on this
/// image alone, never on the pool.
///
/// A cancelled image reports [`ImageOutcome::Cancelled`] to every waiter, and
/// a worker that finishes a cancelled job discards its result.
#[derive(Clone, Debug)]
pub struct PendingImage {
    /// The shared result slot and the wake-up.
    shared: Arc<PendingShared>,
}

impl PendingImage {
    /// Creates a pending image with no result yet.
    ///
    /// `cache_key` names the decoded-image cache entry the result fills,
    /// when there is one.
    fn new(cache_key: Option<DecodedImageKey>) -> Self {
        Self {
            shared: Arc::new(PendingShared {
                state: Mutex::new(ImageState::Pending),
                ready: Condvar::new(),
                demanded: AtomicBool::new(false),
                cache_key: Mutex::new(cache_key),
            }),
        }
    }

    /// The decoded-image cache entry this decode fills, when the caller named
    /// one.
    ///
    /// The scene's cache takes the decoded pixels under this key when the
    /// image resolves, so a later load of the same image is a cache hit
    /// instead of a second decode.
    pub fn cache_key(&self) -> Option<DecodedImageKey> {
        lock(&self.shared.cache_key).clone()
    }

    /// Marks the image demanded: a worker prefers a demanded job over a
    /// normal one.
    pub fn demand(&self) {
        self.shared.demanded.store(true, Ordering::SeqCst);
    }

    /// Whether a caller is waiting for the image.
    pub fn is_demanded(&self) -> bool {
        self.shared.demanded.load(Ordering::SeqCst)
    }

    /// Whether the decode has produced a result.
    pub fn is_ready(&self) -> bool {
        self.shared.is_finished()
    }

    /// The current state, without waiting.
    pub fn state(&self) -> ImageState {
        let state = lock(&self.shared.state);
        match &*state {
            ImageState::Pending => ImageState::Pending,
            ImageState::Done(outcome) => ImageState::Done(outcome.clone()),
        }
    }

    /// Waits until the decode produced a result and returns it.
    ///
    /// The wait releases the lock while it sleeps, and a result published
    /// between the check and the sleep is not missed.
    pub fn await_ready(&self) -> ImageOutcome {
        let mut state = lock(&self.shared.state);
        loop {
            match &*state {
                ImageState::Done(outcome) => return outcome.clone(),
                ImageState::Pending => {}
            }
            state = match self.shared.ready.wait(state) {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }

    /// Cancels the image.
    ///
    /// A waiter sees [`ImageOutcome::Cancelled`] and a worker discards the
    /// result it would have published. Returns whether the image was still
    /// waiting.
    pub fn cancel(&self) -> bool {
        self.shared.complete(ImageOutcome::Cancelled)
    }

    /// Publishes `outcome` when the image has not produced one yet.
    fn complete(&self, outcome: ImageOutcome) -> bool {
        self.shared.complete(outcome)
    }
}

/// The decode accounting the pool keeps.
///
/// The counters are the number of image decodes the pool ran and the time
/// they took. A worker updates them under the pool's own lock, so a counter
/// never crosses a thread boundary: the thread that owns the pool reads the
/// totals whenever it wants them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodeStats {
    /// The number of decodes the pool ran.
    pub decodes: u64,
    /// The accumulated time the pool spent decoding.
    pub elapsed: Duration,
}

/// The pool's configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodePoolConfig {
    /// The number of worker threads.
    pub workers: usize,
    /// The maximum number of queued jobs.
    pub queue_limit: usize,
    /// The maximum number of bytes the pool may hold for the queued and the
    /// in-flight decodes.
    ///
    /// A job is charged the size of its bytes plus the size of the pixels it
    /// will produce, so the limit bounds the pool's footprint.
    pub memory_limit: u64,
}

impl Default for DecodePoolConfig {
    fn default() -> Self {
        Self {
            workers: default_worker_count(),
            queue_limit: 64,
            memory_limit: 64 * 1024 * 1024,
        }
    }
}

/// The worker count the pool uses when nothing else is configured.
///
/// Two cores are left to the frame thread and the host, so the pool never
/// competes with the renderer for them.
fn default_worker_count() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |count| count.get().saturating_sub(2))
        .max(1)
}

/// The pool's failures.
#[derive(Debug, thiserror::Error)]
pub enum DecodePoolError {
    /// A worker thread could not be started.
    #[error("could not start an image decode worker: {message}")]
    Worker {
        /// The platform's message.
        message: String,
    },
    /// The configuration is not usable.
    #[error("the image decode pool configuration is invalid: {message}")]
    Config {
        /// What is wrong with the configuration.
        message: String,
    },
    /// The queue is full.
    #[error("the image decode pool queue holds its {limit} jobs")]
    QueueFull {
        /// The configured queue limit.
        limit: usize,
    },
    /// The memory limit would be exceeded.
    #[error("the image decode pool would exceed its memory limit of {limit} bytes")]
    MemoryLimit {
        /// The configured limit.
        limit: u64,
    },
    /// The job's byte accounting does not fit.
    #[error("the image decode job's size does not fit in the pool's accounting")]
    SizeOverflow,
    /// The pool has no more job identities to hand out.
    #[error("the image decode pool has no more job identities")]
    IdExhausted,
}

/// The identity of one submitted decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JobId(u64);

/// One queued decode.
struct Job {
    /// The job's identity.
    id: JobId,
    /// The future the result is published into.
    pending: PendingImage,
    /// The bytes to decode.
    bytes: Arc<[u8]>,
    /// The storage name the bytes came from.
    hint: Option<String>,
    /// The bytes this job holds against the pool's memory limit.
    charged: u64,
}

/// The queue and the byte accounting the workers and the frame thread share.
struct PoolState {
    /// The jobs waiting for a worker.
    queue: VecDeque<Job>,
    /// The bytes the queued and the in-flight jobs hold.
    bytes: u64,
    /// Whether the pool is shutting down.
    stopping: bool,
    /// The number of decodes the workers ran.
    decodes: u64,
    /// The accumulated time the workers spent decoding.
    decode_time: Duration,
}

/// The queue and the wake-up the workers and the frame thread share.
struct PoolShared {
    /// The pool's only lock.
    state: Mutex<PoolState>,
    /// Signalled when a job is queued or the pool stops.
    wake: Condvar,
}

/// The decode pool.
///
/// The pool owns its worker threads, its queue and its byte accounting. It is
/// middle-layer state: the engine owns it, the composition root installs the
/// decoder, and the compatibility layer submits and awaits jobs through it.
///
/// A job nobody waits for is *normal*; a job a caller or the frame barrier
/// waits for is *demanded*. A worker takes a demanded job first, so a
/// script's load is never delayed behind a prefetch backlog.
pub struct DecodePool {
    /// The queue and the byte accounting the workers and the frame thread
    /// share.
    shared: Arc<PoolShared>,
    /// The worker threads, joined when the pool drops.
    workers: Vec<JoinHandle<()>>,
    /// The configuration the pool was started with.
    config: DecodePoolConfig,
    /// The identity of the next submitted job.
    next_id: u64,
}

impl DecodePool {
    /// Starts the pool.
    ///
    /// # Errors
    ///
    /// Returns [`DecodePoolError::Config`] when the configuration asks for no
    /// worker or no queue, and [`DecodePoolError::Worker`] when a thread
    /// cannot be started.
    pub fn new(
        decoder: Arc<dyn ImageDecoder>,
        config: DecodePoolConfig,
    ) -> Result<Self, DecodePoolError> {
        if config.workers == 0 {
            return Err(DecodePoolError::Config {
                message: "the pool needs at least one worker".to_string(),
            });
        }
        if config.queue_limit == 0 {
            return Err(DecodePoolError::Config {
                message: "the pool needs a queue of at least one job".to_string(),
            });
        }
        let shared = Arc::new(PoolShared {
            state: Mutex::new(PoolState {
                queue: VecDeque::new(),
                bytes: 0,
                stopping: false,
                decodes: 0,
                decode_time: Duration::ZERO,
            }),
            wake: Condvar::new(),
        });
        let mut workers = Vec::with_capacity(config.workers);
        for index in 0..config.workers {
            let worker_shared = Arc::clone(&shared);
            let worker_decoder = Arc::clone(&decoder);
            let handle = std::thread::Builder::new()
                .name(format!("krkr-image-decode-{index}"))
                .spawn(move || worker_loop(worker_decoder, worker_shared));
            match handle {
                Ok(handle) => workers.push(handle),
                Err(error) => {
                    // The workers that already started must not outlive the
                    // pool: they are stopped and joined before the error
                    // goes out.
                    stop_workers(&shared, workers);
                    return Err(DecodePoolError::Worker {
                        message: error.to_string(),
                    });
                }
            }
        }
        Ok(Self {
            shared,
            workers,
            config,
            next_id: 0,
        })
    }

    /// Submits a decode and returns the future its result lands in.
    ///
    /// `expected_bytes` is the size of the pixels the decode will produce; it
    /// is what the job is charged against the memory limit. `hint` is the
    /// storage name the bytes were read from.
    ///
    /// # Errors
    ///
    /// Returns [`DecodePoolError::QueueFull`] when the queue is full,
    /// [`DecodePoolError::MemoryLimit`] when the job would push the pool past
    /// its limit, [`DecodePoolError::SizeOverflow`] when the job's byte
    /// accounting does not fit and [`DecodePoolError::IdExhausted`] when the
    /// pool has no more job identities. The caller then decodes
    /// synchronously; the pool never drops a job silently.
    pub fn submit(
        &mut self,
        bytes: Arc<[u8]>,
        hint: Option<String>,
        expected_bytes: u64,
    ) -> Result<(JobId, PendingImage), DecodePoolError> {
        self.enqueue(None, bytes, hint, expected_bytes)
    }

    /// Submits a decode that fills the decoded-image cache entry `key`.
    ///
    /// The scene's cache takes the decoded pixels under `key` when the image
    /// resolves, so a later load of the same image is a cache hit instead of
    /// a second decode. `key` is the entry the synchronous load path stores
    /// under, so both paths share one cache entry.
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
    ) -> Result<(JobId, PendingImage), DecodePoolError> {
        self.enqueue(Some(key), bytes, hint, expected_bytes)
    }

    /// Queues a decode and returns the future its result lands in.
    ///
    /// `cache_key` names the decoded-image cache entry the result fills, when
    /// the caller wants the scene's cache to keep it.
    fn enqueue(
        &mut self,
        cache_key: Option<DecodedImageKey>,
        bytes: Arc<[u8]>,
        hint: Option<String>,
        expected_bytes: u64,
    ) -> Result<(JobId, PendingImage), DecodePoolError> {
        let byte_len = u64::try_from(bytes.len()).map_err(|_| DecodePoolError::SizeOverflow)?;
        let charged = byte_len
            .checked_add(expected_bytes)
            .ok_or(DecodePoolError::SizeOverflow)?;
        let pending = PendingImage::new(cache_key);
        let id = JobId(self.next_id);
        let Some(next_id) = self.next_id.checked_add(1) else {
            return Err(DecodePoolError::IdExhausted);
        };
        self.next_id = next_id;
        {
            let mut state = lock(&self.shared.state);
            if state.stopping {
                return Err(DecodePoolError::Config {
                    message: "the image decode pool is stopping".to_string(),
                });
            }
            if state.queue.len() >= self.config.queue_limit {
                return Err(DecodePoolError::QueueFull {
                    limit: self.config.queue_limit,
                });
            }
            if state.bytes.saturating_add(charged) > self.config.memory_limit {
                return Err(DecodePoolError::MemoryLimit {
                    limit: self.config.memory_limit,
                });
            }
            state.bytes = state.bytes.saturating_add(charged);
            state.queue.push_back(Job {
                id,
                pending: pending.clone(),
                bytes,
                hint,
                charged,
            });
        }
        self.shared.wake.notify_one();
        Ok((id, pending))
    }

    /// Cancels a queued job.
    ///
    /// A running job is not preempted; its worker discards the result when
    /// it lands. Returns whether the job was still queued.
    pub fn cancel(&mut self, id: JobId) -> bool {
        let removed = {
            let mut state = lock(&self.shared.state);
            let index = state.queue.iter().position(|job| job.id == id);
            match index {
                Some(index) => {
                    let job = state.queue.remove(index);
                    if let Some(job) = &job {
                        state.bytes = state.bytes.saturating_sub(job.charged);
                    }
                    job
                }
                None => None,
            }
        };
        match removed {
            Some(job) => {
                job.pending.complete(ImageOutcome::Cancelled);
                true
            }
            None => false,
        }
    }

    /// The number of jobs waiting for a worker.
    pub fn queued(&self) -> usize {
        lock(&self.shared.state).queue.len()
    }

    /// The number of bytes the pool currently holds.
    pub fn held_bytes(&self) -> u64 {
        lock(&self.shared.state).bytes
    }

    /// The decode accounting: how many decodes the pool ran and how long they
    /// took.
    ///
    /// The counters cover every decode the pool performed, the successful
    /// ones and the failed ones alike.
    pub fn stats(&self) -> DecodeStats {
        let state = lock(&self.shared.state);
        DecodeStats {
            decodes: state.decodes,
            elapsed: state.decode_time,
        }
    }
}

impl Drop for DecodePool {
    fn drop(&mut self) {
        stop_workers(&self.shared, std::mem::take(&mut self.workers));
    }
}

/// Stops the pool's workers and joins them.
///
/// The queued jobs are cancelled, so a waiter on one of them wakes instead of
/// waiting for a decode that will never run. A running decode is not
/// preempted: the join waits for it, and it publishes its own result.
fn stop_workers(shared: &PoolShared, workers: Vec<JoinHandle<()>>) {
    {
        let mut state = lock(&shared.state);
        state.stopping = true;
        while let Some(job) = state.queue.pop_front() {
            state.bytes = state.bytes.saturating_sub(job.charged);
            job.pending.complete(ImageOutcome::Cancelled);
        }
    }
    shared.wake.notify_all();
    for worker in workers {
        if worker.join().is_err() {
            log::warn!("engine: an image decode worker stopped with a panic");
        }
    }
}

/// Locks a mutex, recovering from a poisoned lock.
///
/// No code path panics while the lock is held, so the recovery cannot expose
/// a half-updated state.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Takes the next job: a demanded one when the queue holds one, else the
/// oldest.
fn take_next_job(state: &mut PoolState) -> Option<Job> {
    if let Some(index) = state.queue.iter().position(|job| job.pending.is_demanded()) {
        return state.queue.remove(index);
    }
    state.queue.pop_front()
}

/// Returns a job's bytes to the pool's accounting.
fn release_bytes(shared: &PoolShared, charged: u64) {
    let mut state = lock(&shared.state);
    state.bytes = state.bytes.saturating_sub(charged);
}

/// Records the decode a worker ran: the count and the time it took.
///
/// The counters live in the pool's own state, so a worker updates them under
/// the pool's lock and the thread that owns the pool reads the totals. No
/// counter crosses a thread boundary.
fn record_decode(shared: &PoolShared, elapsed: Duration) {
    let mut state = lock(&shared.state);
    state.decodes = state.decodes.saturating_add(1);
    state.decode_time = state.decode_time.saturating_add(elapsed);
}

/// Publishes a failure for a job whose worker stops before it does.
///
/// The guard holds the future of the job a worker is running. Its drop
/// publishes a failure when the job has not produced a result, so a waiter
/// wakes with a failure instead of waiting for a worker that is gone.
struct JobGuard {
    /// The future the result is published into.
    pending: PendingImage,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.pending.complete(ImageOutcome::Failed(
            "the image decode worker stopped before it published a result".to_string(),
        ));
    }
}

/// The message of a panic payload.
///
/// The payload is a `Box<dyn Any + Send>`: a string payload carries the
/// message the panicking code wrote, and anything else is reported as an
/// unnamed panic. The panic is never silenced: it becomes the job's failed
/// outcome, and the waiter sees it.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return (*message).clone();
    }
    "the image decoder panicked".to_string()
}

/// The worker loop: take a job, decode it outside the lock and publish the
/// result.
fn worker_loop(decoder: Arc<dyn ImageDecoder>, shared: Arc<PoolShared>) {
    loop {
        let job = {
            let mut state = lock(&shared.state);
            loop {
                if state.stopping {
                    return;
                }
                if let Some(job) = take_next_job(&mut state) {
                    break job;
                }
                state = match shared.wake.wait(state) {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
            }
        };
        // The guard covers every path out of this iteration: a worker that
        // stops for any reason leaves the waiter with a result.
        let _guard = JobGuard {
            pending: job.pending.clone(),
        };
        // A job that was cancelled before it ran is dropped without
        // decoding.
        if job.pending.is_ready() {
            release_bytes(&shared, job.charged);
            continue;
        }
        let started = Instant::now();
        let outcome = match std::panic::catch_unwind(AssertUnwindSafe(|| {
            decoder.decode(&job.bytes, job.hint.as_deref())
        })) {
            Ok(Ok(image)) => ImageOutcome::Ready(Arc::new(image)),
            Ok(Err(error)) => ImageOutcome::Failed(error.to_string()),
            Err(payload) => ImageOutcome::Failed(panic_message(payload.as_ref())),
        };
        record_decode(&shared, started.elapsed());
        release_bytes(&shared, job.charged);
        job.pending.complete(outcome);
    }
}
