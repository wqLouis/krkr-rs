//! The image decode pool and the pending-image future.
//!
//! Every decode runs through a gated fake decoder: the decoder records that
//! it started and then blocks until the test opens that decode's gate. The
//! tests therefore observe *which* decode ran and *when*, without a timing
//! assumption.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use api::media::{DecodedImage, ImageInfo};
use engine::imagedecode::{
    DecodePool, DecodePoolConfig, DecodePoolError, ImageDecodeError, ImageDecoder, ImageOutcome,
    ImageSizeError, ImageState, JobId, PendingImage,
};
use engine::{DecodedImageKey, DecodedImageMode, Engine, EngineParts, GameFrame, Map, Scene};
use render::{PipelineConfig, begin_render_state, create_render_pipeline};
use testkit::headless_window;

/// The pixels the fake decoder produces.
const PIXELS: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

/// The bytes one full-resolution character part charges the pool.
///
/// The character art is a 1445x3304 WebP, so its RGBA pixels are
/// 19 112 720 bytes; with the compressed bytes the measured charge of one
/// part is about 19.6 MB.
const CHARACTER_PART_CHARGE: u64 = 19_600_000;

/// The compressed bytes of one character part, as the pool charges them.
const CHARACTER_PART_BYTES: &[u8] = b"character-part";

/// A latch the test opens to release a decode.
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

/// A set of gates that opens when the test leaves.
///
/// A decode worker waits for its gate, so a test that fails while a worker
/// is inside the decoder must open the gates on the way out; otherwise the
/// pool's drop waits for a worker that never returns.
struct GateSet {
    /// The gates to open.
    gates: Vec<Arc<Gate>>,
}

impl GateSet {
    /// Opens every gate.
    fn open(&self) {
        for gate in &self.gates {
            gate.open();
        }
    }
}

impl Drop for GateSet {
    fn drop(&mut self) {
        self.open();
    }
}

/// A decoder whose every call blocks on the gate of its hint.
struct GatedDecoder {
    /// The hints that entered the decoder, in the order they entered.
    started: Mutex<Vec<String>>,
    /// Signalled when a decode enters the decoder.
    entered: Condvar,
    /// The gate of each hint.
    gates: Mutex<Map<String, Arc<Gate>>>,
    /// The hints whose decode fails.
    failing: Mutex<Vec<String>>,
}

impl GatedDecoder {
    /// A decoder with no gate yet; every decode blocks until one is opened.
    fn new() -> Self {
        Self {
            started: Mutex::new(Vec::new()),
            entered: Condvar::new(),
            gates: Mutex::new(Map::default()),
            failing: Mutex::new(Vec::new()),
        }
    }

