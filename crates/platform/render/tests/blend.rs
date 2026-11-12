//! The Photoshop blend family's table and the program generated from it.
//!
//! The table is the one the engine's bitmap path and the GPU's
//! destination-dependent program both read, and the program is generated from
//! the table, so the two cannot disagree.
//!
//! Reference: the per-channel kernels are the functors of
//! `reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-660`, the
//! overlay, soft-light, colour-dodge and colour-burn tables are
//! `reference/krkr2/cpp/core/visual/gl/blend_function.cpp:526-547`, the
//! family is registered as the HDA variant — the destination's alpha held —
//! in `reference/krkr2/cpp/core/visual/RenderManager.cpp:2857-2924`, and the
//! opacity dispatch — the plain functor at the full opacity, the faded one
//! below it — is `reference/krkr2/cpp/core/visual/RenderManager.cpp:1798-1826`.

use render::blend::{ChannelFunction, Composition, PhotoshopBlend};
use render::blend_shader::dst_blend_shader;

/// One vector: the source, the destination, the opacity and every mode's
/// expected result.
#[derive(Debug)]
struct Vector {
    /// The source pixel.
    source: [u8; 4],
    /// The destination pixel.
    destination: [u8; 4],
    /// The pass opacity.
    opacity: u8,
    /// Every mode's expected result, in the table's order.
    expected: [[u8; 4]; 16],
}

