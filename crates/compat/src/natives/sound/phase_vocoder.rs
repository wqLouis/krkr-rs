//! `WaveSoundBuffer.PhaseVocoder` — the pitch/time-scaling DSP filter class.
//!
//! Behavioral reference: `tTJSNC_PhaseVocoder` / `tTJSNI_PhaseVocoder`
//! (`reference/cpp/core/sound/PhaseVocoderFilter.cpp`) and the DSP itself
//! (`reference/cpp/core/sound/PhaseVocoderDSP.cpp`). The class is registered
//! as a static member of `WaveSoundBuffer` in the reference
//! (`ScriptMgnIntf.cpp`), so scripts write `new WaveSoundBuffer.PhaseVocoder()`
//! and add it to `WaveSoundBuffer.filters`.
//!
//! # Surface
//!
//! Defaults match the reference constructor: `window = 4096`, `overlap = 0`,
//! `pitch = 1.0`, `time = 1.0`. `window` accepts the powers of two
//! `64..=32768`; `overlap` accepts `0, 2, 4, 8, 16, 32`; any other value is a
//! typed error with the reference's message. `interface` returns an opaque
//! integer token (the reference returns the `iTVPBasicWaveFilter*` pointer as
//! an integer).
//!
//! # DSP application
//!
//! [`PhaseVocoderDsp`] is a faithful port of the reference analysis /
//! frequency-resampling / synthesis pipeline (Vorbis-I window, Ooura-style
//! unnormalised real FFT, per-bin phase propagation, overlap-add). It is
//! applied by [`super::sound_play`] to the decoded source through the
//! `WaveSoundBuffer.filters` chain, so `pitch` and `time` actually alter the
//! rendered audio rather than only round-tripping.
#![allow(unsafe_code)]

use std::f64::consts::PI;
use std::ffi::{c_char, c_int, c_void};
use std::sync::Arc;

use api::media::AudioSpec;
use engine::sound::{AudioStream, LoopInfo, LoopLink, SoundError, SoundLabel, SoundSource};
use tjs::{NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Value};

use crate::native_util::{report_error, set_int, set_void, value_ref};
use crate::{CompatCtx, CompatError};

/// A validated `PhaseVocoder` configuration, mirrored into
/// [`super::SoundState`] so the `filters` chain can resolve an `interface`
/// token to its DSP parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct VocoderConfig {
    /// FFT frame size (power of two, `64..=32768`).
    pub window: usize,
    /// Overlap/oversampling factor (`0` = auto, else `2, 4, 8, 16, 32`).
    pub overlap: u32,
    /// Frequency (pitch) scale.
    pub pitch: f32,
    /// Time scale.
    pub time: f32,
}

impl Default for VocoderConfig {
    fn default() -> Self {
        Self {
            window: 4096,
            overlap: 0,
            pitch: 1.0,
            time: 1.0,
        }
    }
}

/// Payload of one `WaveSoundBuffer.PhaseVocoder` filter object.
struct PhaseVocoderInst {
    config: VocoderConfig,
}

extern "C" fn pv_create(engine: *mut c_void) -> *mut c_void {
    let instance = Box::into_raw(Box::new(PhaseVocoderInst {
        config: VocoderConfig::default(),
    })) as *mut c_void;
    // Publish the config so `sound_play` can resolve the `interface` token.
    if let Some(resolved) = super::resolve(engine) {
        let ctx = resolved.ctx();
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        let services = unsafe { ctx.services() };
        services
            .state::<super::SoundState>()
            .vocoders
            .insert(instance as usize, VocoderConfig::default());
    }
    instance
}

extern "C" fn pv_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    if let Some(resolved) = super::resolve(engine) {
        let ctx = resolved.ctx();
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        let services = unsafe { ctx.services() };
        services
            .state::<super::SoundState>()
            .vocoders
            .remove(&(instance as usize));
    }
    // SAFETY: `instance` came from `pv_create` and is released once.
    drop(unsafe { Box::from_raw(instance as *mut PhaseVocoderInst) });
}