    /// The gate of `hint`, created on first use.
    fn gate(&self, hint: &str) -> Arc<Gate> {
        let mut gates = self.gates.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            gates
                .entry(hint.to_string())
                .or_insert_with(|| Arc::new(Gate::new())),
        )
    }

    /// Makes the decode of `hint` fail.
    fn fail(&self, hint: &str) {
        self.failing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(hint.to_string());
    }

    /// Waits until `count` decodes have entered the decoder.
    fn wait_for_started(&self, count: usize) {
        let mut started = self.started.lock().unwrap_or_else(PoisonError::into_inner);
        while started.len() < count {
            started = self
                .entered
                .wait(started)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The hints that entered the decoder so far.
    fn started(&self) -> Vec<String> {
        self.started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ImageDecoder for GatedDecoder {
    fn probe(&self, _bytes: &[u8], _hint: Option<&str>) -> Result<ImageInfo, ImageDecodeError> {
        Ok(ImageInfo {
            width: 2,
            height: 2,
            has_alpha: true,
            format: Some("test".to_string()),
        })
    }

    fn decode(&self, _bytes: &[u8], hint: Option<&str>) -> Result<DecodedImage, ImageDecodeError> {
        let name = hint.unwrap_or("?").to_string();
        {
            let mut started = self.started.lock().unwrap_or_else(PoisonError::into_inner);
            started.push(name.clone());
            self.entered.notify_all();
        }
        let gate = self
            .gates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&name)
            .cloned();
        if let Some(gate) = gate {
            gate.wait();
        }
        if self
            .failing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&name)
        {
            return Err(ImageDecodeError::Failed {
                message: format!("{name} is broken"),
            });
        }
        Ok(DecodedImage::new(2, 2, PIXELS.to_vec()))
    }
}

/// A pool configuration with `workers` workers and a generous limit.
fn config(workers: usize) -> DecodePoolConfig {
    DecodePoolConfig {
        workers,
        queue_limit: 64,
        memory_limit: 1024 * 1024,
    }
}

/// Submits a decode of `hint` and returns its job id and future.
fn submit(pool: &mut DecodePool, hint: &str) -> (JobId, PendingImage) {
    pool.submit(
        Arc::from(b"payload".as_slice()),
        Some(hint.to_string()),
        PIXELS.len() as u64,
        None,
    )
    .expect("the job is accepted")
}

/// Submits a decode charged like one full-resolution character part.
///
/// A job is charged the size of its bytes plus the size of the pixels it
/// will produce, so the measured charge of one part is split between the
/// two.
fn submit_character_part(
    pool: &mut DecodePool,
    hint: &str,
) -> Result<(JobId, PendingImage), DecodePoolError> {
    let pixels =
        CHARACTER_PART_CHARGE - u64::try_from(CHARACTER_PART_BYTES.len()).expect("the length fits");
    pool.submit(
        Arc::from(CHARACTER_PART_BYTES),
        Some(hint.to_string()),
        pixels,
        None,
    )
}

/// A batch of decodes runs at the same time: the pool overlaps them instead
/// of running them one after another.
#[test]
fn a_batch_of_decodes_runs_in_parallel() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(4)).expect("pool");
    let mut futures = Vec::new();
    for index in 0..4 {
        let hint = format!("image{index}");
        let gate = decoder.gate(&hint);
        let (_, pending) = submit(&mut pool, &hint);
        futures.push((pending, gate));
    }
    // All four decodes are inside the decoder at once: the fourth one can
    // only be there when the three others are still waiting for their gate.
    decoder.wait_for_started(4);
    for (_, gate) in &futures {
        gate.open();
    }
    for (pending, _) in &futures {
        assert!(matches!(pending.await_ready(), ImageOutcome::Ready(_)));
    }
}

/// The default limit holds the four character parts of an avatar switch at
/// once.
///
/// A part that does not fit is refused, and the load then decodes on the
/// calling thread: the whole decode time lands on the frame that loads the
/// art. The default limit holds four parts of the measured charge, so the
/// fourth part of an avatar switch is not refused.
#[test]
fn the_default_limit_holds_four_character_parts() {
    let config = DecodePoolConfig::default();
    assert!(
        config.memory_limit >= 4 * CHARACTER_PART_CHARGE,
        "the default limit of {} bytes holds four character parts of {CHARACTER_PART_CHARGE} bytes",
        config.memory_limit
    );
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config).expect("pool");
    // The gates open when the test leaves, so a refused part cannot leave a
    // worker waiting for a gate that never opens.
    let mut gates = GateSet { gates: Vec::new() };
    let mut submitted = Vec::new();
    for index in 0..4 {
        let hint = format!("part{index}");
        gates.gates.push(decoder.gate(&hint));
        submitted.push(submit_character_part(&mut pool, &hint));
    }
    for (index, result) in submitted.iter().enumerate() {
        assert!(
            result.is_ok(),
            "the character part {index} is accepted: {result:?}"
        );
    }
    assert_eq!(
        pool.held_bytes(),
        4 * CHARACTER_PART_CHARGE,
        "the pool holds the four character parts at once"
    );
    // A request that does not fit the limit is still refused.
    let refused = pool.submit(
        Arc::from(CHARACTER_PART_BYTES),
        Some("huge".to_string()),
        config.memory_limit,
        None,
    );
    match refused {
        Err(DecodePoolError::MemoryLimit { limit }) => {
            assert_eq!(limit, config.memory_limit, "the refusal names the limit");
        }
        other => panic!("the oversized job is refused, got {other:?}"),
    }
    gates.open();
}

