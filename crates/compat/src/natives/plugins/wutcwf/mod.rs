//! `wutcwf` plugin — a full port of `reference/cpp/plugins/wutcwf.cpp`.
//!
//! The reference registers a `tTVPWaveDecoderCreator` with the wave system;
//! the creator opens a storage whose bytes start with the `TCWF0\x1a` mark
//! and decodes the stream with a combined MS-ADPCM / IMA-ADPCM codec. It
//! registers **no** TJS class or function.
//!
//! # Where the decoder lives in krkr-rs
//!
//! L3 never links a codec: `compat` exposes the host decoder seam
//! [`add_audio_decoder`](crate::add_audio_decoder), and the L4 host
//! (`crates/app/src/audio.rs`) composes the L1 `media` decoders behind it.
//! The reference's decoder *creator* maps to that seam: [`register`]
//! installs [`decode_tcwf`] as the sound decoder, so `WaveSoundBuffer.open`
//! decodes a `TCWF0\x1a` stream exactly like the reference's creator does.
//! [`is_tcwf`] is exposed so a future composite host decoder can dispatch on
//! the signature before falling back to the L1 codecs.
//!
//! The decode is a byte-for-byte port of the reference `TCWFDecoder`:
//!
//! * the 24-byte `TTCWFHeader`;
//! * per channel, a 32-byte `TTCWBlockHeader` (`ms_sample0`, `ms_sample1`,
//!   `ms_idelta`, `ms_bpred`, `ima_stepindex` and six unexpected-peak
//!   corrections);
//! * the MS-ADPCM low nibble of every byte followed by the IMA-ADPCM high
//!   nibble of the same byte, as the reference's two `for(k = 2, p = 0; ...)`
//!   loops;
//! * the six unexpected-peak corrections applied last.
//!
//! No ADPCM crate covers this stream: the reference packs the MS-ADPCM low
//! nibble and the IMA-ADPCM high nibble of the *same* byte (two codecs
//! interleaved in one stream) inside a custom 32-byte block header with
//! per-channel peak corrections, so the codec stays a faithful port rather
//! than a crate call.
//!
//! The reference emits signed 16-bit samples; [`DecodedAudio`] stores
//! interleaved `f32`, so every sample is scaled by `1 / 32768`.
#![allow(unsafe_code)]

use api::media::{AudioSpec, DecodedAudio};
use engine::sound::{LoopInfo, SoundSource};
use std::sync::Arc;

use crate::natives::sound::add_audio_decoder;
use crate::{CompatCtx, CompatError};

/// The packed `TTCWFHeader` (`#pragma pack(1)`), little-endian.
const HEADER_SIZE: usize = 24;
/// `sizeof(TTCWBlockHeader)` — `2 + 2 + 2 + 1 + 1 + 6 * (2 + 2)`.
const BLOCK_HEADER_SIZE: usize = 32;
/// Number of unexpected-peak corrections in a block header.
const PEAK_COUNT: usize = 6;
/// The six-byte stream mark (`"TCWF0\x1a"`).
const MARK: [u8; 6] = *b"TCWF0\x1a";
/// Full-scale value of a signed 16-bit sample.
const SAMPLE_SCALE: f32 = 32768.0;

