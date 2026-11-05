//! The destination-dependent blend program's GLSL source.
//!
//! The program is generated from the blend table in [`crate::blend`], so the
//! shader and the table cannot drift apart: every mode's per-channel
//! function is emitted from the same description the CPU path runs.
//!
//! The generator lives in the crate so a test can compare the committed
//! shader with the generated one; the `write_dst_blend_shader` binary writes
//! the generated source into the shader directory.

use crate::blend::{ChannelFunction, Composition, PhotoshopBlend};

/// The index the plain colour dodge — the mode outside the Photoshop family
/// — takes in the program.
pub const PLAIN_COLOR_DODGE: u32 = 16;
/// The index the plain darken takes in the program.
pub const PLAIN_DARKEN: u32 = 17;
/// The index the plain lighten takes in the program.
pub const PLAIN_LIGHTEN: u32 = 18;

/// The destination-dependent program's GLSL source.
///
/// The source is generated from the blend table: the modes, their indices,
/// their per-channel functions and the way the source's alpha mixes them in
/// all come from the table.
pub fn dst_blend_shader() -> String {
    let mut shader = String::new();
    shader.push_str(HEADER);
    shader.push_str(&mode_list());
    shader.push_str(HELPERS);
    shader.push_str(PLAIN_MODES);
    shader.push_str(&mode_functions());
    shader.push_str(&main_function());
    shader
}

/// The file's header: the version, the descriptors and the push constants.
const HEADER: &str = r#"#version 450

// Destination-dependent blend fragment stage.
//
// This file is generated from `crates/platform/render/src/blend.rs`; write it
// again with `cargo run -p render --bin write_dst_blend_shader`.
//
// `u_source` is the source quad's texture and `u_destination` is the copy of
// the destination the blend reads. The push constant's mode index selects the
// mode; every mode of the Photoshop family mixes the source's alpha into the
// destination and holds the destination's alpha.
//
// The source tint carries the node's colour only; the composed opacity
// travels in its own push constant, because the blend functions mix the
// source's alpha with the opacity separately.

layout(location = 0) in vec2 in_uv;
layout(location = 0) out vec4 out_color;

// The source texture is bound in the first descriptor set, the copy of the
// destination in the second one and the blend tables in the third; the
// Vulkan layouts bind the three sets and the interface test checks the
// numbers against the compiled module.
layout(set = 0, binding = 0) uniform sampler2D u_source;
layout(set = 1, binding = 0) uniform sampler2D u_destination;
layout(set = 2, binding = 0) readonly buffer BlendTables {
    uint soft_light[65536];
} blend_tables;

layout(push_constant) uniform PushConstants {
    mat4 mvp;
    vec4 tint;
    vec4 uv_rect;
    uint mode;
    float opacity;
    vec2 target_size;
} pc;

"#;

/// The helpers the modes' per-channel expressions call.
const HELPERS: &str = r#"// The channel helpers.
//
// The channels are whole bytes and the arithmetic is whole-word arithmetic:
// the mix's difference is signed and its shift rounds towards negative
// infinity, so a channel below the destination's darkens it.

uint mix_channel(uint destination, uint source, uint weight) {
    int difference = int(source) - int(destination);
    return uint((int(destination) + ((difference * int(weight)) >> 8)) & 0xFF);
}

uint fade_channel(uint source, uint weight) {
    return (source * weight) >> 8;
}

uint saturating_sum(uint destination, uint source) {
    return min(destination + source, 255u);
}

uint saturating_difference(uint destination, uint source) {
    return uint(max(int(destination) + int(source) - 255, 0));
}

uint truncated_product(uint destination, uint source) {
    return (destination * source) >> 8;
}

uint screen_channel(uint destination, uint source) {
    return destination + source - ((destination * source) >> 8);
}

uint overlay_channel(uint source, uint destination) {
    uint product = source * destination * 2u;
    if (destination < 128u) {
        return product / 255u;
    }
    return (source + destination) * 2u - product / 255u - 255u;
}

uint soft_light_channel(uint source, uint destination) {
    return blend_tables.soft_light[source * 256u + destination];
}

uint color_dodge_channel(uint source, uint destination) {
    if (255u - source <= destination) {
        return 255u;
    }
    return destination * 255u / (255u - source);
}