/// Run `f` with the payload of a `PhaseVocoder` instance.
fn with_payload<R>(instance: *mut c_void, f: impl FnOnce(&mut PhaseVocoderInst) -> R) -> R {
    // SAFETY: the dispatcher guarantees `instance` is the create payload.
    let payload = unsafe { &mut *(instance as *mut PhaseVocoderInst) };
    f(payload)
}

/// Mirror the payload config into the shared registry.
fn publish(engine: *mut c_void, instance: *mut c_void) {
    let Some(resolved) = super::resolve(engine) else {
        return;
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let config = with_payload(instance, |payload| payload.config);
    services
        .state::<super::SoundState>()
        .vocoders
        .insert(instance as usize, config);
}

/// `interface` — the opaque filter token the `filters` chain resolves. The
/// reference returns the `iTVPBasicWaveFilter*` as an integer; the instance
/// address is the port's analogue.
extern "C" fn pv_interface_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, instance as usize as i64);
    0
}

extern "C" fn pv_window_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(
        out,
        with_payload(instance, |payload| payload.config.window as i64),
    );
    0
}

extern "C" fn pv_window_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let window = super::value_as_i64(unsafe { &*value });
    // Reference `SetWindow`: only the powers of two 64..=32768 are accepted.
    if !matches!(
        window,
        64 | 128 | 256 | 512 | 1024 | 2048 | 4096 | 8192 | 16384 | 32768
    ) {
        return report_error(out_error, "Invalid window size must be in 64 to 32768");
    }
    with_payload(instance, |payload| payload.config.window = window as usize);
    publish(engine, instance);
    0
}

extern "C" fn pv_overlap_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(
        out,
        i64::from(with_payload(instance, |payload| payload.config.overlap)),
    );
    0
}

extern "C" fn pv_overlap_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let overlap = super::value_as_i64(unsafe { &*value });
    // Reference `SetOverlap`: 0 (auto) or one of 2, 4, 8, 16, 32.
    if !matches!(overlap, 0 | 2 | 4 | 8 | 16 | 32) {
        return report_error(out_error, "Invalid overlap count must be in 2 to 32");
    }
    with_payload(instance, |payload| payload.config.overlap = overlap as u32);
    publish(engine, instance);
    0
}

extern "C" fn pv_pitch_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    super::set_real(
        out,
        f64::from(with_payload(instance, |payload| payload.config.pitch)),
    );
    0
}

extern "C" fn pv_pitch_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_payload(instance, |payload| {
        payload.config.pitch = super::value_as_f64(value_ref(value)) as f32;
    });
    publish(engine, instance);
    0
}

extern "C" fn pv_time_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    super::set_real(
        out,
        f64::from(with_payload(instance, |payload| payload.config.time)),
    );
    0
}

extern "C" fn pv_time_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_payload(instance, |payload| {
        payload.config.time = super::value_as_f64(value_ref(value)) as f32;
    });
    publish(engine, instance);
    0
}

extern "C" fn pv_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

/// Register the hidden `__TvpPhaseVocoder` class.
///
/// The reference attaches the class to the `WaveSoundBuffer` class object as
/// `PhaseVocoder` (`ScriptMgnIntf.cpp`); [`super::register`] performs that
/// aliasing after this call so scripts use `new WaveSoundBuffer.PhaseVocoder()`
/// while the global namespace stays clean.
pub(super) fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "__TvpPhaseVocoder",
        create: pv_create,
        destroy: pv_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "__TvpPhaseVocoder",
            f: pv_ctor,
        }],
        properties: vec![
            NativeInstancePropertyDef {
                name: "interface",
                get: Some(pv_interface_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "window",
                get: Some(pv_window_get),
                set: Some(pv_window_set),
            },
            NativeInstancePropertyDef {
                name: "overlap",
                get: Some(pv_overlap_get),
                set: Some(pv_overlap_set),
            },
            NativeInstancePropertyDef {
                name: "pitch",
                get: Some(pv_pitch_get),
                set: Some(pv_pitch_set),
            },
            NativeInstancePropertyDef {
                name: "time",
                get: Some(pv_time_get),
                set: Some(pv_time_set),
            },
        ],
    })
}

// ---------------------------------------------------------------------------
// DSP
// ---------------------------------------------------------------------------

