//! The engine's CPU-job pool.
//!
//! The pool runs CPU-bound work on worker threads while the frame thread
//! keeps working. A job that has not produced its result yet is a
//! [`PendingJob`]: the caller that submitted it carries the handle, and
//! every read of the result waits for it.
//!
//! The pool owns the workers, the queue, the byte accounting and the job
//! accounting ([`JobStats`]). A job is a closure over a value the caller
//! casts back ([`JobValue::downcast`]); the pool does not know what the job
//! computes, so every CPU-bound task in the engine shares it. The image
//! decode path is the first user: it submits its decodes as jobs and casts
//! the pixels back.
//!
//! A caller submits a job and casts the value it produced:
//!
//! ```
//! use std::sync::Arc;
//!
//! use engine::jobpool::{JobOutcome, JobPool, JobPoolConfig, JobValue, JobWork};
//!
//! let mut pool = JobPool::new(JobPoolConfig {
//!     workers: 1,
//!     ..JobPoolConfig::default()
//! })?;
//! // The work the worker performs: it produces the value the caller casts back.
//! let work: Arc<JobWork> = Arc::new(|| Ok(JobValue::new(String::from("parsed"))));
//! let (_id, pending) = pool.submit(work, 64)?;
//! match pending.await_ready() {
//!     JobOutcome::Ready(value) => {
//!         let text = value.downcast::<String>()?;
//!         assert_eq!(&*text, "parsed");
//!     }
//!     JobOutcome::Failed(message) => panic!("the job failed: {message}"),
//!     JobOutcome::Cancelled => panic!("the job was cancelled"),
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Nothing here is a process global: two pools in one process do not
//! interact.

use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The work one job performs.
///
/// The closure runs on a worker thread and returns the value the job
/// produced, or the message of the failure. It must not touch the state of
/// the thread that submitted it.
pub type JobWork = dyn Fn() -> Result<JobValue, String> + Send + Sync;

/// The value a completed job produced.
///
/// The value is opaque to the pool: the caller that submitted the job knows
/// what it asked for and casts the value back with [`JobValue::downcast`].
#[derive(Clone)]
pub struct JobValue {
    /// The value the job produced.
    inner: Arc<dyn Any + Send + Sync>,
    /// The name of the value's type, for the failure of a mismatched cast.
    type_name: &'static str,
}

impl JobValue {
    /// Wraps `value` as the result of a job.
    pub fn new<T: Any + Send + Sync>(value: T) -> Self {
        Self {
            inner: Arc::new(value),
            type_name: std::any::type_name::<T>(),
        }
    }

    /// Casts the value to `T`.
    ///
    /// # Errors
    ///
    /// Returns [`JobCastError::TypeMismatch`] when the job produced a value
    /// of another type. The value is never handed out as a substitute for
    /// the type the caller asked for.
    pub fn downcast<T: Any + Send + Sync>(&self) -> Result<Arc<T>, JobCastError> {
        Arc::clone(&self.inner)
            .downcast::<T>()
            .map_err(|_| JobCastError::TypeMismatch {
                expected: std::any::type_name::<T>(),
                actual: self.type_name,
            })
    }
}

impl fmt::Debug for JobValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobValue")
            .field("type", &self.type_name)
            .finish()
    }
}

/// The failure of a cast of a job's value.
#[derive(Debug, thiserror::Error)]
pub enum JobCastError {
    /// The job produced a value of another type.
    #[error("the job produced a {actual}, not the expected {expected}")]
    TypeMismatch {
        /// The type the caller asked for.
        expected: &'static str,
        /// The type the job produced.
        actual: &'static str,
    },
}

/// What a pending job produced.
#[derive(Clone)]
pub enum JobOutcome {
    /// The job produced this value.
    Ready(JobValue),
    /// The job failed; the message names the cause.
    Failed(String),
    /// The job was cancelled before it produced a result.
    Cancelled,
}

impl fmt::Debug for JobOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(value) => f.debug_tuple("Ready").field(value).finish(),
            Self::Failed(message) => f.debug_tuple("Failed").field(message).finish(),
            Self::Cancelled => f.write_str("Cancelled"),
        }
    }
}

/// The state of a pending job.
#[derive(Clone, Debug)]
pub enum JobState {
    /// The job has not produced a result yet.
    Pending,
    /// The job produced a result.
    Done(JobOutcome),
}

/// The result slot and the wake-up a pending job shares with its waiters.
#[derive(Debug)]
struct PendingShared {
    /// The result slot.
    state: Mutex<JobState>,
    /// Signalled when the state leaves [`JobState::Pending`].
    ready: Condvar,
    /// Whether a caller is waiting for the result.
    demanded: AtomicBool,
}