uint color_burn_channel(uint source, uint destination) {
    if (source <= 255u - destination) {
        return 0u;
    }
    return 255u - (255u - destination) * 255u / source;
}

uint abs_difference(uint first, uint second) {
    return first > second ? first - second : second - first;
}

uint exclusion_channel(uint destination, uint source) {
    return destination + source - ((destination * source) >> 7);
}

"#;

/// The modes outside the Photoshop family.
///
/// The plain colour dodge, the plain darken and the plain lighten fade the
/// source by the opacity and then replace the channel; they are not part of
/// the family's table.
const PLAIN_MODES: &str = r#"// The modes outside the Photoshop family.
//
// The plain colour dodge, the plain darken and the plain lighten fade the
// source by the opacity and replace the channel; they are not part of the
// family's table.

uvec3 blend_plain_color_dodge(vec3 source, vec3 destination, float factor) {
    vec3 scaled = destination / max(1.0 - (source - 0.001), destination);
    vec3 blended = mix(destination, scaled, factor);
    return uvec3(uint(round(blended.r * 255.0)),
                 uint(round(blended.g * 255.0)),
                 uint(round(blended.b * 255.0)));
}

uvec3 blend_plain_darken(vec3 source, vec3 destination, float factor) {
    vec3 blended = mix(destination, min(source, destination), factor);
    return uvec3(uint(round(blended.r * 255.0)),
                 uint(round(blended.g * 255.0)),
                 uint(round(blended.b * 255.0)));
}

uvec3 blend_plain_lighten(vec3 source, vec3 destination, float factor) {
    vec3 blended = mix(destination, max(source, destination), factor);
    return uvec3(uint(round(blended.r * 255.0)),
                 uint(round(blended.g * 255.0)),
                 uint(round(blended.b * 255.0)));
}

"#;

/// The table of the program's mode indices.
fn mode_list() -> String {
    let mut list = String::from("// The mode indices are:\n//\n");
    for mode in PhotoshopBlend::ALL {
        list.push_str(&format!(
            "//  {:>2}  {}\n",
            mode.index(),
            mode.name().replace(' ', "-")
        ));
    }
    list.push_str(&format!(
        "//  {:>2}  plain-color-dodge\n",
        PLAIN_COLOR_DODGE
    ));
    list.push_str(&format!("//  {:>2}  plain-darken\n", PLAIN_DARKEN));
    list.push_str(&format!("//  {:>2}  plain-lighten\n", PLAIN_LIGHTEN));
    list.push('\n');
    list
}

/// The per-mode functions, one per mode of the table.
fn mode_functions() -> String {
    let mut functions = String::new();
    for mode in PhotoshopBlend::ALL {
        functions.push_str(&format!(
            "uvec3 blend_{}(uvec3 source, uvec3 destination, uint weight) {{\n",
            identifier(mode.name())
        ));
        functions.push_str("    return uvec3(\n");
        let expressions: Vec<String> = [".r", ".g", ".b"]
            .iter()
            .map(|suffix| format!("        {}", channel_expression(mode, suffix)))
            .collect();
        functions.push_str(&expressions.join(",\n"));
        functions.push_str("\n        );\n}\n\n");
    }
    functions
}