/// An offline phase vocoder: [`Self::process`] renders one decoded buffer into
/// a time/pitch-scaled buffer.
///
/// The pipeline mirrors `tRisaPhaseVocoderDSP::Process` / `ProcessCore`:
/// windowed analysis frames at `input_hop`, a per-bin phase-difference
/// frequency estimate, optional frequency-axis resampling, phase
/// accumulation at `output_hop`, and overlap-add synthesis with the Vorbis-I
/// window. The FFT is an unnormalised iterative radix-2 complex transform
/// (matching the Ooura `rdft` scaling the reference assumes).
pub(super) struct PhaseVocoderDsp {
    frame_size: usize,
    channels: usize,
    sample_rate: f32,
    over_sampling: usize,
    input_hop: usize,
    output_hop: usize,
    time_scale: f32,
    frequency_scale: f32,
    rebuild: bool,
    input_window: Vec<f32>,
    output_window: Vec<f32>,
    over_sampling_radian: f32,
    over_sampling_radian_recp: f32,
    frequency_per_filter_band: f32,
    frequency_per_filter_band_recp: f32,
    exact_time_scale: f32,
    last_anal_phase: Vec<Vec<f32>>,
    last_synth_phase: Vec<Vec<f32>>,
}

impl PhaseVocoderDsp {
    /// Build a DSP for `config` over `sample_rate`/`channels`.
    pub(super) fn new(sample_rate: u32, channels: usize, config: VocoderConfig) -> Self {
        let frame_size = config.window.max(16);
        let n2 = frame_size / 2;
        let mut dsp = Self {
            frame_size,
            channels,
            sample_rate: sample_rate as f32,
            over_sampling: 8,
            input_hop: frame_size / 8,
            output_hop: frame_size / 8,
            time_scale: 1.0,
            frequency_scale: 1.0,
            rebuild: true,
            input_window: vec![0.0; frame_size],
            output_window: vec![0.0; frame_size],
            over_sampling_radian: 0.0,
            over_sampling_radian_recp: 0.0,
            frequency_per_filter_band: 0.0,
            frequency_per_filter_band_recp: 0.0,
            exact_time_scale: 1.0,
            last_anal_phase: vec![vec![0.0; n2]; channels],
            last_synth_phase: vec![vec![0.0; n2]; channels],
        };
        // The reference `Decode` sets the scales then the oversampling.
        dsp.set_frequency_scale(config.pitch);
        dsp.set_time_scale(config.time);
        dsp.set_over_sampling(config.overlap);
        dsp
    }

    /// FFT frame size in samples.
    pub(super) fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// Input frames consumed by one [`Self::process_hop`].
    pub(super) fn input_hop(&self) -> usize {
        self.input_hop
    }

    /// Output frames committed by one [`Self::process_hop`].
    pub(super) fn output_hop(&self) -> usize {
        self.output_hop
    }

    fn set_time_scale(&mut self, value: f32) {
        let value = if value.is_finite() { value } else { 1.0 };
        if self.time_scale != value {
            self.time_scale = value;
            self.rebuild = true;
            self.input_hop = self.frame_size / self.over_sampling;
            self.output_hop = ((self.input_hop as f32 * value) as usize) & !1;
        }
    }

    fn set_frequency_scale(&mut self, value: f32) {
        let value = if value.is_finite() { value } else { 1.0 };
        if self.frequency_scale != value {
            self.frequency_scale = value;
            self.rebuild = true;
        }
    }

    fn set_over_sampling(&mut self, value: u32) {
        let mut value = value as usize;
        if value == 0 {
            // Reference thresholds (empirical).
            value = if self.time_scale <= 0.2 {
                2
            } else if self.time_scale <= 1.2 {
                4
            } else {
                8
            };
        }
        if self.over_sampling != value {
            self.over_sampling = value.max(2);
            self.input_hop = self.frame_size / self.over_sampling;
            self.output_hop = ((self.input_hop as f32 * self.time_scale) as usize) & !1;
            self.rebuild = true;
        }
    }