/// MS-ADPCM `AdaptationTable`.
const ADAPTATION_TABLE: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];
/// MS-ADPCM `AdaptCoeff1`.
const ADAPT_COEFF1: [i32; 7] = [256, 512, 0, 192, 240, 460, 392];
/// MS-ADPCM `AdaptCoeff2`.
const ADAPT_COEFF2: [i32; 7] = [0, -256, 0, 64, 0, -208, -232];
/// IMA-ADPCM `ima_index_adjust`.
const IMA_INDEX_ADJUST: [i32; 16] = [
    -1, -1, -1, -1, // +0 - +3, decrease the step size
    2, 4, 6, 8, // +4 - +7, increase the step size
    -1, -1, -1, -1, // -0 - -3, decrease the step size
    2, 4, 6, 8, // -4 - -7, increase the step size
];
/// IMA-ADPCM `ima_step_size` (89 entries).
const IMA_STEP_SIZE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// A typed TCWF decode failure (no silent truncation or silence).
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TcwfError {
    /// The stream does not start with the `TCWF0\x1a` mark.
    #[error("not a TCWF stream: missing the TCWF0\\x1a mark")]
    BadSignature,
    /// The fixed 24-byte header is truncated.
    #[error("truncated TCWF header: {found} of {needed} bytes")]
    TruncatedHeader {
        /// Bytes the header needs.
        needed: usize,
        /// Bytes actually present.
        found: usize,
    },
    /// A stream with zero channels has no samples to produce.
    #[error("TCWF stream has zero channels")]
    ZeroChannels,
    /// The sample rate in the header is not positive.
    #[error("TCWF stream has an invalid sample rate {0} Hz")]
    InvalidSampleRate(i32),
    /// The block count in the header is not positive.
    #[error("TCWF stream has an invalid block count {0}")]
    InvalidBlockCount(i32),
    /// `bytesperblock` / `samplesperblock` cannot describe a block.
    #[error(
        "TCWF stream has an invalid block layout: {bytesperblock} bytes per block for \
         {samplesperblock} samples"
    )]
    InvalidBlockLayout {
        /// Header `bytesperblock`.
        bytesperblock: i32,
        /// Header `samplesperblock`.
        samplesperblock: i32,
    },
    /// A block (or a channel within it) runs past the end of the stream.
    #[error("truncated TCWF block {block} channel {channel}: offset {offset}")]
    TruncatedBlock {
        /// Block index (one per channel row).
        block: usize,
        /// Channel within the block.
        channel: usize,
        /// Byte offset the block starts at.
        offset: usize,
    },
    /// A block's MS-ADPCM predictor is out of the `AdaptCoeff` range.
    #[error("TCWF block {block} channel {channel} has an invalid predictor {predictor}")]
    BadPredictor {
        /// Block index.
        block: usize,
        /// Channel within the block.
        channel: usize,
        /// The rejected predictor.
        predictor: u8,
    },
    /// A block's IMA-ADPCM step index is out of the `ima_step_size` range.
    #[error("TCWF block {block} channel {channel} has an invalid step index {stepindex}")]
    BadStepIndex {
        /// Block index.
        block: usize,
        /// Channel within the block.
        channel: usize,
        /// The rejected step index.
        stepindex: u8,
    },
    /// The header describes more samples than the host can address.
    #[error("TCWF stream describes too many samples for this host")]
    TooManySamples,
    /// An unexpected-peak correction points past the block's samples.
    #[error("TCWF block {block} channel {channel} peak position {position} is out of range")]
    PeakOutOfRange {
        /// Block index.
        block: usize,
        /// Channel within the block.
        channel: usize,
        /// The rejected position.
        position: u16,
    },
}

/// The parsed `TTCWFHeader`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TcwfHeader {
    channels: u8,
    frequency: i32,
    numblocks: i32,
    bytesperblock: i32,
    samplesperblock: i32,
}

impl TcwfHeader {
    /// Parse and validate the fixed 24-byte header.
    fn parse(bytes: &[u8]) -> Result<Self, TcwfError> {
        if bytes.len() < HEADER_SIZE {
            return Err(TcwfError::TruncatedHeader {
                needed: HEADER_SIZE,
                found: bytes.len(),
            });
        }
        if bytes[..MARK.len()] != MARK {
            return Err(TcwfError::BadSignature);
        }
        let header = Self {
            channels: bytes[6],
            frequency: i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            numblocks: i32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            bytesperblock: i32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            samplesperblock: i32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
        };
        if header.channels == 0 {
            return Err(TcwfError::ZeroChannels);
        }
        if header.frequency <= 0 {
            return Err(TcwfError::InvalidSampleRate(header.frequency));
        }
        if header.numblocks <= 0 {
            return Err(TcwfError::InvalidBlockCount(header.numblocks));
        }
        // A block must hold the two seed samples, fit in the block buffer and
        // provide one nibble byte per decoded sample after the seeds.
        let bytes_per_block = header.bytesperblock as usize;
        let samples_per_block = header.samplesperblock as usize;
        if samples_per_block < 2
            || bytes_per_block < BLOCK_HEADER_SIZE
            || bytes_per_block - BLOCK_HEADER_SIZE < samples_per_block - 2
        {
            return Err(TcwfError::InvalidBlockLayout {
                bytesperblock: header.bytesperblock,
                samplesperblock: header.samplesperblock,
            });
        }
        Ok(header)
    }
}

/// `true` when `bytes` starts with the `TCWF0\x1a` mark.
pub fn is_tcwf(bytes: &[u8]) -> bool {
    bytes.len() >= MARK.len() && bytes[..MARK.len()] == MARK
}