impl PendingShared {
    /// Publishes `outcome` when the job has not produced one yet.
    ///
    /// Returns whether the outcome was published; a cancelled job already
    /// holds one and keeps it.
    fn complete(&self, outcome: JobOutcome) -> bool {
        let mut state = lock(&self.state);
        if matches!(&*state, JobState::Pending) {
            *state = JobState::Done(outcome);
            self.ready.notify_all();
            true
        } else {
            false
        }
    }

    /// Whether the job already holds a result.
    fn is_finished(&self) -> bool {
        let state = lock(&self.state);
        matches!(&*state, JobState::Done(_))
    }
}

/// A job that has not produced its result yet.
///
/// The handle is created by [`JobPool::submit`] and handed to the caller
/// that submitted the job. Every access to the result waits for it: the
/// wait is on this job alone, never on the pool.
///
/// A cancelled job reports [`JobOutcome::Cancelled`] to every waiter, and a
/// worker that finishes a cancelled job discards its result.
#[derive(Clone, Debug)]
pub struct PendingJob {
    /// The shared result slot and the wake-up.
    shared: Arc<PendingShared>,
}

impl PendingJob {
    /// Creates a pending job with no result yet.
    fn new() -> Self {
        Self {
            shared: Arc::new(PendingShared {
                state: Mutex::new(JobState::Pending),
                ready: Condvar::new(),
                demanded: AtomicBool::new(false),
            }),
        }
    }

    /// Whether `other` waits for the same job as this one.
    ///
    /// Two clones of one pending job share the result slot: the result lands
    /// in every holder, and cancelling the job takes them all.
    pub(crate) fn same_job(&self, other: &PendingJob) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    /// Marks the job demanded: a worker prefers a demanded job over a
    /// normal one.
    pub fn demand(&self) {
        self.shared.demanded.store(true, Ordering::SeqCst);
    }

    /// Whether a caller is waiting for the job.
    pub fn is_demanded(&self) -> bool {
        self.shared.demanded.load(Ordering::SeqCst)
    }

    /// Whether the job has produced a result.
    pub fn is_ready(&self) -> bool {
        self.shared.is_finished()
    }

    /// The current state, without waiting.
    pub fn state(&self) -> JobState {
        let state = lock(&self.shared.state);
        match &*state {
            JobState::Pending => JobState::Pending,
            JobState::Done(outcome) => JobState::Done(outcome.clone()),
        }
    }

    /// Waits until the job produced a result and returns it.
    ///
    /// The wait releases the lock while it sleeps, and a result published
    /// between the check and the sleep is not missed.
    pub fn await_ready(&self) -> JobOutcome {
        let mut state = lock(&self.shared.state);
        loop {
            match &*state {
                JobState::Done(outcome) => return outcome.clone(),
                JobState::Pending => {}
            }
            state = match self.shared.ready.wait(state) {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }

    /// Cancels the job.
    ///
    /// A waiter sees [`JobOutcome::Cancelled`] and a worker discards the
    /// result it would have published. Returns whether the job was still
    /// waiting.
    pub fn cancel(&self) -> bool {
        self.shared.complete(JobOutcome::Cancelled)
    }

    /// Publishes `outcome` when the job has not produced one yet.
    fn complete(&self, outcome: JobOutcome) -> bool {
        self.shared.complete(outcome)
    }
}

/// The identity of one submitted job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JobId(u64);

/// The job accounting the pool keeps.
///
/// The counters are the number of jobs the pool ran and the time they took.
/// A worker updates them under the pool's own lock, so a counter never
/// crosses a thread boundary: the thread that owns the pool reads the
/// totals whenever it wants them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobStats {
    /// The number of jobs the pool ran.
    pub jobs: u64,
    /// The accumulated time the pool spent running jobs.
    pub elapsed: Duration,
}

/// The pool's configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobPoolConfig {
    /// The number of worker threads.
    pub workers: usize,
    /// The maximum number of queued jobs.
    pub queue_limit: usize,
    /// The maximum number of bytes the pool may hold for the queued and the
    /// in-flight jobs.
    ///
    /// A job is charged the number of bytes it holds, so the limit bounds
    /// the pool's footprint. A job that does not fit is refused with
    /// [`JobPoolError::MemoryLimit`], and the caller does the work on its
    /// own thread.
    ///
    /// The default is [`DEFAULT_MEMORY_LIMIT`].
    pub memory_limit: u64,
}

