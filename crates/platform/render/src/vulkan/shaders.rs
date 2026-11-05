//! Precompiled SPIR-V shader modules.
//!
//! The GLSL sources live next to this crate under `shaders/` and are compiled
//! with `glslc --target-env=vulkan1.0 -O` as part of preparing the source
//! tree. Embedding the resulting `.spv` blobs keeps the crate free of a build
//! dependency on a shader compiler while still shipping real compiled code.
//!
//! The descriptors the shaders declare are named here: the GLSL source, the
//! compiled module and the Vulkan descriptor set layouts must agree on the
//! set and the binding of every sampler, so the numbers live in one place
//! and the interface test checks the three against each other.
//!
//! Push-constant block layout (shared by every module):
//!
//! | offset | field     | bytes |
//! |--------|-----------|-------|
//! | 0      | `mat4 mvp`| 64    |
//! | 64     | `vec4 tint`| 16   |
//! | 80     | `vec4 uv_rect`| 16 |
//! | 96     | `uint mode`| 4    |
//! | 100    | `float opacity`| 4 |
//! | 104    | `vec2 target_size`| 8 |

/// Vertex stage for the unit quad.
pub(crate) const QUAD_VERT: &[u8] = include_bytes!("../../shaders/quad.vert.spv");
/// Fragment stage that samples the bound texture.
pub(crate) const TEXTURED_FRAG: &[u8] = include_bytes!("../../shaders/textured.frag.spv");
/// Fragment stage that outputs the tint directly.
pub(crate) const SOLID_FRAG: &[u8] = include_bytes!("../../shaders/solid.frag.spv");
/// Fragment stage that blends a source over a copy of the destination.
pub(crate) const DST_BLEND_FRAG: &[u8] = include_bytes!("../../shaders/dst_blend.frag.spv");

/// A descriptor a shader declares.
///
/// The set and the binding are the numbers the GLSL source, the compiled
/// SPIR-V module and the Vulkan descriptor set layout must all name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Descriptor {
    /// The descriptor set the descriptor is bound in.
    pub(crate) set: u32,
    /// The binding the descriptor occupies in its set.
    pub(crate) binding: u32,
}

/// The descriptor the scene's programs bind the source texture through.
///
/// The source texture is the first descriptor set's only descriptor, so the
/// set holds one binding and the shader samples it at binding 0.
pub(crate) const SOURCE_DESCRIPTOR: Descriptor = Descriptor { set: 0, binding: 0 };

/// The descriptor the destination-dependent program binds the copy of the
/// destination through.
///
/// The copy of the destination is the second descriptor set's only
/// descriptor, so the set holds one binding and the shader samples it at
/// binding 0.
pub(crate) const DESTINATION_DESCRIPTOR: Descriptor = Descriptor { set: 1, binding: 0 };