/// Decode a whole TCWF stream into interleaved 16-bit PCM stored as `f32`.
///
/// This is the whole-file form of the reference `TCWFDecoder` + `Render`:
/// it decodes every block in the header and produces
/// `numblocks * samplesperblock` frames, channel-interleaved.
pub fn decode_tcwf(bytes: &[u8]) -> Result<DecodedAudio, TcwfError> {
    let header = TcwfHeader::parse(bytes)?;
    let channels = usize::from(header.channels);
    let samples_per_block = header.samplesperblock as usize;
    let bytes_per_block = header.bytesperblock as usize;
    let blocks = header.numblocks as usize;
    let row_bytes = bytes_per_block
        .checked_mul(channels)
        .ok_or(TcwfError::TooManySamples)?;

    // Validate the block count against the bytes actually present *before*
    // sizing the output, so a corrupt header cannot trigger a huge
    // allocation (a typed error, not an abort).
    let available_rows = (bytes.len() - HEADER_SIZE) / row_bytes;
    if blocks > available_rows {
        return Err(TcwfError::TruncatedBlock {
            block: available_rows,
            channel: 0,
            offset: HEADER_SIZE + available_rows * row_bytes,
        });
    }

    let total_frames = blocks
        .checked_mul(samples_per_block)
        .ok_or(TcwfError::TooManySamples)?;
    let total_samples = total_frames
        .checked_mul(channels)
        .ok_or(TcwfError::TooManySamples)?;
    let mut interleaved = Vec::with_capacity(total_samples);

    let mut offset = HEADER_SIZE;
    for block in 0..blocks {
        if offset + row_bytes > bytes.len() {
            return Err(TcwfError::TruncatedBlock {
                block,
                channel: 0,
                offset,
            });
        }
        let mut samples = vec![0i16; samples_per_block * channels];
        for channel in 0..channels {
            let start = offset + channel * bytes_per_block;
            let block_bytes = &bytes[start..start + bytes_per_block];
            decode_block(&header, block_bytes, block, channel, &mut samples)?;
        }
        for frame in 0..samples_per_block {
            let base = frame * channels;
            for channel in 0..channels {
                interleaved.push(samples[base + channel] as f32 / SAMPLE_SCALE);
            }
        }
        offset += row_bytes;
    }

    Ok(DecodedAudio::new(
        AudioSpec {
            sample_rate: header.frequency as u32,
            channels: u32::from(header.channels),
            total_frames: Some(total_frames as u64),
        },
        interleaved,
    ))
}

/// The six unexpected-peak corrections of one block header.
type Peaks = [(u16, i16); PEAK_COUNT];