/// The program's entry point, with the dispatch over the mode index.
fn main_function() -> String {
    let mut main = String::new();
    main.push_str("void main() {\n");
    main.push_str("    vec4 source_sample = texture(u_source, in_uv) * pc.tint;\n");
    main.push_str(
        "    vec4 destination_sample = texture(u_destination, gl_FragCoord.xy / pc.target_size);\n",
    );
    main.push('\n');
    main.push_str("    // The channels are whole bytes: the blend's arithmetic is whole-byte\n");
    main.push_str("    // arithmetic, so the samples are quantized before they are blended.\n");
    main.push_str("    uvec3 source = uvec3(uint(round(source_sample.r * 255.0)),\n");
    main.push_str("                          uint(round(source_sample.g * 255.0)),\n");
    main.push_str("                          uint(round(source_sample.b * 255.0)));\n");
    main.push_str("    uvec3 destination = uvec3(uint(round(destination_sample.r * 255.0)),\n");
    main.push_str("                             uint(round(destination_sample.g * 255.0)),\n");
    main.push_str("                             uint(round(destination_sample.b * 255.0)));\n");
    main.push('\n');
    main.push_str("    // The weight the source's alpha mixes the mode's channel with: the\n");
    main.push_str("    // source's alpha, scaled by the opacity.\n");
    main.push_str("    uint source_alpha = uint(round(source_sample.a * 255.0));\n");
    main.push_str("    uint opacity = uint(round(pc.opacity * 255.0));\n");
    main.push_str(
        "    uint weight = opacity >= 255u ? source_alpha : (source_alpha * opacity) >> 8;\n",
    );
    main.push('\n');
    main.push_str("    // The weight the modes outside the family mix with.\n");
    main.push_str("    float factor = source_sample.a * pc.opacity;\n");
    main.push('\n');
    main.push_str("    uvec3 blended;\n");
    for (position, mode) in PhotoshopBlend::ALL.iter().enumerate() {
        main.push_str(&format!(
            "    if (pc.mode == {}u) {{\n        blended = blend_{}(source, destination, weight);\n    }}",
            mode.index(),
            identifier(mode.name())
        ));
        if position + 1 < PhotoshopBlend::ALL.len() {
            main.push_str(" else");
        }
    }
    main.push_str(&format!(
        " else if (pc.mode == {PLAIN_COLOR_DODGE}u) {{\n        blended = blend_plain_color_dodge(source_sample.rgb, destination_sample.rgb, factor);\n    }}"
    ));
    main.push_str(&format!(
        " else if (pc.mode == {PLAIN_DARKEN}u) {{\n        blended = blend_plain_darken(source_sample.rgb, destination_sample.rgb, factor);\n    }}"
    ));
    main.push_str(
        " else {\n        blended = blend_plain_lighten(source_sample.rgb, destination_sample.rgb, factor);\n    }\n",
    );
    main.push('\n');
    main.push_str("    out_color = vec4(vec3(blended) / 255.0, destination_sample.a);\n");
    main.push_str("}\n");
    main
}

/// The GLSL expression that blends one channel of `mode`.
///
/// `suffix` names the channel of the vectors the expression reads.
fn channel_expression(mode: PhotoshopBlend, suffix: &str) -> String {
    let source = format!("source{suffix}");
    let destination = format!("destination{suffix}");
    match mode.composition() {
        Composition::MixByAlpha => format!(
            "mix_channel({destination}, {}, weight)",
            function_expression(mode.function(), &source, &destination)
        ),
        Composition::ReplaceFaded => function_expression(
            mode.function(),
            &format!("fade_channel({source}, weight)"),
            &destination,
        ),
    }
}

/// The GLSL expression that applies `function` to one channel.
fn function_expression(function: ChannelFunction, source: &str, destination: &str) -> String {
    match function {
        ChannelFunction::Source => source.to_owned(),
        ChannelFunction::SaturatedSum => format!("saturating_sum({destination}, {source})"),
        ChannelFunction::SaturatedDifference => {
            format!("saturating_difference({destination}, {source})")
        }
        ChannelFunction::TruncatedProduct => {
            format!("truncated_product({destination}, {source})")
        }
        ChannelFunction::Screen => format!("screen_channel({destination}, {source})"),
        ChannelFunction::Overlay => format!("overlay_channel({source}, {destination})"),
        ChannelFunction::SwappedOverlay => format!("overlay_channel({destination}, {source})"),
        ChannelFunction::SoftLight => format!("soft_light_channel({source}, {destination})"),
        ChannelFunction::ColorDodge => format!("color_dodge_channel({source}, {destination})"),
        ChannelFunction::ColorBurn => format!("color_burn_channel({source}, {destination})"),
        ChannelFunction::Lighten => format!("max({source}, {destination})"),
        ChannelFunction::Darken => format!("min({source}, {destination})"),
        ChannelFunction::Difference => format!("abs_difference({source}, {destination})"),
        ChannelFunction::Exclusion => format!("exclusion_channel({destination}, {source})"),
    }
}

/// A mode's name as an identifier.
fn identifier(name: &str) -> String {
    name.replace(' ', "_")
}