const VECTORS: &[Vector] = &[
    Vector {
        source: [64, 128, 192, 255],
        destination: [200, 100, 50, 128],
        opacity: 255,
        expected: [
            [64, 127, 191, 128],
            [254, 227, 241, 128],
            [9, 0, 0, 128],
            [50, 50, 37, 128],
            [213, 177, 204, 128],
            [173, 100, 74, 128],
            [100, 100, 153, 128],
            [177, 100, 85, 128],
            [254, 199, 201, 128],
            [255, 199, 199, 128],
            [36, 0, 0, 128],
            [200, 127, 191, 128],
            [64, 100, 50, 128],
            [136, 28, 141, 128],
            [137, 27, 141, 128],
            [164, 127, 166, 128],
        ],
    },
    Vector {
        source: [60, 180, 90, 192],
        destination: [200, 100, 50, 128],
        opacity: 255,
        expected: [
            [95, 160, 80, 128],
            [241, 216, 117, 128],
            [53, 43, 12, 128],
            [84, 77, 25, 128],
            [210, 182, 104, 128],
            [178, 130, 38, 128],
            [120, 148, 38, 128],
            [181, 123, 35, 128],
            [241, 216, 70, 128],
            [242, 212, 67, 128],
            [66, 52, 12, 128],
            [200, 160, 80, 128],
            [95, 100, 50, 128],
            [155, 85, 42, 128],
            [155, 35, 17, 128],
            [175, 130, 91, 128],
        ],
    },
    Vector {
        source: [10, 20, 30, 40],
        destination: [200, 190, 180, 170],
        opacity: 255,
        expected: [
            [170, 163, 156, 170],
            [201, 193, 184, 170],
            [168, 160, 151, 170],
            [169, 162, 155, 170],
            [200, 190, 181, 170],
            [192, 181, 171, 170],
            [171, 164, 158, 170],
            [193, 183, 173, 170],
            [201, 192, 183, 170],
            [200, 192, 182, 170],
            [168, 160, 151, 170],
            [200, 190, 180, 170],
            [170, 163, 156, 170],
            [198, 186, 175, 170],
            [199, 187, 176, 170],
            [199, 188, 178, 170],
        ],
    },
    Vector {
        source: [10, 20, 30, 40],
        destination: [200, 190, 180, 170],
        opacity: 128,
        expected: [
            [185, 176, 168, 170],
            [200, 191, 182, 170],
            [184, 175, 165, 170],
            [184, 176, 167, 170],
            [200, 190, 180, 170],
            [196, 185, 175, 170],
            [185, 177, 169, 170],
            [196, 186, 176, 170],
            [200, 191, 181, 170],
            [200, 190, 181, 170],
            [184, 175, 165, 170],
            [200, 190, 180, 170],
            [185, 176, 168, 170],
            [199, 188, 177, 170],
            [200, 189, 178, 170],
            [199, 189, 179, 170],
        ],
    },
    Vector {
        source: [60, 180, 90, 192],
        destination: [200, 100, 50, 128],
        opacity: 128,
        expected: [
            [147, 130, 65, 128],
            [220, 158, 83, 128],
            [126, 71, 31, 128],
            [142, 88, 37, 128],
            [205, 141, 77, 128],
            [189, 115, 44, 128],
            [160, 124, 44, 128],
            [190, 111, 42, 128],
            [220, 158, 60, 128],
            [218, 135, 57, 128],
            [133, 76, 31, 128],
            [200, 130, 65, 128],
            [147, 100, 50, 128],
            [177, 92, 46, 128],
            [178, 33, 17, 128],
            [187, 115, 70, 128],
        ],
    },
    Vector {
        source: [255, 255, 255, 255],
        destination: [0, 0, 0, 255],
        opacity: 255,
        expected: [
            [254, 254, 254, 255],
            [254, 254, 254, 255],
            [0, 0, 0, 255],
            [0, 0, 0, 255],
            [254, 254, 254, 255],
            [0, 0, 0, 255],
            [254, 254, 254, 255],
            [0, 0, 0, 255],
            [254, 254, 254, 255],
            [0, 0, 0, 255],
            [0, 0, 0, 255],
            [254, 254, 254, 255],
            [0, 0, 0, 255],
            [254, 254, 254, 255],
            [254, 254, 254, 255],
            [254, 254, 254, 255],
        ],
    },
    Vector {
        source: [0, 0, 0, 0],
        destination: [200, 100, 50, 128],
        opacity: 255,
        expected: [
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
            [200, 100, 50, 128],
        ],
    },
    Vector {
        source: [128, 128, 128, 255],
        destination: [128, 128, 128, 255],
        opacity: 255,
        expected: [
            [128, 128, 128, 255],
            [254, 254, 254, 255],
            [1, 1, 1, 255],
            [64, 64, 64, 255],
            [191, 191, 191, 255],
            [128, 128, 128, 255],
            [128, 128, 128, 255],
            [128, 128, 128, 255],
            [254, 254, 254, 255],
            [255, 255, 255, 255],
            [2, 2, 2, 255],
            [128, 128, 128, 255],
            [128, 128, 128, 255],
            [0, 0, 0, 255],
            [1, 1, 1, 255],
            [128, 128, 128, 255],
        ],
    },
    // The two vectors whose blue channel is below the destination's blue
    // channel: the packed subtraction borrows out of the blue lane, and the
    // packed addition carries back into the red lane. The two cancel, so
    // the per-channel mix is the packed mix's result
    // (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-383`).
    Vector {
        source: [32, 31, 131, 99],
        destination: [32, 173, 185, 139],
        opacity: 171,
        expected: [
            [32, 136, 171, 139],
            [40, 180, 203, 139],
            [23, 128, 153, 139],
            [24, 133, 161, 139],
            [39, 175, 194, 139],
            [25, 157, 185, 139],
            [25, 139, 185, 139],
            [25, 161, 185, 139],
            [33, 178, 203, 139],
            [33, 177, 212, 139],
            [23, 128, 167, 139],
            [32, 173, 185, 139],
            [32, 136, 171, 139],
            [23, 165, 151, 139],
            [24, 166, 152, 139],
            [38, 170, 170, 139],
        ],
    },
    Vector {
        source: [250, 238, 120, 228],
        destination: [234, 91, 242, 204],
        opacity: 54,
        expected: [
            [237, 118, 219, 204],
            [237, 121, 244, 204],
            [233, 87, 216, 204],
            [232, 89, 217, 204],
            [238, 119, 243, 204],
            [237, 105, 242, 204],
            [237, 117, 239, 204],
            [235, 101, 241, 204],
            [237, 121, 244, 204],
            [255, 109, 255, 204],
            [234, 88, 239, 204],
            [237, 118, 242, 204],
            [234, 91, 219, 204],
            [193, 101, 219, 204],
            [188, 47, 220, 204],
            [195, 103, 222, 204],
        ],
    },
];

/// The committed shader is the program the table generates.
#[test]
fn the_committed_shader_is_the_generated_one() {
    let committed = include_str!("../shaders/dst_blend.frag");
    assert_eq!(
        committed,
        dst_blend_shader(),
        "the committed shader is not the one the table generates; write it again with \
         `cargo run -p render --bin write_dst_blend_shader`"
    );
}

