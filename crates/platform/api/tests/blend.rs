//! Integration tests for [`api::blend`].

use api::blend::*;

#[test]
fn default_is_source_over() {
    assert_eq!(BlendMode::default(), BlendMode::SourceOver);
}

#[test]
fn destination_dependent_wraps_dst_blend() {
    let mode = BlendMode::DestinationDependent(DstBlend::SoftLight);
    assert!(mode.is_destination_dependent());
    assert_eq!(mode.dst_blend(), Some(DstBlend::SoftLight));
    assert!(!BlendMode::Multiply.is_destination_dependent());
    assert_eq!(BlendMode::Multiply.dst_blend(), None);
}

/// The Photoshop subtractive blend is the linear burn, so it reads the
/// destination as the destination-dependent modes do.
#[test]
fn the_photoshop_subtractive_blend_reads_the_destination() {
    assert!(BlendMode::PsSubtractive.is_destination_dependent());
}

/// Every mode of the Photoshop family reads the destination.
///
/// The family's per-channel function takes the destination's channel, so the
/// destination-dependent program draws every mode of it
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-660`).
#[test]
fn every_photoshop_mode_reads_the_destination() {
    for mode in [
        BlendMode::PsNormal,
        BlendMode::PsAdditive,
        BlendMode::PsSubtractive,
        BlendMode::PsMultiplicative,
        BlendMode::PsScreen,
    ] {
        assert!(mode.is_destination_dependent(), "the mode {mode:?}");
    }
}