/// The four character parts of an avatar switch decode at the same time.
///
/// The pool holds them all, so the avatar switch decodes them concurrently
/// instead of refusing the fourth and decoding it on the calling thread.
#[test]
fn four_character_parts_decode_at_the_same_time() {
    let decoder = Arc::new(GatedDecoder::new());
    let config = DecodePoolConfig {
        workers: 4,
        ..DecodePoolConfig::default()
    };
    let mut pool = DecodePool::new(decoder.clone(), config).expect("pool");
    // The gates open when the test leaves, so a refused part cannot leave a
    // worker waiting for a gate that never opens.
    let mut gates = GateSet { gates: Vec::new() };
    let mut submitted = Vec::new();
    for index in 0..4 {
        let hint = format!("part{index}");
        gates.gates.push(decoder.gate(&hint));
        submitted.push(submit_character_part(&mut pool, &hint));
    }
    for (index, result) in submitted.iter().enumerate() {
        assert!(
            result.is_ok(),
            "the character part {index} is accepted: {result:?}"
        );
    }
    // All four decodes are inside the decoder at once: the fourth one can
    // only be there when the three others are still waiting for their gate.
    decoder.wait_for_started(4);
    gates.open();
    for (index, result) in submitted.into_iter().enumerate() {
        let (_, pending) = match result {
            Ok(submitted) => submitted,
            Err(error) => panic!("the character part {index} is accepted: {error}"),
        };
        assert!(matches!(pending.await_ready(), ImageOutcome::Ready(_)));
    }
}

/// The first access waits for the decode: it cannot finish while the gate is
/// closed.
#[test]
fn the_first_access_waits_for_the_decode() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let gate = decoder.gate("slow");
    let (_, pending) = submit(&mut pool, "slow");
    decoder.wait_for_started(1);

    let mut scene = Scene::default();
    let id = scene
        .add_pending_bitmap(2, 2, pending.clone())
        .expect("the pending bitmap");
    let finished = Arc::new(Mutex::new(false));
    let worker = {
        let finished = Arc::clone(&finished);
        thread::spawn(move || {
            let mut scene = scene;
            scene.resolve_bitmap(id);
            *finished.lock().unwrap_or_else(PoisonError::into_inner) = true;
        })
    };
    // The decode is still blocked, so the resolve cannot have returned.
    assert!(
        !*finished.lock().unwrap_or_else(PoisonError::into_inner),
        "the resolve returned before the decode finished"
    );
    gate.open();
    worker.join().expect("the resolve thread finished");
    assert!(
        *finished.lock().unwrap_or_else(PoisonError::into_inner),
        "the resolve returned after the decode finished"
    );
}

/// An image that is already decoded resolves without waiting.
#[test]
fn a_ready_image_resolves_without_waiting() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let gate = decoder.gate("ready");
    let (_, pending) = submit(&mut pool, "ready");
    decoder.wait_for_started(1);
    gate.open();
    assert!(matches!(pending.await_ready(), ImageOutcome::Ready(_)));

    let mut scene = Scene::default();
    let id = scene
        .add_pending_bitmap(2, 2, pending)
        .expect("the pending bitmap");
    assert!(scene.resolve_bitmap(id));
    let bitmap = scene.bitmap(id).expect("the bitmap");
    assert_eq!(bitmap.rgba, PIXELS.to_vec(), "the decoded pixels landed");
    assert!(bitmap.pending.is_none(), "the pending image is consumed");
}

/// A decode that fails installs a blank transparent image of the probed size
/// and reports the failure; no exception reaches the caller.
#[test]
fn a_failed_decode_installs_a_blank_and_reports_the_failure() {
    let decoder = Arc::new(GatedDecoder::new());
    decoder.fail("broken");
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let gate = decoder.gate("broken");
    let (_, pending) = submit(&mut pool, "broken");
    decoder.wait_for_started(1);
    gate.open();

    match pending.await_ready() {
        ImageOutcome::Failed(message) => {
            assert_eq!(message, "broken is broken", "the codec's own message");
        }
        other => panic!("the decode failed as expected, got {other:?}"),
    }
    match pending.state() {
        ImageState::Done(ImageOutcome::Failed(_)) => {}
        other => panic!("the state holds the failure, got {other:?}"),
    }

    let mut scene = Scene::default();
    let id = scene
        .add_pending_bitmap(3, 2, pending)
        .expect("the pending bitmap");
    assert!(scene.resolve_bitmap(id));
    let bitmap = scene.bitmap(id).expect("the bitmap");
    assert_eq!(
        bitmap.rgba,
        vec![0u8; 3 * 2 * 4],
        "the blank keeps the probed size and is transparent"
    );
    assert_eq!((bitmap.width, bitmap.height), (3, 2), "the probed size");
}