    fn rebuild_params(&mut self) {
        // Vorbis-I window, plus the reference's output gain.
        let recovery = 2.0f32;
        let output_volume = self.time_scale
            / self.frame_size as f32
            / self.frequency_scale.abs().max(f32::MIN_POSITIVE).sqrt()
            / self.over_sampling as f32
            * 2.0
            * recovery;
        for i in 0..self.frame_size {
            let x = (i as f64 + 0.5) / self.frame_size as f64;
            let window = (PI / 2.0 * (PI * x).sin().powi(2)).sin() as f32;
            self.input_window[i] = window;
            self.output_window[i] = window * output_volume;
        }
        self.over_sampling_radian = (2.0 * std::f32::consts::PI) / self.over_sampling as f32;
        self.over_sampling_radian_recp = 1.0 / self.over_sampling_radian;
        self.frequency_per_filter_band = self.sample_rate / self.frame_size as f32;
        self.frequency_per_filter_band_recp = if self.frequency_per_filter_band != 0.0 {
            1.0 / self.frequency_per_filter_band
        } else {
            0.0
        };
        self.exact_time_scale = self.output_hop as f32 / self.input_hop as f32;
        self.rebuild = false;
    }

    /// Append one analysis/synthesis hop to `output`.
    ///
    /// `input` must hold at least [`Self::frame_size`] frames. The hop's
    /// window starts at frame `write_frame` of the output timeline, whose
    /// first frame is `front_frame` (the committed prefix
    /// `[front_frame, write_frame)` has already been consumed by the caller).
    /// The overlapping tail of the previous hop is preserved and summed into,
    /// so the caller can drain the committed prefix and keep only a bounded
    /// window.
    pub(super) fn process_hop(
        &mut self,
        input: &[f32],
        output: &mut Vec<f32>,
        write_frame: u64,
        front_frame: u64,
    ) {
        if self.rebuild {
            self.rebuild_params();
        }
        let channels = self.channels;
        let frame_size = self.frame_size;
        let offset = (write_frame - front_frame) as usize;
        let needed = (offset + frame_size) * channels;
        if output.len() < needed {
            output.resize(needed, 0.0);
        }
        for ch in 0..channels {
            let synth = self.process_channel(ch, input, 0);
            for frame in 0..frame_size {
                output[(offset + frame) * channels + ch] +=
                    synth[frame] * self.output_window[frame];
            }
        }
    }