/// Every mode of the family blends the reference's vectors exactly.
///
/// The whole-word arithmetic of the reference's functors is a per-channel
/// function and a mix by the source's alpha: the table's channel functions
/// reproduce it byte for byte, and the destination's alpha is held.
#[test]
fn every_mode_blends_the_reference_vectors() {
    for vector in VECTORS {
        for (mode, expected) in PhotoshopBlend::ALL.iter().zip(vector.expected.iter()) {
            let blended = mode.blend_pixel(vector.destination, vector.source, vector.opacity);
            assert_eq!(
                blended,
                *expected,
                "the mode {:?} blended {vector:?} into {:?} and gave {:?}",
                mode.name(),
                vector.destination,
                blended
            );
        }
    }
}

/// Every mode of the family has an index of its own.
///
/// The program selects its per-channel function with the index, so two modes
/// sharing an index would draw the same function.
#[test]
fn every_mode_has_an_index_of_its_own() {
    let mut indices: Vec<u32> = PhotoshopBlend::ALL
        .iter()
        .map(|mode| mode.index())
        .collect();
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(indices.len(), PhotoshopBlend::ALL.len());
    for (position, mode) in PhotoshopBlend::ALL.iter().enumerate() {
        assert_eq!(
            mode.index(),
            u32::try_from(position).expect("the mode's position")
        );
    }
}

/// The two legacy modes fade the source and replace the channel.
///
/// The legacy colour dodge and the legacy difference fade the source by the
/// source's alpha first and their table replaces the destination's channel;
/// every other mode mixes its channel into the destination's by the source's
/// alpha (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:568-581` and
/// `:630-642`).
#[test]
fn the_legacy_modes_replace_the_channel() {
    for mode in [PhotoshopBlend::COLOR_DODGE5, PhotoshopBlend::DIFFERENCE5] {
        assert_eq!(mode.composition(), Composition::ReplaceFaded);
    }
    for mode in [
        PhotoshopBlend::NORMAL,
        PhotoshopBlend::ADDITIVE,
        PhotoshopBlend::SUBTRACTIVE,
        PhotoshopBlend::MULTIPLICATIVE,
        PhotoshopBlend::SCREEN,
        PhotoshopBlend::OVERLAY,
        PhotoshopBlend::HARD_LIGHT,
        PhotoshopBlend::SOFT_LIGHT,
        PhotoshopBlend::COLOR_DODGE,
        PhotoshopBlend::COLOR_BURN,
        PhotoshopBlend::LIGHTEN,
        PhotoshopBlend::DARKEN,
        PhotoshopBlend::DIFFERENCE,
        PhotoshopBlend::EXCLUSION,
    ] {
        assert_eq!(mode.composition(), Composition::MixByAlpha);
    }
}

/// The hard light is the overlay table with the operands swapped.
///
/// The hard light takes the overlay table with the source and the
/// destination exchanged
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:537-564`).
#[test]
fn the_hard_light_is_the_swapped_overlay() {
    assert_eq!(
        PhotoshopBlend::HARD_LIGHT.function(),
        ChannelFunction::SwappedOverlay
    );
    assert_eq!(PhotoshopBlend::OVERLAY.function(), ChannelFunction::Overlay);
}

/// The soft-light table the GPU reads is the table's own.
///
/// The soft-light entries are the truncated values of a double-precision
/// power, and a shader's floating-point power cannot reproduce them, so the
/// program reads them from the table the pipeline uploads
/// (`reference/krkr2/cpp/core/visual/gl/blend_function.cpp:526-547`).
#[test]
fn the_soft_light_table_holds_every_entry() {
    let table = render::blend::soft_light_table();
    assert_eq!(table.len(), 256 * 256);
    for source in 0..=u8::MAX {
        for destination in 0..=u8::MAX {
            let expected = PhotoshopBlend::SOFT_LIGHT
                .function()
                .apply(source, destination) as u8;
            let index = usize::from(source) * 256 + usize::from(destination);
            assert_eq!(
                table[index], expected,
                "the entry for {source} over {destination}"
            );
        }
    }
}
