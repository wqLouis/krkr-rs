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
