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

use std::f64::consts::PI;
use std::ffi::{c_char, c_int, c_void};

use engine::sound::SoundSource;
use tjs::{NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Value};

use crate::native_util::{report_error, set_int, set_void};
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

/// Borrow the payload of a `PhaseVocoder` instance.
fn payload(instance: *mut c_void) -> &'static mut PhaseVocoderInst {
    // SAFETY: the dispatcher guarantees `instance` is the create payload.
    unsafe { &mut *(instance as *mut PhaseVocoderInst) }
}

/// Mirror the payload config into the shared registry.
fn publish(engine: *mut c_void, instance: *mut c_void) {
    let Some(resolved) = super::resolve(engine) else {
        return;
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let config = payload(instance).config;
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
    set_int(out, payload(instance).config.window as i64);
    0
}

extern "C" fn pv_window_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let window = super::value_as_i64(unsafe { &*value });
    // Reference `SetWindow`: only the powers of two 64..=32768 are accepted.
    if !matches!(
        window,
        64 | 128 | 256 | 512 | 1024 | 2048 | 4096 | 8192 | 16384 | 32768
    ) {
        return report_error(out_error, "Invalid window size must be in 64 to 32768");
    }
    payload(instance).config.window = window as usize;
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
    set_int(out, i64::from(payload(instance).config.overlap));
    0
}

extern "C" fn pv_overlap_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let overlap = super::value_as_i64(unsafe { &*value });
    // Reference `SetOverlap`: 0 (auto) or one of 2, 4, 8, 16, 32.
    if !matches!(overlap, 0 | 2 | 4 | 8 | 16 | 32) {
        return report_error(out_error, "Invalid overlap count must be in 2 to 32");
    }
    payload(instance).config.overlap = overlap as u32;
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
    super::set_real(out, f64::from(payload(instance).config.pitch));
    0
}

extern "C" fn pv_pitch_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    payload(instance).config.pitch = super::value_as_f64(unsafe { &*value }) as f32;
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
    super::set_real(out, f64::from(payload(instance).config.time));
    0
}

extern "C" fn pv_time_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    payload(instance).config.time = super::value_as_f64(unsafe { &*value }) as f32;
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

    /// Render `input` (interleaved) into a scaled interleaved buffer.
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
        let output_hop = self.output_hop.max(2);
        let mut output: Vec<f32> = Vec::new();
        let mut out_frames = 0usize;
        let mut pos = 0usize;

        while pos + frame_size <= input_frames {
            let needed = (out_frames + frame_size) * channels;
            if output.len() < needed {
                output.resize(needed, 0.0);
            }
            // Zero the fresh tail so only completed overlaps are summed.
            let tail_start = out_frames + frame_size - output_hop;
            for frame in tail_start..out_frames + frame_size {
                for ch in 0..channels {
                    output[frame * channels + ch] = 0.0;
                }
            }
            for ch in 0..channels {
                let synth = self.process_channel(ch, input, pos);
                for frame in 0..frame_size {
                    output[(out_frames + frame) * channels + ch] +=
                        synth[frame] * self.output_window[frame];
                }
            }
            out_frames += output_hop;
            pos += self.input_hop;
        }
        output.truncate(out_frames * channels);
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
// Filter-chain application
// ---------------------------------------------------------------------------

/// Render `source` through the `PhaseVocoder` chain, returning a new
/// [`SoundSource`] whose PCM is the scaled output.
///
/// The cumulative time scale is applied to the loop links and labels so the
/// `.sli` loop points keep referring to the same musical position after a
/// time stretch.
pub(super) fn apply_chain(
    source: &SoundSource,
    configs: &[VocoderConfig],
) -> Result<std::sync::Arc<SoundSource>, String> {
    use api::media::{AudioSpec, DecodedAudio};
    use engine::sound::{LoopInfo, LoopLink, SoundLabel};

    let spec = source.spec();
    let sample_rate = spec.sample_rate;
    let channels = usize::from(spec.channels);
    if channels == 0 {
        return Err("source has no channels".to_string());
    }
    let mut samples = source.audio().interleaved.clone();
    let mut time_scale = 1.0f32;
    for config in configs {
        let mut dsp = PhaseVocoderDsp::new(sample_rate, channels, *config);
        samples = dsp.process(&samples);
        if config.time.is_finite() && config.time > 0.0 {
            time_scale *= config.time;
        }
    }
    let frames = samples.len() / channels;
    let new_spec = AudioSpec {
        sample_rate,
        channels: spec.channels,
        total_frames: Some(frames as u64),
    };
    let audio = std::sync::Arc::new(DecodedAudio::new(new_spec, samples));
    let info = source.loops().map(|info| {
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
    });
    SoundSource::with_loops(audio, info)
        .map(std::sync::Arc::new)
        .map_err(|error| error.to_string())
}