impl Default for JobPoolConfig {
    fn default() -> Self {
        Self {
            workers: default_worker_count(),
            queue_limit: 64,
            memory_limit: DEFAULT_MEMORY_LIMIT,
        }
    }
}

/// The default byte limit of the job pool.
///
/// The limit bounds the bytes the queued and the in-flight jobs hold, so it
/// bounds the pool's footprint.
///
/// The default is 256 MiB.
pub const DEFAULT_MEMORY_LIMIT: u64 = 256 * 1024 * 1024;

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
pub enum JobPoolError {
    /// A worker thread could not be started.
    #[error("could not start a job worker: {message}")]
    Worker {
        /// The platform's message.
        message: String,
    },
    /// The configuration is not usable.
    #[error("the job pool configuration is invalid: {message}")]
    Config {
        /// What is wrong with the configuration.
        message: String,
    },
    /// The queue is full.
    #[error("the job pool queue holds its {limit} jobs")]
    QueueFull {
        /// The configured queue limit.
        limit: usize,
    },
    /// The memory limit would be exceeded.
    #[error(
        "the memory limit of {limit} bytes ({} MiB) would be exceeded",
        *limit / (1024u64 * 1024)
    )]
    MemoryLimit {
        /// The configured limit.
        limit: u64,
    },
    /// The job's byte accounting does not fit.
    #[error("the job's size does not fit in the pool's accounting")]
    SizeOverflow,
    /// The pool has no more job identities to hand out.
    #[error("the job pool has no more job identities")]
    IdExhausted,
}

/// One queued job.
struct Job {
    /// The job's identity.
    id: JobId,
    /// The handle the result is published into.
    pending: PendingJob,
    /// The work the worker performs.
    work: Arc<JobWork>,
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
    /// The number of jobs the workers ran.
    jobs: u64,
    /// The accumulated time the workers spent running jobs.
    job_time: Duration,
}

/// The queue and the wake-up the workers and the frame thread share.
struct PoolShared {
    /// The pool's only lock.
    state: Mutex<PoolState>,
    /// Signalled when a job is queued or the pool stops.
    wake: Condvar,
}

/// The pool that runs CPU-bound jobs off the frame thread.
///
/// The pool owns its worker threads, its queue and its byte accounting. It
/// is middle-layer state: the engine owns it, the composition root starts
/// it, and the callers that submit work reach it through the engine.
///
/// A job nobody waits for is *normal*; a job a caller waits for is
/// *demanded*. A worker takes a demanded job first, so a caller's work is
/// never delayed behind a prefetch backlog.
pub struct JobPool {
    /// The queue and the byte accounting the workers and the frame thread
    /// share.
    shared: Arc<PoolShared>,
    /// The worker threads, joined when the pool drops.
    workers: Vec<JoinHandle<()>>,
    /// The configuration the pool was started with.
    config: JobPoolConfig,
    /// The identity of the next submitted job.
    next_id: u64,
}