/// Decode one channel's block into `samples` (interleaved with `stride`
/// channels). The port of the reference `TCWFDecoder::ReadBlock`.
fn decode_block(
    header: &TcwfHeader,
    block: &[u8],
    block_index: usize,
    channel: usize,
    samples: &mut [i16],
) -> Result<(), TcwfError> {
    let stride = header.channels as usize;
    let samples_per_block = header.samplesperblock as usize;

    debug_assert_eq!(block.len(), header.bytesperblock as usize);
    debug_assert_eq!(samples.len(), samples_per_block * stride);

    let ms_sample0 = i16::from_le_bytes([block[0], block[1]]);
    let ms_sample1 = i16::from_le_bytes([block[2], block[3]]);
    let ms_idelta = i32::from(i16::from_le_bytes([block[4], block[5]]));
    let ms_bpred = block[6];
    let ima_stepindex = i32::from(block[7]);
    let mut peaks: Peaks = [(0, 0); PEAK_COUNT];
    for (index, peak) in peaks.iter_mut().enumerate() {
        let base = 8 + index * 4;
        peak.0 = u16::from_le_bytes([block[base], block[base + 1]]);
        peak.1 = i16::from_le_bytes([block[base + 2], block[base + 3]]);
    }
    if ms_bpred >= 7 {
        return Err(TcwfError::BadPredictor {
            block: block_index,
            channel,
            predictor: ms_bpred,
        });
    }
    if ima_stepindex > 88 {
        return Err(TcwfError::BadStepIndex {
            block: block_index,
            channel,
            stepindex: block[7],
        });
    }

    let data = &block[BLOCK_HEADER_SIZE..];
    samples[channel] = ms_sample0;
    samples[stride + channel] = ms_sample1;

    // MS-ADPCM: the low nibble of each byte.
    let mut idelta = ms_idelta;
    let bpred = ms_bpred as usize;
    for k in 2..samples_per_block {
        let nibble = (data[k - 2] & 0x0F) as usize;
        let idelta_save = idelta;
        idelta = (ADAPTATION_TABLE[nibble] * idelta) >> 8;
        if idelta < 16 {
            idelta = 16;
        }
        let mut bytecode = nibble as i32;
        if bytecode & 0x8 != 0 {
            bytecode -= 0x10;
        }
        let previous1 = i32::from(samples[(k - 1) * stride + channel]);
        let previous2 = i32::from(samples[(k - 2) * stride + channel]);
        let predict = (previous1 * ADAPT_COEFF1[bpred] + previous2 * ADAPT_COEFF2[bpred]) >> 8;
        let current = (bytecode * idelta_save + predict).clamp(-32768, 32767);
        samples[k * stride + channel] = current as i16;
    }

    // IMA-ADPCM: the high nibble of the same bytes, accumulated onto the
    // MS-ADPCM sample.
    let mut stepindex = ima_stepindex;
    let mut previous = 0i32;
    for k in 2..samples_per_block {
        let bytecode = ((data[k - 2] >> 4) & 0x0F) as usize;
        let step = IMA_STEP_SIZE[stepindex as usize];
        let mut current = previous;
        let mut diff = step >> 3;
        if bytecode & 1 != 0 {
            diff += step >> 2;
        }
        if bytecode & 2 != 0 {
            diff += step >> 1;
        }
        if bytecode & 4 != 0 {
            diff += step;
        }
        if bytecode & 8 != 0 {
            diff = -diff;
        }
        current = (current + diff).clamp(-32768, 32767);
        stepindex = (stepindex + IMA_INDEX_ADJUST[bytecode]).clamp(0, 88);
        previous = current;
        let sample = (i32::from(samples[k * stride + channel]) + current).clamp(-32768, 32767);
        samples[k * stride + channel] = sample as i16;
    }

    // Unexpected-peak corrections, applied last.
    for (position, revise) in peaks {
        if revise != 0 {
            if usize::from(position) >= samples_per_block {
                return Err(TcwfError::PeakOutOfRange {
                    block: block_index,
                    channel,
                    position,
                });
            }
            let index = usize::from(position) * stride + channel;
            let sample = (i32::from(samples[index]) - i32::from(revise)).clamp(-32768, 32767);
            samples[index] = sample as i16;
        }
    }

    Ok(())
}

