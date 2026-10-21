//! The storage read pool and the pending-read future.
//!
//! Every read runs through a gated fake reader: the reader records that it
//! started and then blocks until the test opens that read's gate. The tests
//! therefore observe *which* read ran and *when*, without a timing
//! assumption.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;

use api::error::FsError;
use engine::storage::{
    FileSystemReader, StorageJob, StorageOutcome, StoragePoolConfig, StorageReadPool, StorageReader,
};
use engine::{Engine, EngineParts, GameFrame, Map, Storage, StorageReadError};
use render::{PipelineConfig, begin_render_state, create_render_pipeline};
use testkit::{build_xp3, headless_window};

mod common;
use common::{mount_memory, mount_memory_with_archives};

/// The bytes a disk file holds in the tests.
const PAYLOAD: &[u8] = b"payload";

/// A latch the test opens to release a read.
struct Gate {
    /// Whether the gate is open.
    open: Mutex<bool>,
    /// Signalled when the gate opens.
    signal: Condvar,
}

impl Gate {
    /// A closed gate.
    fn new() -> Self {
        Self {
            open: Mutex::new(false),
            signal: Condvar::new(),
        }
    }

    /// Waits until the gate is open.
    fn wait(&self) {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        while !*open {
            open = self
                .signal
                .wait(open)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Opens the gate and releases every waiter.
    fn open(&self) {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        *open = true;
        self.signal.notify_all();
    }
}

/// The key a job is known by: the file's path or the in-archive name.
fn job_key(job: &StorageJob) -> String {
    match job {
        StorageJob::Disk { path, .. } => path.display().to_string(),
        StorageJob::Archive { in_arc, .. } => in_arc.clone(),
    }
}

/// A reader whose every call blocks on the gate of its job.
struct GatedReader {
    /// The reader the gated one delegates to.
    inner: FileSystemReader,
    /// The keys that entered the reader, in the order they entered.
    started: Mutex<Vec<String>>,
    /// Signalled when a read enters the reader.
    entered: Condvar,
    /// The gate of each key.
    gates: Mutex<Map<String, Arc<Gate>>>,
    /// The keys whose read fails.
    failing: Mutex<Vec<String>>,
}

impl GatedReader {
    /// A reader with no gate yet; every read blocks until one is opened.
    fn new() -> Self {
        Self {
            inner: FileSystemReader,
            started: Mutex::new(Vec::new()),
            entered: Condvar::new(),
            gates: Mutex::new(Map::default()),
            failing: Mutex::new(Vec::new()),
        }
    }

    /// The gate of `key`, created on first use.
    fn gate(&self, key: &str) -> Arc<Gate> {
        let mut gates = self.gates.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            gates
                .entry(key.to_string())
                .or_insert_with(|| Arc::new(Gate::new())),
        )
    }

    /// Makes the read of `key` fail.
    fn fail(&self, key: &str) {
        self.failing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(key.to_string());
    }

    /// Waits until `count` reads have entered the reader.
    fn wait_for_started(&self, count: usize) {
        let mut started = self.started.lock().unwrap_or_else(PoisonError::into_inner);
        while started.len() < count {
            started = self
                .entered
                .wait(started)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The keys that entered the reader so far.
    fn started(&self) -> Vec<String> {
        self.started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl StorageReader for GatedReader {
    fn read(&self, job: &StorageJob) -> Result<Vec<u8>, StorageReadError> {
        let key = job_key(job);
        {
            let mut started = self.started.lock().unwrap_or_else(PoisonError::into_inner);
            started.push(key.clone());
            self.entered.notify_all();
        }
        let gate = self
            .gates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .cloned();
        if let Some(gate) = gate {
            gate.wait();
        }
        if self
            .failing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&key)
        {
            return Err(StorageReadError::Fs(FsError::Io {
                message: format!("{key} is broken"),
            }));
        }
        self.inner.read(job)
    }
}

/// A pool configuration with `workers` workers and a generous limit.
fn config(workers: usize) -> StoragePoolConfig {
    StoragePoolConfig {
        workers,
        queue_limit: 64,
        memory_limit: 1024 * 1024,
    }
}

/// Mounts an in-memory storage with `files` and `archives`, and installs
/// `pool` as its read pool.
fn storage_with_pool(
    pool: &Arc<StorageReadPool>,
    files: &[(&str, &[u8])],
    archives: &[&str],
) -> Storage {
    let mut storage = if archives.is_empty() {
        mount_memory(files)
    } else {
        mount_memory_with_archives(files, archives)
    };
    storage.read_pool = Some(Arc::clone(pool));
    storage
}

/// The optimistic lookup answers the entry's existence and its size, and the
/// read it schedules carries the bytes.
///
/// The lookup does not read the entry on the calling thread: the read runs on
/// a worker, and the handle the lookup answers with carries its result.
#[test]
fn an_optimistic_lookup_answers_the_size_and_schedules_the_read() {
    let reader = Arc::new(GatedReader::new());
    let pool = Arc::new(StorageReadPool::new(reader.clone(), config(1)).expect("pool"));
    let storage = storage_with_pool(&pool, &[("data.bin", PAYLOAD)], &[]);
    let gate = reader.gate("/data.bin");

    let handle = storage
        .lookup("data.bin")
        .expect("the lookup resolves")
        .expect("the entry exists");
    assert_eq!(
        handle.size(),
        Some(PAYLOAD.len() as u64),
        "the reported size"
    );
    reader.wait_for_started(1);
    assert!(
        !handle.is_ready(),
        "the read has not produced its bytes yet"
    );

    gate.open();
    match handle.await_outcome() {
        StorageOutcome::Ready(bytes) => assert_eq!(&(*bytes)[..], PAYLOAD, "the bytes"),
        other => panic!("the read succeeded, got {other:?}"),
    }
}

/// A name that does not resolve is a miss, and a miss is not an error: the
/// lookup answers `None` and the read raises the typed not-found error.
#[test]
fn a_miss_is_a_miss_and_not_a_failure() {
    let reader = Arc::new(GatedReader::new());
    let pool = Arc::new(StorageReadPool::new(reader.clone(), config(1)).expect("pool"));
    let storage = storage_with_pool(&pool, &[("data.bin", PAYLOAD)], &[]);

    assert!(
        storage
            .lookup("missing.bin")
            .expect("the lookup resolves")
            .is_none(),
        "the miss is not an entry"
    );
    match storage.read("missing.bin") {
        Err(StorageReadError::NotFound(name)) => assert_eq!(name, "missing.bin"),
        other => panic!("the read reports the miss, got {other:?}"),
    }
    assert!(
        reader.started().is_empty(),
        "a miss schedules no read: {:?}",
        reader.started()
    );
}

/// The data access waits for the read the lookup scheduled: the bytes are the
/// ones the worker produced.
#[test]
fn the_read_waits_for_the_scheduled_read() {
    let reader = Arc::new(GatedReader::new());
    let pool = Arc::new(StorageReadPool::new(reader.clone(), config(1)).expect("pool"));
    let storage = storage_with_pool(&pool, &[("data.bin", PAYLOAD)], &[]);
    let gate = reader.gate("/data.bin");
    let handle = storage
        .lookup("data.bin")
        .expect("the lookup resolves")
        .expect("the entry exists");
    reader.wait_for_started(1);

    // The wait ends only when the read produced its bytes, so it cannot
    // finish while the gate is closed.
    let finished = Arc::new(Mutex::new(false));
    let waiter = {
        let finished = Arc::clone(&finished);
        thread::spawn(move || {
            handle.await_outcome();
            *finished.lock().unwrap_or_else(PoisonError::into_inner) = true;
        })
    };
    assert!(
        !*finished.lock().unwrap_or_else(PoisonError::into_inner),
        "the wait returned before the read finished"
    );
    gate.open();
    waiter.join().expect("the waiter finished");
    assert!(
        *finished.lock().unwrap_or_else(PoisonError::into_inner),
        "the wait returned after the read finished"
    );

    // The read the script makes finds the scheduled read and serves its
    // bytes.
    assert_eq!(
        storage.read("data.bin").expect("the read succeeds"),
        PAYLOAD.to_vec()
    );
    assert_eq!(reader.started().len(), 1, "the entry was read once");
}

/// A read that fails on the worker is the storage's own typed error, never a
/// miss and never an empty result.
#[test]
fn a_backend_failure_is_surfaced_on_the_data_access() {
    let reader = Arc::new(GatedReader::new());
    reader.fail("/data.bin");
    let pool = Arc::new(StorageReadPool::new(reader.clone(), config(1)).expect("pool"));
    let storage = storage_with_pool(&pool, &[("data.bin", PAYLOAD)], &[]);

    match storage.read("data.bin") {
        Err(StorageReadError::Fs(FsError::Io { message })) => {
            assert_eq!(message, "/data.bin is broken", "the backend's own message");
        }
        other => panic!("the read failed as expected, got {other:?}"),
    }
}

/// The mount order decides which archive serves an entry: the archive mounted
/// last wins, in the scheduled read as well.
#[test]
fn the_later_mount_wins_the_scheduled_read() {
    let data = build_xp3(&[("shared.txt", b"from-data", false)]);
    let patch = build_xp3(&[("shared.txt", b"from-patch", false)]);
    let reader = Arc::new(GatedReader::new());
    let pool = Arc::new(StorageReadPool::new(reader.clone(), config(1)).expect("pool"));
    let storage = storage_with_pool(
        &pool,
        &[
            ("data.xp3", data.as_slice()),
            ("patch.xp3", patch.as_slice()),
        ],
        &["data.xp3", "patch.xp3"],
    );

    assert_eq!(
        storage.read("shared.txt").expect("the read succeeds"),
        b"from-patch".to_vec(),
        "the archive mounted last serves the entry"
    );
}

/// The frame drains the finished reads: the read the previous frame
/// scheduled is published before the next frame's script runs.
#[test]
fn the_frame_drains_the_finished_reads() {
    let reader = Arc::new(GatedReader::new());
    let mut engine = headless_engine();
    let pool = engine
        .install_storage_reader(reader.clone(), config(1))
        .expect("the pool starts");
    let storage = storage_with_pool(&pool, &[("data.bin", PAYLOAD)], &[]);
    let gate = reader.gate("/data.bin");

    let handle = storage
        .lookup("data.bin")
        .expect("the lookup resolves")
        .expect("the entry exists");
    reader.wait_for_started(1);
    assert_eq!(pool.pending_count(), 1, "the read is in flight");
    gate.open();
    handle.await_outcome();
    assert_eq!(
        pool.pending_count(),
        1,
        "the finished read waits for the frame to publish it"
    );

    engine.step(16.0, &mut NoFrame).expect("the frame ran");
    assert_eq!(
        pool.pending_count(),
        0,
        "the frame published the finished read"
    );

    // The published read still serves the bytes, and the entry is not read a
    // second time.
    assert_eq!(
        storage.read("data.bin").expect("the read succeeds"),
        PAYLOAD.to_vec()
    );
    assert_eq!(reader.started().len(), 1, "the entry was read once");
}

/// The hook of a frame that runs nothing.
struct NoFrame;

impl GameFrame for NoFrame {
    fn before_render(&mut self, _now_ms: u64, _dt_ms: f64) -> Result<(), String> {
        Ok(())
    }
}

/// Builds the bottom objects of a headless engine.
fn headless_parts() -> EngineParts {
    let pipeline = create_render_pipeline(PipelineConfig::null()).expect("null pipeline");
    let handle = headless_window();
    let (surface, render_state) = begin_render_state(&pipeline, &handle).expect("surface");
    EngineParts {
        pipeline,
        surface,
        render_state,
        surface_size: (64, 64),
        headless: true,
        overlay: None,
    }
}

/// Builds a headless engine over the null backend.
fn headless_engine() -> Engine {
    Engine::new(headless_parts()).expect("engine")
}