/// A demanded job runs before the normal jobs queued behind it.
#[test]
fn a_demanded_job_runs_before_the_queued_one() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(2)).expect("pool");
    let first = decoder.gate("first");
    let second = decoder.gate("second");
    let third = decoder.gate("third");
    let fourth = decoder.gate("fourth");
    let (_, first_pending) = submit(&mut pool, "first");
    let (_, second_pending) = submit(&mut pool, "second");
    decoder.wait_for_started(2);
    let (_, third_pending) = submit(&mut pool, "third");
    let (_, fourth_pending) = submit(&mut pool, "fourth");

    // The third job is demanded; the fourth is not.
    third_pending.demand();
    first.open();
    decoder.wait_for_started(3);
    let started = decoder.started();
    assert!(
        started.contains(&"third".to_string()),
        "the demanded job ran: {started:?}"
    );
    assert!(
        !started.contains(&"fourth".to_string()),
        "the normal job behind it did not run: {started:?}"
    );

    // Every gate is opened: "third" and "fourth" are still decoding, and a
    // decode that never leaves the codec would hold the await below forever.
    second.open();
    third.open();
    fourth.open();
    for pending in [first_pending, second_pending, third_pending, fourth_pending] {
        assert!(matches!(pending.await_ready(), ImageOutcome::Ready(_)));
    }
}

/// A cancelled job wakes its waiter with the cancellation.
#[test]
fn a_cancelled_job_wakes_its_waiter() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let running_gate = decoder.gate("running");
    let (_, running) = submit(&mut pool, "running");
    decoder.wait_for_started(1);
    let (queued_id, queued) = submit(&mut pool, "queued");

    assert!(pool.cancel(queued_id), "the queued job was cancelled");
    assert!(matches!(queued.await_ready(), ImageOutcome::Cancelled));

    // The running job is not preempted; it finishes normally.
    running_gate.open();
    assert!(matches!(running.await_ready(), ImageOutcome::Ready(_)));
}

/// Cancelling the future itself stops the worker from publishing a result.
#[test]
fn cancelling_the_future_discards_the_result() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let gate = decoder.gate("dropped");
    let (_, pending) = submit(&mut pool, "dropped");
    decoder.wait_for_started(1);

    assert!(pending.cancel(), "the image was still waiting");
    assert!(matches!(pending.await_ready(), ImageOutcome::Cancelled));
    gate.open();
}

/// Replacing the image of a bitmap cancels the decode that would have
/// filled it.
#[test]
fn replacing_the_image_cancels_the_pending_decode() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let gate = decoder.gate("replaced");
    let (_, pending) = submit(&mut pool, "replaced");
    decoder.wait_for_started(1);

    let mut scene = Scene::default();
    let id = scene
        .add_pending_bitmap(2, 2, pending.clone())
        .expect("the pending bitmap");
    assert!(scene.replace_bitmap_image(id, 1, 1, vec![7u8; 4]));
    assert!(
        matches!(pending.await_ready(), ImageOutcome::Cancelled),
        "the replaced image cancelled the decode"
    );
    gate.open();
    let bitmap = scene.bitmap(id).expect("the bitmap");
    assert_eq!(bitmap.rgba, vec![7u8; 4], "the replacement stands");
}

/// The frame barrier resolves the queued image before the upload and the
/// projection, so the frame uploads the decoded pixels and not a blank.
#[test]
fn the_frame_barrier_resolves_before_the_upload() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut engine = headless_engine();
    let decoder_for_pool: Arc<dyn ImageDecoder> = decoder.clone();
    engine
        .install_decoder(decoder_for_pool, config(1))
        .expect("the pool starts");
    let gate = decoder.gate("frame");

    // The decode is submitted before the frame; the hook puts its future into
    // the scene, and the barrier after the hook waits for the decode.
    let (_, pending) = engine
        .decode_pool_mut()
        .expect("the pool is installed")
        .submit(
            Arc::from(b"payload".as_slice()),
            Some("frame".to_string()),
            PIXELS.len() as u64,
            None,
        )
        .expect("the job is accepted");
    let scene_ptr = engine.scene_mut() as *mut Scene;
    let mut hook = QueueImage {
        scene: scene_ptr,
        pending,
        id: 0,
        resolved_at_after_project: false,
        clean_at_after_project: false,
    };
    // The decode is released as soon as it starts, so the barrier does not
    // wait for the test.
    let opener = {
        let decoder = decoder.clone();
        let gate = Arc::clone(&gate);
        thread::spawn(move || {
            decoder.wait_for_started(1);
            gate.open();
        })
    };
    engine.step(16.0, &mut hook).expect("the frame ran");
    opener.join().expect("the opener finished");

    // The upward hook runs after the upload and the projection, so what it
    // sees proves the barrier ran before both.
    assert!(
        hook.resolved_at_after_project,
        "the image was decoded before the upload and the projection"
    );
    assert!(
        hook.clean_at_after_project,
        "the resolved pixels were uploaded in the same frame"
    );
    let scene = engine.scene();
    let bitmap = scene.bitmap(hook.id).expect("the bitmap");
    assert_eq!(bitmap.rgba, PIXELS.to_vec(), "the decoded pixels landed");
}