/// Register the plugin's decoder with the sound system.
///
/// `wutcwf.dll` registers a wave-decoder creator with the sound system and
/// no TJS members. The Rust analogue is the [`add_audio_decoder`] seam: the
/// closure adapts [`decode_tcwf`] into the L2 [`SoundSource`] the sound
/// natives expect, exactly as the L4 host adapts the L1 `media` codecs.
///
/// The sound model has a single decoder slot, so the L4 host can install a
/// composite decoder over this one (the boot order installs the host decoder
/// after the compat surface); within `compat` this decoder is what
/// `WaveSoundBuffer.open` uses.
///
/// [`add_audio_decoder`]: crate::add_audio_decoder
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    add_audio_decoder(
        services,
        Box::new(
            |bytes: &[u8], loops: Option<LoopInfo>, _hint: Option<&str>| {
                if !is_tcwf(bytes) {
                    return Err("the stream does not carry the TCWF0\\x1a mark".to_string());
                }
                let audio = decode_tcwf(bytes).map_err(|error| error.to_string())?;
                let source = SoundSource::with_loops(Arc::new(audio), loops)
                    .map_err(|error| error.to_string())?;
                Ok(Arc::new(source))
            },
        ),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests: the pure ADPCM codec over synthetic blocks
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a packed TCWF header.
    fn header_bytes(
        channels: u8,
        frequency: i32,
        numblocks: i32,
        bytesperblock: i32,
        samplesperblock: i32,
    ) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_SIZE);
        bytes.extend_from_slice(&MARK);
        bytes.push(channels);
        bytes.push(0);
        bytes.extend_from_slice(&frequency.to_le_bytes());
        bytes.extend_from_slice(&numblocks.to_le_bytes());
        bytes.extend_from_slice(&bytesperblock.to_le_bytes());
        bytes.extend_from_slice(&samplesperblock.to_le_bytes());
        bytes
    }

    /// Build a packed block header (all peaks zero) followed by `nibbles`.
    fn block_bytes(
        seed0: i16,
        seed1: i16,
        idelta: i16,
        bpred: u8,
        stepindex: u8,
        nibbles: &[u8],
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&seed0.to_le_bytes());
        bytes.extend_from_slice(&seed1.to_le_bytes());
        bytes.extend_from_slice(&idelta.to_le_bytes());
        bytes.push(bpred);
        bytes.push(stepindex);
        bytes.extend_from_slice(&[0u8; 24]);
        bytes.extend_from_slice(nibbles);
        bytes
    }

    // The six-byte stream mark checked by `TCWFDecoder::Open`
    // (`reference/cpp/plugins/wutcwf.cpp:196-201`) and the packed
    // `TTCWFHeader` (`:13-21`).
    #[test]
    fn mark_detection_accepts_only_the_reference_signature() {
        let mut stream = header_bytes(1, 44100, 1, 34, 4);
        stream.extend(block_bytes(0, 0, 16, 0, 0, &[0, 0]));
        assert!(is_tcwf(&stream));
        assert!(!is_tcwf(b"TCWF0")); // truncated
        assert!(!is_tcwf(b"TCWF1\x1a\x00"));
        assert!(!is_tcwf(b""));
    }

    // Byte-for-byte reproduction of the reference decode loops: the
    // MS-ADPCM pass over the low nibbles (`reference/cpp/plugins/wutcwf.cpp:270-293`)
    // and the IMA-ADPCM pass over the high nibbles of the same bytes
    // (`:299-337`), with the header seeds taken from the 32-byte
    // `TTCWBlockHeader` (`:26-35`).
    #[test]
    fn decoded_samples_match_the_reference_adpcm_formulas() {
        // One mono block, four samples. The seeds bypass the predictors; the
        // following two samples exercise both nibble paths.
        let nibbles = [0x0F, 0x80];
        let stream = {
            let mut stream = header_bytes(1, 22050, 1, BLOCK_HEADER_SIZE as i32 + 2, 4);
            stream.extend(block_bytes(100, 200, 16, 0, 0, &nibbles));
            stream
        };
        let audio = decode_tcwf(&stream).expect("decode synthetic stream");
        assert_eq!(audio.spec.sample_rate, 22050);
        assert_eq!(audio.spec.channels, 1);
        assert_eq!(audio.spec.total_frames, Some(4));
        assert_eq!(audio.interleaved.len(), 4);

        // Sample 0/1 are the seeds.
        assert!((audio.interleaved[0] - 100.0 / SAMPLE_SCALE).abs() < f32::EPSILON);
        assert!((audio.interleaved[1] - 200.0 / SAMPLE_SCALE).abs() < f32::EPSILON);

        // Independently reproduce the reference formulas for the two
        // prediction samples.
        let mut samples = [100i32, 200i32, 0, 0];
        let mut idelta = 16i32;
        for k in 2..4usize {
            let nibble = (nibbles[k - 2] & 0x0F) as usize;
            let idelta_save = idelta;
            idelta = (ADAPTATION_TABLE[nibble] * idelta) >> 8;
            if idelta < 16 {
                idelta = 16;
            }
            let mut bytecode = nibble as i32;
            if bytecode & 0x8 != 0 {
                bytecode -= 0x10;
            }
            let predict =
                (samples[k - 1] * ADAPT_COEFF1[0] + samples[k - 2] * ADAPT_COEFF2[0]) >> 8;
            samples[k] = (bytecode * idelta_save + predict).clamp(-32768, 32767);
        }
        let mut stepindex = 0i32;
        let mut previous = 0i32;
        for k in 2..4usize {
            let bytecode = ((nibbles[k - 2] >> 4) & 0x0F) as usize;
            let step = IMA_STEP_SIZE[stepindex as usize];
            let mut diff = step >> 3;
            if bytecode & 1 != 0 {
                diff += step >> 2;
            }
            if bytecode & 2 != 0 {
                diff += step >> 1;
            }
            if bytecode & 4 != 0 {
                diff += step;
            }
            if bytecode & 8 != 0 {
                diff = -diff;
            }
            let current = (previous + diff).clamp(-32768, 32767);
            stepindex = (stepindex + IMA_INDEX_ADJUST[bytecode]).clamp(0, 88);
            previous = current;
            samples[k] = (samples[k] + current).clamp(-32768, 32767);
        }
        for (k, &sample) in samples.iter().enumerate() {
            let expected = sample as f32 / SAMPLE_SCALE;
            assert!(
                (audio.interleaved[k] - expected).abs() < f32::EPSILON,
                "sample {k}: {} != {expected}",
                audio.interleaved[k]
            );
        }
    }

    // `Render` interleaves `Header.channels` samples per frame from the
    // per-channel `Samples` buffers (`reference/cpp/plugins/wutcwf.cpp:126-147`),
    // each channel decoded in order by `ReadBlock` (`:133-140`).
    #[test]
    fn stereo_channels_are_deinterleaved_by_stride() {
        // Two mono-sample blocks back to back: channel 0 then channel 1.
        let samples_per_block = 2;
        let bytes_per_block = BLOCK_HEADER_SIZE as i32;
        let stream = {
            let mut stream = header_bytes(2, 44100, 1, bytes_per_block, samples_per_block);
            stream.extend(block_bytes(10, 20, 16, 0, 0, &[]));
            stream.extend(block_bytes(-30, -40, 16, 0, 0, &[]));
            stream
        };
        let audio = decode_tcwf(&stream).expect("decode stereo stream");
        assert_eq!(audio.spec.channels, 2);
        assert_eq!(audio.spec.total_frames, Some(2));
        assert_eq!(
            audio.interleaved,
            vec![
                10.0 / SAMPLE_SCALE,
                -30.0 / SAMPLE_SCALE,
                20.0 / SAMPLE_SCALE,
                -40.0 / SAMPLE_SCALE,
            ]
        );
    }

    // The port turns every structural failure the reference stumbles over
    // into a typed error: a truncated 24-byte header
    // (`reference/cpp/plugins/wutcwf.cpp:197-199`), a bad mark (`:200-201`),
    // an out-of-range `ms_bpred` — the reference bails with
    // "おそらく同期がとれていない" (`:261-262`) — and an `ima_stepindex`
    // past the 89-entry `ima_step_size` table (`:52-63`), which the
    // reference would index out of bounds (`:302`).
    #[test]
    fn typed_errors_replace_silent_failures() {
        assert_eq!(
            decode_tcwf(b"nope").unwrap_err(),
            TcwfError::TruncatedHeader {
                needed: HEADER_SIZE,
                found: 4
            }
        );
        let mut wrong_mark = header_bytes(1, 44100, 1, 34, 4);
        wrong_mark[0] = b'X';
        assert_eq!(
            decode_tcwf(&wrong_mark).unwrap_err(),
            TcwfError::BadSignature
        );
        assert_eq!(
            decode_tcwf(&header_bytes(0, 44100, 1, 34, 4)).unwrap_err(),
            TcwfError::ZeroChannels
        );
        assert_eq!(
            decode_tcwf(&header_bytes(1, 0, 1, 34, 4)).unwrap_err(),
            TcwfError::InvalidSampleRate(0)
        );
        assert_eq!(
            decode_tcwf(&header_bytes(1, 44100, 0, 34, 4)).unwrap_err(),
            TcwfError::InvalidBlockCount(0)
        );
        assert_eq!(
            decode_tcwf(&header_bytes(1, 44100, 1, 33, 4)).unwrap_err(),
            TcwfError::InvalidBlockLayout {
                bytesperblock: 33,
                samplesperblock: 4
            }
        );
        // A well-formed header with a missing data block is a typed error.
        assert!(matches!(
            decode_tcwf(&header_bytes(1, 44100, 1, 34, 4)).unwrap_err(),
            TcwfError::TruncatedBlock { .. }
        ));
        // `bpred == 7` is rejected exactly like the reference.
        let mut stream = header_bytes(1, 44100, 1, 34, 4);
        stream.extend(block_bytes(0, 0, 16, 7, 0, &[0, 0]));
        assert_eq!(
            decode_tcwf(&stream).unwrap_err(),
            TcwfError::BadPredictor {
                block: 0,
                channel: 0,
                predictor: 7
            }
        );
        // `ima_stepindex > 88` would read past `ima_step_size`; the port
        // returns a typed error instead.
        let mut stream = header_bytes(1, 44100, 1, 34, 4);
        stream.extend(block_bytes(0, 0, 16, 0, 200, &[0, 0]));
        assert_eq!(
            decode_tcwf(&stream).unwrap_err(),
            TcwfError::BadStepIndex {
                block: 0,
                channel: 0,
                stepindex: 200
            }
        );
    }
}