    /// Render `input` (interleaved) into a scaled interleaved buffer.
    #[cfg(test)]
    pub(super) fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.rebuild {
            self.rebuild_params();
        }
        let channels = self.channels;
        let frame_size = self.frame_size;
        let input_frames = input.len() / channels.max(1);
        if frame_size == 0 || self.input_hop == 0 {
            return input.to_vec();
        }
        let input_hop = self.input_hop;
        let output_hop = self.output_hop.max(2);
        let steps = if input_frames >= frame_size {
            (input_frames - frame_size) / input_hop + 1
        } else {
            0
        };
        let mut output: Vec<f32> = Vec::new();
        for step in 0..steps {
            let start = step * input_hop * channels;
            self.process_hop(&input[start..], &mut output, (step * output_hop) as u64, 0);
        }
        output.truncate(steps * output_hop * channels);
        output
    }

    /// Analyse + synthesise one channel's frame, returning the un-windowed
    /// synthesis frame.
    fn process_channel(&mut self, ch: usize, input: &[f32], pos: usize) -> Vec<f32> {
        let n = self.frame_size;
        let n2 = n / 2;
        let channels = self.channels;
        let mut re = vec![0.0f32; n];
        let mut im = vec![0.0f32; n];
        for i in 0..n {
            re[i] = input[(pos + i) * channels + ch] * self.input_window[i];
        }
        fft(&mut re, &mut im, false);

        let mut mag = vec![0.0f32; n2];
        // `freq[i]` holds the true frequency (pitch-shift branch); `dev[i]`
        // holds the phase deviation to accumulate (pitch-1 branch).
        let mut freq = vec![0.0f32; n2];
        let mut dev = vec![0.0f32; n2];
        for i in 0..n2 {
            let (bin_re, bin_im) = if i == 0 { (re[0], 0.0) } else { (re[i], im[i]) };
            let m = (bin_re * bin_re + bin_im * bin_im).sqrt();
            let ang = bin_im.atan2(bin_re);
            let mut tmp = self.last_anal_phase[ch][i] - ang;
            self.last_anal_phase[ch][i] = ang;
            tmp -= i as f32 * self.over_sampling_radian;
            tmp = wrap_pi(tmp);
            if self.frequency_scale != 1.0 {
                tmp *= self.over_sampling_radian_recp;
                freq[i] = (i as f32 + tmp) * self.frequency_per_filter_band;
            } else {
                tmp += i as f32 * self.over_sampling_radian;
                tmp *= self.exact_time_scale;
                dev[i] = tmp;
            }
            mag[i] = m;
        }

        let mut sre = vec![0.0f32; n];
        let mut sim = vec![0.0f32; n];
        if self.frequency_scale != 1.0 {
            // Frequency-axis resampling with bilinear interpolation.
            let scale_rcp = 1.0f32 / self.frequency_scale;
            for i in 0..n2 {
                let fi = i as f32 * scale_rcp;
                let index = fi as usize;
                let frac = fi - index as f32;
                let (sm, sf) = if index + 1 < n2 {
                    (
                        mag[index] + frac * (mag[index + 1] - mag[index]),
                        self.frequency_scale
                            * (freq[index] + frac * (freq[index + 1] - freq[index])),
                    )
                } else if index < n2 {
                    (mag[index], freq[index] * self.frequency_scale)
                } else {
                    (0.0, 0.0)
                };
                let mut tmp = sf * self.frequency_per_filter_band_recp - i as f32;
                tmp *= self.over_sampling_radian;
                tmp += i as f32 * self.over_sampling_radian;
                tmp *= self.exact_time_scale;
                self.last_synth_phase[ch][i] -= tmp;
                let ang = self.last_synth_phase[ch][i];
                let (sin, cos) = ang.sin_cos();
                sre[i] = sm * cos;
                sim[i] = sm * sin;
            }
        } else {
            for i in 0..n2 {
                self.last_synth_phase[ch][i] -= dev[i];
                let ang = self.last_synth_phase[ch][i];
                let (sin, cos) = ang.sin_cos();
                sre[i] = mag[i] * cos;
                sim[i] = mag[i] * sin;
            }
        }
        // The reference discards the Nyquist bin; complete the Hermitian
        // spectrum so the inverse transform is real.
        sre[n2] = 0.0;
        sim[n2] = 0.0;
        for k in n2 + 1..n {
            sre[k] = sre[n - k];
            sim[k] = -sim[n - k];
        }
        fft(&mut sre, &mut sim, true);
        sre
    }
}

/// In-place iterative radix-2 FFT. `inverse` selects the sign; neither
/// direction is normalised, matching the reference's `rdft` usage.
fn fft(re: &mut [f32], im: &mut [f32], inverse: bool) {
    let n = re.len();
    if n <= 1 || n & (n - 1) != 0 {
        return;
    }
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2usize;
    while len <= n {
        // The reference's `rdft` uses the opposite imaginary sign to the
        // textbook forward transform, so the forward pass here is `e^{+j}`
        // (and the inverse `e^{-j}`); the phase math below relies on it.
        let angle = 2.0 * std::f32::consts::PI / len as f32 * if inverse { -1.0 } else { 1.0 };
        let (sin, cos) = angle.sin_cos();
        let (wr, wi) = (cos, sin);
        let mut start = 0usize;
        while start < n {
            let mut cur_re = 1.0f32;
            let mut cur_im = 0.0f32;
            for k in 0..len / 2 {
                let u_re = re[start + k];
                let u_im = im[start + k];
                let v_re = re[start + k + len / 2] * cur_re - im[start + k + len / 2] * cur_im;
                let v_im = re[start + k + len / 2] * cur_im + im[start + k + len / 2] * cur_re;
                re[start + k] = u_re + v_re;
                im[start + k] = u_im + v_im;
                re[start + k + len / 2] = u_re - v_re;
                im[start + k + len / 2] = u_im - v_im;
                let next_re = cur_re * wr - cur_im * wi;
                let next_im = cur_re * wi + cur_im * wr;
                cur_re = next_re;
                cur_im = next_im;
            }
            start += len;
        }
        len <<= 1;
    }
}