/// The hook that queues one image into the engine's scene and reports what
/// the frame had done with it by the time the upward hook runs.
struct QueueImage {
    /// The scene the engine owns.
    scene: *mut Scene,
    /// The image the hook queues.
    pending: PendingImage,
    /// The id of the queued bitmap.
    id: u32,
    /// Whether the image was decoded by the time the upward hook ran.
    resolved_at_after_project: bool,
    /// Whether the bitmap was uploaded by the time the upward hook ran.
    clean_at_after_project: bool,
}

impl GameFrame for QueueImage {
    fn before_render(&mut self, _now_ms: u64, _dt_ms: f64) -> Result<(), String> {
        // SAFETY: the engine owns the scene for the whole test and does not
        // move it while the hook runs.
        let scene = unsafe { &mut *self.scene };
        self.id = scene
            .add_pending_bitmap(2, 2, self.pending.clone())
            .expect("the pending bitmap");
        Ok(())
    }

    fn after_project(&mut self, _drawn_layers: &[u32]) -> Result<(), String> {
        // SAFETY: the engine owns the scene for the whole test and does not
        // move it while the hook runs.
        let scene = unsafe { &*self.scene };
        if let Some(bitmap) = scene.bitmap(self.id) {
            self.resolved_at_after_project = bitmap.rgba == PIXELS.to_vec();
            self.clean_at_after_project = !bitmap.dirty;
        }
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

/// Dropping the pool cancels the jobs it never started, so a waiter wakes
/// even while another decode is still running.
#[test]
fn dropping_the_pool_cancels_the_jobs_it_never_started() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let gate = decoder.gate("running");
    let (_, running) = submit(&mut pool, "running");
    decoder.wait_for_started(1);
    let (_, queued) = submit(&mut pool, "queued");

    let dropper = thread::spawn(move || drop(pool));
    // The queued job is cancelled while the running decode is still going.
    assert!(
        matches!(queued.await_ready(), ImageOutcome::Cancelled),
        "the queued job was cancelled when the pool dropped"
    );
    gate.open();
    dropper.join().expect("the pool dropped");
    assert!(matches!(running.await_ready(), ImageOutcome::Ready(_)));
}

/// A probed size whose pixel count is not representable is refused with a
/// typed error instead of wrapping around.
#[test]
fn an_unrepresentable_image_size_is_refused() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let (_, pending) = submit(&mut pool, "huge");
    let mut scene = Scene::default();
    let refused = scene.add_pending_bitmap(u32::MAX, u32::MAX, pending.clone());
    assert!(
        matches!(&refused, Err(ImageSizeError::TooLarge { .. })),
        "the size is refused: {refused:?}"
    );
    assert!(scene.bitmaps.is_empty(), "a refused size creates no bitmap");
}

/// A job whose byte accounting does not fit is refused with a typed error,
/// and the pool keeps working.
#[test]
fn a_job_whose_size_does_not_fit_is_refused() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let refused = pool.submit(
        Arc::from(b"payload".as_slice()),
        Some("huge".to_string()),
        u64::MAX,
        None,
    );
    assert!(
        matches!(&refused, Err(DecodePoolError::SizeOverflow)),
        "the job is refused: {refused:?}"
    );
    // The pool still accepts a job that fits.
    let (_, pending) = submit(&mut pool, "small");
    let gate = decoder.gate("small");
    decoder.wait_for_started(1);
    gate.open();
    assert!(matches!(pending.await_ready(), ImageOutcome::Ready(_)));
}