/// Converts a little-endian SPIR-V byte blob into 32-bit words.
pub(crate) fn to_words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| u32::from_le_bytes(*chunk))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{DESTINATION_DESCRIPTOR, Descriptor, SOURCE_DESCRIPTOR, to_words};
    use crate::collections::Map;

    /// The opcode of the `OpDecorate` instruction.
    const OP_DECORATE: u32 = 71;
    /// The `Binding` decoration.
    const DECORATION_BINDING: u32 = 33;
    /// The `DescriptorSet` decoration.
    const DECORATION_DESCRIPTOR_SET: u32 = 34;
    /// The magic number every SPIR-V module starts with.
    const SPIRV_MAGIC: u32 = 0x0723_0203;
    /// The number of words in a SPIR-V module's header.
    const SPIRV_HEADER_WORDS: usize = 5;

    /// Every shader in the directory: the GLSL source and the compiled blob.
    ///
    /// The blobs are the crate's own, so the test checks the files the crate
    /// ships.
    const SHADERS: &[(&str, &str, &[u8])] = &[
        (
            "quad.vert",
            include_str!("../../shaders/quad.vert"),
            super::QUAD_VERT,
        ),
        (
            "textured.frag",
            include_str!("../../shaders/textured.frag"),
            super::TEXTURED_FRAG,
        ),
        (
            "solid.frag",
            include_str!("../../shaders/solid.frag"),
            super::SOLID_FRAG,
        ),
        (
            "dst_blend.frag",
            include_str!("../../shaders/dst_blend.frag"),
            super::DST_BLEND_FRAG,
        ),
    ];

    /// The descriptor every sampler of every shader is expected to declare.
    ///
    /// The scene's programs bind the source texture through the source's
    /// descriptor; the destination-dependent program binds the copy of the
    /// destination through the destination's. These are the numbers the
    /// Vulkan descriptor set layouts build themselves from.
    const EXPECTED: &[(&str, &str, Descriptor)] = &[
        ("textured.frag", "u_texture", SOURCE_DESCRIPTOR),
        ("dst_blend.frag", "u_source", SOURCE_DESCRIPTOR),
        ("dst_blend.frag", "u_destination", DESTINATION_DESCRIPTOR),
    ];

    /// Every shader's source, compiled module and layout name the same
    /// descriptor sets and bindings.
    ///
    /// A shader that names a set and a binding the layouts do not provide
    /// reads a descriptor that does not exist: the pipeline creation fails on
    /// a strict driver, and the draw reads unbound data on a lenient one.
    #[test]
    fn every_shader_agrees_with_its_compiled_module_and_the_layouts() {
        for (name, source, blob) in SHADERS {
            let declared = glsl_descriptors(source);
            let compiled = module_descriptors(blob);
            let mut declared_slots: Vec<(u32, u32)> = declared
                .iter()
                .map(|(_, set, binding)| (*set, *binding))
                .collect();
            declared_slots.sort();
            assert_eq!(
                declared_slots, compiled,
                "the {name} source and its compiled module declare different descriptors"
            );

            for (variable, set, binding) in &declared {
                let expected = EXPECTED
                    .iter()
                    .find(|(shader, sampler, _)| *shader == *name && *sampler == *variable)
                    .unwrap_or_else(|| {
                        panic!(
                            "the {name} shader declares {variable}, which no descriptor set \
                             layout binds"
                        )
                    });
                let expected_set = expected.2.set;
                let expected_binding = expected.2.binding;
                assert_eq!(
                    (*set, *binding),
                    (expected_set, expected_binding),
                    "the {name} shader's {variable} names set {set} binding {binding}, while the \
                     layout binds set {expected_set} binding {expected_binding}"
                );
            }
        }

        // Every expected sampler must be declared, so a renamed variable
        // cannot slip past the comparison above.
        for (name, variable, expected) in EXPECTED {
            let source = SHADERS
                .iter()
                .find(|(shader, _, _)| *shader == *name)
                .map(|(_, source, _)| *source)
                .unwrap_or_else(|| panic!("the {name} shader is not covered by the test"));
            let declared = glsl_descriptors(source);
            let found = declared
                .iter()
                .find(|(sampler, _, _)| *sampler == *variable)
                .unwrap_or_else(|| panic!("the {name} shader does not declare {variable}"));
            assert_eq!(
                (found.1, found.2),
                (expected.set, expected.binding),
                "the {name} shader's {variable} names another descriptor than the layout binds"
            );
        }
    }

    /// Every shader file in the directory is covered by the test.
    ///
    /// A shader that is added to the directory without a place in the table
    /// would not be checked, so the table must name every file.
    #[test]
    fn every_shader_in_the_directory_is_covered() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders");
        let mut files: Vec<String> = std::fs::read_dir(&directory)
            .expect("the shaders directory")
            .map(|entry| {
                entry
                    .expect("a shader entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(".frag") || name.ends_with(".vert"))
            .collect();
        files.sort();
        let mut covered: Vec<String> = SHADERS
            .iter()
            .map(|(name, _, _)| (*name).to_owned())
            .collect();
        covered.sort();
        assert_eq!(
            covered, files,
            "every shader in the directory must be covered by the interface test"
        );
    }

    /// The descriptors a GLSL source declares.
    ///
    /// A declaration names the set and the binding in its `layout` qualifier
    /// and the sampler in the declaration itself.
    fn glsl_descriptors(source: &str) -> Vec<(&str, u32, u32)> {
        let mut descriptors = Vec::new();
        for line in source.lines() {
            if !line.contains("layout(set") {
                continue;
            }
            let Some(set) = number_after(line, "set =") else {
                continue;
            };
            let Some(binding) = number_after(line, "binding =") else {
                continue;
            };
            let Some(declaration) = line.split(';').next() else {
                continue;
            };
            let Some(variable) = declaration.split_whitespace().next_back() else {
                continue;
            };
            descriptors.push((variable, set, binding));
        }
        descriptors
    }

    /// The number a marker introduces, up to the next separator.
    fn number_after(text: &str, marker: &str) -> Option<u32> {
        let rest = text.split_once(marker)?.1;
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .collect();
        digits.parse().ok()
    }

    /// The descriptor sets and bindings a compiled module declares.
    ///
    /// The `OpDecorate` instructions carry the two decorations every
    /// descriptor variable has: the `DescriptorSet` and the `Binding`.
    fn module_descriptors(blob: &[u8]) -> Vec<(u32, u32)> {
        let words = to_words(blob);
        assert_eq!(
            words.first(),
            Some(&SPIRV_MAGIC),
            "the blob is not a SPIR-V module"
        );
        let mut sets: Map<u32, u32> = Map::default();
        let mut bindings: Map<u32, u32> = Map::default();
        let mut index = SPIRV_HEADER_WORDS;
        while index < words.len() {
            let header = words[index];
            let opcode = header & 0xffff;
            let length = (header >> 16) as usize;
            assert!(length >= 1, "the module has an instruction without words");
            if opcode == OP_DECORATE {
                // An OpDecorate carries the target and the decoration, and some
                // decorations carry no operand at all.
                assert!(
                    length >= 3,
                    "the module has an OpDecorate instruction without a decoration"
                );
                let target = words[index + 1];
                if length < 4 {
                    index += length;
                    continue;
                }
                match words[index + 2] {
                    DECORATION_BINDING => {
                        bindings.insert(target, words[index + 3]);
                    }
                    DECORATION_DESCRIPTOR_SET => {
                        sets.insert(target, words[index + 3]);
                    }
                    _ => {}
                }
            }
            index += length;
        }
        let mut descriptors: Vec<(u32, u32)> = bindings
            .iter()
            .filter_map(|(target, binding)| sets.get(target).map(|set| (*set, *binding)))
            .collect();
        descriptors.sort();
        descriptors
    }
}