/// Wrap a radian value into `[-π, π]` (reference `WrapPi_F1`).
fn wrap_pi(value: f32) -> f32 {
    let unit = (value * (1.0 / std::f32::consts::PI)) as i32;
    let unit = if unit >= 0 {
        unit + (unit & 1)
    } else {
        unit - (unit & 1)
    };
    value - (std::f32::consts::PI * unit as f32)
}

// ---------------------------------------------------------------------------
// Streaming filter chain
// ---------------------------------------------------------------------------

/// One `PhaseVocoder` stage fed and drained as the mixer pulls frames.
///
/// `input` and `output` are interleaved queues whose front is the next frame.
/// `output` holds the committed prefix `[front_frame, write_frame)` plus the
/// uncommitted overlap tail, so at most a `frame_size` window of either queue
/// is ever retained — the reference's ring buffers
/// (`reference/cpp/core/sound/PhaseVocoderDSP.cpp:239-292`).
struct VocoderStage {
    dsp: PhaseVocoderDsp,
    channels: usize,
    input: Vec<f32>,
    output: Vec<f32>,
    front_frame: u64,
    write_frame: u64,
    total_output: u64,
}

impl VocoderStage {
    /// Build a stage whose input is `input_total` frames long.
    fn new(sample_rate: u32, channels: usize, config: VocoderConfig, input_total: u64) -> Self {
        let dsp = PhaseVocoderDsp::new(sample_rate, channels, config);
        let frame_size = dsp.frame_size() as u64;
        let input_hop = dsp.input_hop().max(1) as u64;
        let output_hop = dsp.output_hop().max(2) as u64;
        // One `Process` step per complete `frame_size` window, advancing by
        // `input_hop` and committing `output_hop`
        // (`reference/cpp/core/sound/PhaseVocoderDSP.cpp:336-426`).
        let steps = if input_total >= frame_size {
            (input_total - frame_size) / input_hop + 1
        } else {
            0
        };
        Self {
            dsp,
            channels,
            input: Vec::new(),
            output: Vec::new(),
            front_frame: 0,
            write_frame: 0,
            total_output: steps * output_hop,
        }
    }

    /// Frames buffered and not yet consumed by the DSP.
    fn input_frames(&self) -> u64 {
        (self.input.len() / self.channels) as u64
    }

    /// Committed output frames ready to hand downstream.
    fn ready_frames(&self) -> u64 {
        self.write_frame - self.front_frame
    }

    /// Append interleaved input frames.
    fn push_input(&mut self, samples: &[f32]) {
        self.input.extend_from_slice(samples);
    }

    /// Run one analysis/synthesis hop: consume `input_hop` frames and commit
    /// `output_hop` frames.
    fn step(&mut self) {
        self.dsp.process_hop(
            &self.input,
            &mut self.output,
            self.write_frame,
            self.front_frame,
        );
        let hop = self.dsp.input_hop();
        self.input.drain(..hop * self.channels);
        self.write_frame += self.dsp.output_hop().max(2) as u64;
    }

    /// Copy up to `frames` committed frames into `out`, dropping them.
    fn take(&mut self, frames: usize, out: &mut [f32]) -> usize {
        let n = frames.min(self.ready_frames() as usize);
        let end = n * self.channels;
        out[..end].copy_from_slice(&self.output[..end]);
        self.output.drain(..end);
        self.front_frame += n as u64;
        n
    }
}

/// Build one stage per config, each fed by the previous stage's output.
fn build_stages(
    sample_rate: u32,
    channels: usize,
    configs: &[VocoderConfig],
    input_total: u64,
) -> Vec<VocoderStage> {
    let mut stages = Vec::new();
    let mut total = input_total;
    for config in configs {
        let stage = VocoderStage::new(sample_rate, channels, *config, total);
        total = stage.total_output;
        stages.push(stage);
    }
    stages
}