impl JobPool {
    /// Starts the pool.
    ///
    /// # Errors
    ///
    /// Returns [`JobPoolError::Config`] when the configuration asks for no
    /// worker or no queue, and [`JobPoolError::Worker`] when a thread
    /// cannot be started.
    pub fn new(config: JobPoolConfig) -> Result<Self, JobPoolError> {
        if config.workers == 0 {
            return Err(JobPoolError::Config {
                message: "the pool needs at least one worker".to_string(),
            });
        }
        if config.queue_limit == 0 {
            return Err(JobPoolError::Config {
                message: "the pool needs a queue of at least one job".to_string(),
            });
        }
        let shared = Arc::new(PoolShared {
            state: Mutex::new(PoolState {
                queue: VecDeque::new(),
                bytes: 0,
                stopping: false,
                jobs: 0,
                job_time: Duration::ZERO,
            }),
            wake: Condvar::new(),
        });
        let mut workers = Vec::with_capacity(config.workers);
        for index in 0..config.workers {
            let worker_shared = Arc::clone(&shared);
            let handle = std::thread::Builder::new()
                .name(format!("krkr-job-{index}"))
                .spawn(move || worker_loop(worker_shared));
            match handle {
                Ok(handle) => workers.push(handle),
                Err(error) => {
                    // The workers that already started must not outlive the
                    // pool: they are stopped and joined before the error
                    // goes out.
                    stop_workers(&shared, workers);
                    return Err(JobPoolError::Worker {
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

    /// Submits a job and returns the handle its result lands in.
    ///
    /// `work` is the CPU-bound work the worker performs, and `charge` is
    /// the number of bytes the job holds against the memory limit.
    ///
    /// # Errors
    ///
    /// Returns [`JobPoolError::QueueFull`] when the queue is full,
    /// [`JobPoolError::MemoryLimit`] when the job would push the pool past
    /// its limit and [`JobPoolError::IdExhausted`] when the pool has no
    /// more job identities. The caller then does the work on its own
    /// thread; the pool never drops a job silently.
    pub fn submit(
        &mut self,
        work: Arc<JobWork>,
        charge: u64,
    ) -> Result<(JobId, PendingJob), JobPoolError> {
        let pending = PendingJob::new();
        let id = JobId(self.next_id);
        let Some(next_id) = self.next_id.checked_add(1) else {
            return Err(JobPoolError::IdExhausted);
        };
        self.next_id = next_id;
        {
            let mut state = lock(&self.shared.state);
            if state.stopping {
                return Err(JobPoolError::Config {
                    message: "the job pool is stopping".to_string(),
                });
            }
            if state.queue.len() >= self.config.queue_limit {
                return Err(JobPoolError::QueueFull {
                    limit: self.config.queue_limit,
                });
            }
            if state.bytes.saturating_add(charge) > self.config.memory_limit {
                return Err(JobPoolError::MemoryLimit {
                    limit: self.config.memory_limit,
                });
            }
            state.bytes = state.bytes.saturating_add(charge);
            state.queue.push_back(Job {
                id,
                pending: pending.clone(),
                work,
                charged: charge,
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
                job.pending.complete(JobOutcome::Cancelled);
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

    /// The job accounting: how many jobs the pool ran and how long they
    /// took.
    ///
    /// The counters cover every job the pool performed, the successful ones
    /// and the failed ones alike.
    pub fn stats(&self) -> JobStats {
        let state = lock(&self.shared.state);
        JobStats {
            jobs: state.jobs,
            elapsed: state.job_time,
        }
    }
}

impl Drop for JobPool {
    fn drop(&mut self) {
        stop_workers(&self.shared, std::mem::take(&mut self.workers));
    }
}

/// Stops the pool's workers and joins them.
///
/// The queued jobs are cancelled, so a waiter on one of them wakes instead
/// of waiting for a job that will never run. A running job is not
/// preempted: the join waits for it, and it publishes its own result.
fn stop_workers(shared: &PoolShared, workers: Vec<JoinHandle<()>>) {
    {
        let mut state = lock(&shared.state);
        state.stopping = true;
        while let Some(job) = state.queue.pop_front() {
            state.bytes = state.bytes.saturating_sub(job.charged);
            job.pending.complete(JobOutcome::Cancelled);
        }
    }
    shared.wake.notify_all();
    for worker in workers {
        if worker.join().is_err() {
            log::warn!("engine: a job worker stopped with a panic");
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

/// Records the job a worker ran: the count and the time it took.
///
/// The counters live in the pool's own state, so a worker updates them under
/// the pool's lock and the thread that owns the pool reads the totals. No
/// counter crosses a thread boundary.
fn record_job(shared: &PoolShared, elapsed: Duration) {
    let mut state = lock(&shared.state);
    state.jobs = state.jobs.saturating_add(1);
    state.job_time = state.job_time.saturating_add(elapsed);
}

/// Publishes a failure for a job whose worker stops before it does.
///
/// The guard holds the handle of the job a worker is running. Its drop
/// publishes a failure when the job has not produced a result, so a waiter
/// wakes with a failure instead of waiting for a worker that is gone.
struct JobGuard {
    /// The handle the result is published into.
    pending: PendingJob,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.pending.complete(JobOutcome::Failed(
            "the job worker stopped before it published a result".to_string(),
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
        return message.clone();
    }
    "the job panicked".to_string()
}

/// The worker loop: take a job, run it outside the lock, then publish the
/// result.
fn worker_loop(shared: Arc<PoolShared>) {
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
        // A job that was cancelled before it ran is dropped without being
        // run.
        if job.pending.is_ready() {
            release_bytes(&shared, job.charged);
            continue;
        }
        let started = Instant::now();
        let outcome = match std::panic::catch_unwind(AssertUnwindSafe(|| (job.work)())) {
            Ok(Ok(value)) => JobOutcome::Ready(value),
            Ok(Err(message)) => JobOutcome::Failed(message),
            Err(payload) => JobOutcome::Failed(panic_message(payload.as_ref())),
        };
        record_job(&shared, started.elapsed());
        release_bytes(&shared, job.charged);
        job.pending.complete(outcome);
    }
}