/// Resolving an image moves the content revision and marks the bitmap
/// dirty, so the frame uploads the pixels it just received.
#[test]
fn resolving_an_image_moves_the_revisions_and_marks_the_bitmap_dirty() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let gate = decoder.gate("revision");
    let (_, pending) = submit(&mut pool, "revision");
    decoder.wait_for_started(1);
    gate.open();

    let mut scene = Scene::default();
    let structure_before = scene.structure_revision();
    let content_before = scene.content_revision();
    let id = scene
        .add_pending_bitmap(2, 2, pending)
        .expect("the pending bitmap");
    assert!(
        scene.structure_revision() > structure_before,
        "a new bitmap is a structural change"
    );
    let content_after_enqueue = scene.content_revision();
    assert_eq!(
        content_after_enqueue, content_before,
        "queuing an image is a structural change, not a content change"
    );
    assert!(scene.resolve_bitmap(id));
    assert!(
        scene.content_revision() > content_after_enqueue,
        "the pixels that landed are a content change"
    );
    assert!(
        scene.structure_revision() > structure_before,
        "the resolved bitmap moves the structure revision too"
    );
    let bitmap = scene.bitmap(id).expect("the bitmap");
    assert!(bitmap.dirty, "the resolved bitmap is uploaded this frame");
}

/// The cache key a load names: the entry the decoded image is stored under.
fn cache_key(name: &str) -> DecodedImageKey {
    DecodedImageKey::new(name, 0, DecodedImageMode::Full, 0, 0)
}

/// A deferred decode that the frame barrier resolves enters the scene's
/// decoded-image cache, so a repeated load of the same image is a cache hit
/// and does not decode the image again.
///
/// The cache stores the decoded image under the key the load named and
/// evicts the least-recently-used entries down to the limit
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1952-1960`).
#[test]
fn a_resolved_deferred_decode_enters_the_image_cache() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut engine = headless_engine();
    engine
        .install_decoder(decoder.clone(), config(1))
        .expect("the pool starts");
    let gate = decoder.gate("cached");
    let key = cache_key("cached.png");
    // The load submits the decode under the key the cache stores it under.
    let (_, pending) = engine
        .decode_pool_mut()
        .expect("the pool is installed")
        .submit_cached(
            key.clone(),
            Arc::from(b"payload".as_slice()),
            Some("cached".to_string()),
            PIXELS.len() as u64,
            None,
        )
        .expect("the job is accepted");
    // The cache is enabled, so the resolved image is kept.
    engine.scene_mut().set_graphic_cache_limit(1024 * 1024);
    // Nothing is cached before the decode produced its pixels.
    assert!(
        engine.scene_mut().lookup_decoded_image(&key).is_none(),
        "the cache holds nothing before the decode finished"
    );
    let scene_ptr = engine.scene_mut() as *mut Scene;
    let mut hook = QueueCachedImage {
        scene: scene_ptr,
        pending: Some(pending),
        key: key.clone(),
        hits: 0,
    };
    // The decode is released as soon as it starts, so the barrier does not
    // wait for the test.
    let opener = {
        let decoder = decoder.clone();
        let gate = Arc::clone(&gate);
        thread::spawn(move || {
            decoder.wait_for_started(1);
            gate.open();
        })
    };
    engine.step(16.0, &mut hook).expect("the frame ran");
    opener.join().expect("the opener finished");
    assert_eq!(
        decoder.started().len(),
        1,
        "the frame decoded the image once"
    );

    // The next frame loads the same image again: the cache answers, so the
    // image is not decoded a second time.
    engine.step(16.0, &mut hook).expect("the second frame ran");
    assert_eq!(hook.hits, 1, "the repeated load was a cache hit");
    assert_eq!(
        decoder.started().len(),
        1,
        "the repeated load did not decode the image again"
    );
    let cached = engine
        .scene_mut()
        .lookup_decoded_image(&key)
        .expect("the cache holds the image");
    assert_eq!(
        cached.rgba,
        PIXELS.to_vec(),
        "the cached pixels are the decoded ones"
    );
}

/// The hook that queues one deferred decode into the engine's scene and, on
/// the next frame, loads the same image again.
struct QueueCachedImage {
    /// The scene the engine owns.
    scene: *mut Scene,
    /// The image the hook queues on the first frame.
    pending: Option<PendingImage>,
    /// The cache key the load uses.
    key: DecodedImageKey,
    /// The number of cache hits the repeated load saw.
    hits: usize,
}

impl GameFrame for QueueCachedImage {
    fn before_render(&mut self, _now_ms: u64, _dt_ms: f64) -> Result<(), String> {
        // SAFETY: the engine owns the scene for the whole test and does not
        // move it while the hook runs.
        let scene = unsafe { &mut *self.scene };
        match self.pending.take() {
            Some(pending) => {
                scene
                    .add_pending_bitmap(2, 2, pending)
                    .map_err(|error| error.to_string())?;
            }
            None => {
                if scene.lookup_decoded_image(&self.key).is_some() {
                    self.hits += 1;
                }
            }
        }
        Ok(())
    }
}

/// A job's pass runs on the worker that decoded the image, and the pending
/// image publishes its output rather than the raw decode.
///
/// The reference applies a load's colour key to the decoded scanlines before
/// the image is installed
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1068-1074`), so the
/// installation never sees the unkeyed pixels.
#[test]
fn a_job_pass_runs_on_the_worker_and_publishes_its_output() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let gate = decoder.gate("keyed");
    let pass_thread = Arc::new(Mutex::new(None));
    let pass: Arc<dyn Fn(DecodedImage) -> Result<DecodedImage, String> + Send + Sync> = {
        let pass_thread = Arc::clone(&pass_thread);
        Arc::new(move |image: DecodedImage| {
            *pass_thread.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(thread::current().id());
            let mut rgba = image.rgba;
            for pixel in rgba.as_chunks_mut::<4>().0 {
                pixel[3] = 0;
            }
            Ok(DecodedImage::new(image.width, image.height, rgba))
        })
    };
    let (_, pending) = pool
        .submit(
            Arc::from(b"payload".as_slice()),
            Some("keyed".to_string()),
            PIXELS.len() as u64,
            Some(pass),
        )
        .expect("the job is accepted");
    decoder.wait_for_started(1);
    gate.open();
    let image = match pending.await_ready() {
        ImageOutcome::Ready(image) => image,
        other => panic!("the decode succeeded, got {other:?}"),
    };
    let ran_on = pass_thread
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .expect("the pass ran");
    assert_ne!(
        ran_on,
        thread::current().id(),
        "the pass ran on the decode worker"
    );
    assert!(
        image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 0),
        "the published pixels are the pass output"
    );
    assert_eq!(&image.rgba[..3], &PIXELS[..3], "the pass kept the colours");
}