/// A phase-vocoder chain decoded on demand as the mixer pulls frames.
///
/// This mirrors the reference filter stack: the base decoder/loop manager is
/// wrapped by each filter in `filters` order
/// (`reference/cpp/core/sound/WaveIntf.cpp:920-980`), and each filter pulls
/// only the hop it needs
/// (`reference/cpp/core/sound/PhaseVocoderFilter.cpp:277-370`) instead of
/// materializing the file. Nothing beyond a `frame_size` window per stage is
/// retained.
pub(super) struct FilterChainStream {
    base: Arc<SoundSource>,
    base_frames: u64,
    base_cursor: u64,
    sample_rate: u32,
    channels: u32,
    configs: Vec<VocoderConfig>,
    stages: Vec<VocoderStage>,
    total_output: u64,
}

impl FilterChainStream {
    /// Wrap `base` with the `configs` chain. A base whose length the container
    /// did not report is materialized once so the output frame count is known;
    /// every other base stays incremental.
    pub(super) fn new(
        base: Arc<SoundSource>,
        configs: Vec<VocoderConfig>,
    ) -> Result<Self, SoundError> {
        let sample_rate = base.sample_rate();
        let channels = base.channels();
        if channels == 0 {
            return Err(SoundError::InvalidSpec {
                message: "source has no channels".to_string(),
            });
        }
        let stage_channels = usize::try_from(channels).map_err(|_| SoundError::InvalidSpec {
            message: format!("channel count {channels} does not fit in memory"),
        })?;
        let (base, base_frames) = match base.reported_frames() {
            Some(frames) => (base, frames),
            None => {
                let audio = base.decode_entire()?;
                let frames = audio.frame_count();
                let materialized = Arc::new(SoundSource::with_loops(
                    Arc::new(audio),
                    base.loops().cloned(),
                )?);
                (materialized, frames)
            }
        };
        let stages = build_stages(sample_rate, stage_channels, &configs, base_frames);
        let total_output = stages
            .last()
            .map_or(base_frames, |stage| stage.total_output);
        Ok(Self {
            base,
            base_frames,
            base_cursor: 0,
            sample_rate,
            channels,
            configs,
            stages,
            total_output,
        })
    }

    /// Total output frames the chain will produce.
    pub(super) fn total_output(&self) -> u64 {
        self.total_output
    }

    /// The interleaved channel count as a `usize` for buffer arithmetic.
    fn channels_usize(&self) -> Result<usize, SoundError> {
        usize::try_from(self.channels).map_err(|_| SoundError::InvalidSpec {
            message: format!("channel count {} does not fit in memory", self.channels),
        })
    }

    /// Produce at least one committed output frame at `index`, pulling input
    /// from the previous stage as needed. Returns `false` at end of input.
    fn pump(&mut self, index: usize) -> Result<bool, SoundError> {
        if self.stages[index].ready_frames() > 0 {
            return Ok(true);
        }
        let frame_size = self.stages[index].dsp.frame_size() as u64;
        self.fill_input(index, frame_size)?;
        if self.stages[index].input_frames() < frame_size {
            return Ok(false);
        }
        self.stages[index].step();
        Ok(true)
    }

    /// Ensure stage `index` holds `target` input frames, or as many as remain.
    fn fill_input(&mut self, index: usize, target: u64) -> Result<(), SoundError> {
        if self.stages[index].input_frames() >= target {
            return Ok(());
        }
        if index == 0 {
            let want = target - self.stages[0].input_frames();
            let available = self.base_frames.saturating_sub(self.base_cursor);
            let take = want.min(available) as usize;
            if take == 0 {
                return Ok(());
            }
            let channels = self.channels_usize()?;
            let mut buffer = vec![0.0f32; take * channels];
            for frame in 0..take {
                let index = self.base_cursor + frame as u64;
                let Some((left, right)) = self.base.frame(index)? else {
                    return Err(SoundError::Decode {
                        message: format!(
                            "filter chain: base source reported {0} frames but frame {index} is missing",
                            self.base_frames
                        ),
                    });
                };
                buffer[frame * channels] = left;
                if channels >= 2 {
                    buffer[frame * channels + 1] = right;
                }
            }
            self.base_cursor += take as u64;
            self.stages[0].push_input(&buffer);
            return Ok(());
        }
        while self.stages[index].input_frames() < target {
            if self.stages[index - 1].ready_frames() == 0 && !self.pump(index - 1)? {
                break;
            }
            let want = (target - self.stages[index].input_frames()) as usize;
            let available = self.stages[index - 1].ready_frames() as usize;
            let n = want.min(available);
            if n == 0 {
                break;
            }
            let channels = self.channels_usize()?;
            let mut buffer = vec![0.0f32; n * channels];
            self.stages[index - 1].take(n, &mut buffer);
            self.stages[index].push_input(&buffer);
        }
        Ok(())
    }

    /// Rewind every stage and the base to the first frame.
    fn reset(&mut self) -> Result<(), SoundError> {
        self.stages = build_stages(
            self.sample_rate,
            self.channels_usize()?,
            &self.configs,
            self.base_frames,
        );
        self.base_cursor = 0;
        Ok(())
    }
}

impl AudioStream for FilterChainStream {
    fn spec(&self) -> AudioSpec {
        AudioSpec {
            sample_rate: self.sample_rate,
            channels: self.channels,
            total_frames: Some(self.total_output),
        }
    }

    fn read(&mut self, out: &mut [f32]) -> Result<usize, SoundError> {
        let channels = self.channels_usize()?;
        if channels == 0 {
            return Ok(0);
        }
        let frames_wanted = out.len() / channels;
        let mut written = 0;
        while written < frames_wanted {
            let last = self.stages.len() - 1;
            if self.stages[last].ready_frames() == 0 && !self.pump(last)? {
                break;
            }
            let n = (self.stages[last].ready_frames() as usize).min(frames_wanted - written);
            let start = written * channels;
            let end = start + n * channels;
            self.stages[last].take(n, &mut out[start..end]);
            written += n;
        }
        Ok(written * channels)
    }

    fn seek(&mut self, frame: u64) -> Result<(), SoundError> {
        self.reset()?;
        let channels = self.channels_usize()?;
        let mut remaining = frame;
        let mut buffer = vec![0.0f32; 4096 * channels];
        while remaining > 0 {
            let want = remaining.min(4096) as usize;
            let got = self.read(&mut buffer[..want * channels])?;
            if got == 0 {
                break;
            }
            remaining -= (got / channels) as u64;
        }
        Ok(())
    }
}

/// Build a streaming [`SoundSource`] that renders `base` through the
/// `PhaseVocoder` chain.
///
/// The cumulative time scale scales the loop links and labels so the `.sli`
/// loop points keep referring to the same musical position after a time
/// stretch.
pub(super) fn build_filtered_source(
    base: Arc<SoundSource>,
    configs: &[VocoderConfig],
) -> Result<Arc<SoundSource>, SoundError> {
    let mut time_scale = 1.0f32;
    for config in configs {
        if config.time.is_finite() && config.time > 0.0 {
            time_scale *= config.time;
        }
    }
    let stream = FilterChainStream::new(base.clone(), configs.to_vec())?;
    let spec = AudioSpec {
        sample_rate: stream.sample_rate,
        channels: stream.channels,
        total_frames: Some(stream.total_output()),
    };
    let info = scale_loops(base.loops(), time_scale);
    SoundSource::streaming(spec, Box::new(stream), info).map(Arc::new)
}

/// Scale loop links and labels by the chain's cumulative time scale.
fn scale_loops(info: Option<&LoopInfo>, time_scale: f32) -> Option<LoopInfo> {
    info.map(|info| {
        let scale = f64::from(time_scale.max(0.0));
        let links = info
            .links
            .iter()
            .map(|link| {
                LoopLink::new(
                    (link.from as f64 * scale) as u64,
                    (link.to as f64 * scale) as u64,
                    link.smooth,
                    link.condition,
                    link.ref_value,
                    link.cond_var,
                )
            })
            .collect();
        let labels = info
            .labels
            .iter()
            .map(|label| {
                SoundLabel::new((label.position as f64 * scale) as u64, label.name.clone())
            })
            .collect();
        LoopInfo::new(links, labels)
    })
}