/// A pass that refuses the pixels fails the job; the raw decode is never
/// published.
///
/// The reference runs a load's pass over the decoded image before the image
/// is used (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1068-1074`), so
/// a refusal is the load's failure and never its raw pixels.
#[test]
fn a_failing_pass_fails_the_job() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let gate = decoder.gate("bad-key");
    let pass: Arc<dyn Fn(DecodedImage) -> Result<DecodedImage, String> + Send + Sync> =
        Arc::new(|_image: DecodedImage| Err("the colour key is unusable".to_string()));
    let (_, pending) = pool
        .submit(
            Arc::from(b"payload".as_slice()),
            Some("bad-key".to_string()),
            PIXELS.len() as u64,
            Some(pass),
        )
        .expect("the job is accepted");
    decoder.wait_for_started(1);
    gate.open();
    match pending.await_ready() {
        ImageOutcome::Failed(message) => {
            assert_eq!(message, "the colour key is unusable", "the pass's message");
        }
        other => panic!("the pass failed as expected, got {other:?}"),
    }
}

/// The image a pass produced is what the pending resolution installs and
/// caches: a deferred colour-keyed load leaves the keyed image under the load
/// key, not the raw decode.
///
/// The reference stores the graphic in the cache after the load's own pass
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1952-1960`).
#[test]
fn a_resolved_pass_is_the_cached_image() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let gate = decoder.gate("cached-key");
    let key = DecodedImageKey::new("cached-key.png", 0x01_0203, DecodedImageMode::Full, 0, 0);
    let pass: Arc<dyn Fn(DecodedImage) -> Result<DecodedImage, String> + Send + Sync> =
        Arc::new(|image: DecodedImage| {
            let mut rgba = image.rgba;
            for pixel in rgba.as_chunks_mut::<4>().0 {
                pixel[3] = 7;
            }
            Ok(DecodedImage::new(image.width, image.height, rgba))
        });
    let (_, pending) = pool
        .submit_cached(
            key.clone(),
            Arc::from(b"payload".as_slice()),
            Some("cached-key".to_string()),
            PIXELS.len() as u64,
            Some(pass),
        )
        .expect("the job is accepted");
    decoder.wait_for_started(1);
    gate.open();

    let mut scene = Scene::default();
    scene.set_graphic_cache_limit(1024 * 1024);
    let id = scene
        .add_pending_bitmap(2, 2, pending)
        .expect("the pending bitmap");
    assert!(scene.resolve_bitmap(id));
    let bitmap = scene.bitmap(id).expect("the bitmap");
    assert!(
        bitmap
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 7),
        "the bitmap holds the pass output"
    );
    let cached = scene
        .lookup_decoded_image(&key)
        .expect("the cache holds the resolved image");
    assert!(
        cached
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 7),
        "the cache holds the pass output, not the raw decode"
    );
}

/// A deferred decode counts once in the pool's accounting, and resolving the
/// image again does not count a second decode.
#[test]
fn a_deferred_decode_counts_once_in_the_pool_accounting() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    assert_eq!(pool.stats().decodes, 0, "no decode ran yet");
    let gate = decoder.gate("counted");
    let (_, pending) = pool
        .submit_cached(
            cache_key("counted.png"),
            Arc::from(b"payload".as_slice()),
            Some("counted".to_string()),
            PIXELS.len() as u64,
            None,
        )
        .expect("the job is accepted");
    decoder.wait_for_started(1);
    gate.open();
    assert!(matches!(pending.await_ready(), ImageOutcome::Ready(_)));
    let stats = pool.stats();
    assert_eq!(stats.decodes, 1, "the deferred decode counted once");
    assert!(
        stats.elapsed > Duration::ZERO,
        "the decode time was accumulated"
    );

    // Resolving the image installs the pixels; it does not decode again.
    let mut scene = Scene::default();
    let id = scene
        .add_pending_bitmap(2, 2, pending)
        .expect("the pending bitmap");
    assert!(scene.resolve_bitmap(id));
    assert!(scene.resolve_bitmap(id));
    assert_eq!(
        pool.stats().decodes,
        1,
        "the image was decoded once, however often it resolved"
    );
}

/// The scene finds the decode that is already running for a cache key, so a
/// second load of the same image shares it instead of decoding the bytes a
/// second time.
///
/// The reference consults the decoded-image cache before it loads and stores
/// the decoded image under the same key
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1952-1960`), so the
/// second load of one image is served from the first one's result.
#[test]
fn the_scene_finds_the_decode_running_for_a_key() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let key = cache_key("shared.png");
    let (_, pending) = pool
        .submit_cached(
            key.clone(),
            Arc::from(b"payload".as_slice()),
            Some("shared".to_string()),
            PIXELS.len() as u64,
            None,
        )
        .expect("the job is accepted");
    let mut scene = Scene::default();
    let id = scene
        .add_pending_bitmap(2, 2, pending.clone())
        .expect("the pending bitmap");

    // The running decode is found under the key the load named.
    let (width, height, found) = scene
        .pending_decode_for(&key)
        .expect("the running decode is found");
    assert_eq!((width, height), (2, 2), "the probed size comes with it");
    assert!(
        found.same_decode(&pending),
        "the found future is the running decode"
    );

    // A key no decode fills is not found.
    assert!(
        scene.pending_decode_for(&cache_key("other.png")).is_none(),
        "another key has no running decode"
    );

    // A bitmap that holds no pending decode answers nothing.
    scene.remove_bitmap(id);
    assert!(
        scene.pending_decode_for(&key).is_none(),
        "the removed bitmap carries no decode"
    );
}

/// A decode can be shared: two bitmaps carry the same future and the pixels
/// land in both. Removing one of them leaves the decode alive, so the other
/// bitmap still gets the pixels.
#[test]
fn removing_one_holder_of_a_shared_decode_keeps_the_decode() {
    let decoder = Arc::new(GatedDecoder::new());
    let mut pool = DecodePool::new(decoder.clone(), config(1)).expect("pool");
    let key = cache_key("shared.png");
    let (_, pending) = pool
        .submit_cached(
            key.clone(),
            Arc::from(b"payload".as_slice()),
            Some("shared".to_string()),
            PIXELS.len() as u64,
            None,
        )
        .expect("the job is accepted");
    let mut scene = Scene::default();
    let first = scene
        .add_pending_bitmap(2, 2, pending.clone())
        .expect("the first bitmap");
    let second = scene
        .add_pending_bitmap(2, 2, pending.clone())
        .expect("the second bitmap");

    // The first holder goes away while the second one waits for the decode.
    assert!(scene.remove_bitmap(first));
    assert!(scene.resolve_bitmap(second), "the second bitmap resolves");
    let bitmap = scene.bitmap(second).expect("the bitmap");
    assert_eq!(
        bitmap.rgba,
        PIXELS.to_vec(),
        "the shared decode landed in the bitmap that stayed"
    );
}
