#version 450

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

// The mode indices are:
//
//   0  normal
//   1  additive
//   2  subtractive
//   3  multiplicative
//   4  screen
//   5  overlay
//   6  hard-light
//   7  soft-light
//   8  colour-dodge
//   9  colour-dodge-5
//  10  colour-burn
//  11  lighten
//  12  darken
//  13  difference
//  14  difference-5
//  15  exclusion
//  16  plain-color-dodge
//  17  plain-darken
//  18  plain-lighten

// The channel helpers.
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

// The modes outside the Photoshop family.
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

uvec3 blend_normal(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, source.r, weight),
        mix_channel(destination.g, source.g, weight),
        mix_channel(destination.b, source.b, weight)
        );
}

uvec3 blend_additive(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, saturating_sum(destination.r, source.r), weight),
        mix_channel(destination.g, saturating_sum(destination.g, source.g), weight),
        mix_channel(destination.b, saturating_sum(destination.b, source.b), weight)
        );
}

uvec3 blend_subtractive(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, saturating_difference(destination.r, source.r), weight),
        mix_channel(destination.g, saturating_difference(destination.g, source.g), weight),
        mix_channel(destination.b, saturating_difference(destination.b, source.b), weight)
        );
}

uvec3 blend_multiplicative(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, truncated_product(destination.r, source.r), weight),
        mix_channel(destination.g, truncated_product(destination.g, source.g), weight),
        mix_channel(destination.b, truncated_product(destination.b, source.b), weight)
        );
}

uvec3 blend_screen(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, screen_channel(destination.r, source.r), weight),
        mix_channel(destination.g, screen_channel(destination.g, source.g), weight),
        mix_channel(destination.b, screen_channel(destination.b, source.b), weight)
        );
}

uvec3 blend_overlay(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, overlay_channel(source.r, destination.r), weight),
        mix_channel(destination.g, overlay_channel(source.g, destination.g), weight),
        mix_channel(destination.b, overlay_channel(source.b, destination.b), weight)
        );
}

uvec3 blend_hard_light(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, overlay_channel(destination.r, source.r), weight),
        mix_channel(destination.g, overlay_channel(destination.g, source.g), weight),
        mix_channel(destination.b, overlay_channel(destination.b, source.b), weight)
        );
}

uvec3 blend_soft_light(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, soft_light_channel(source.r, destination.r), weight),
        mix_channel(destination.g, soft_light_channel(source.g, destination.g), weight),
        mix_channel(destination.b, soft_light_channel(source.b, destination.b), weight)
        );
}

uvec3 blend_colour_dodge(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, color_dodge_channel(source.r, destination.r), weight),
        mix_channel(destination.g, color_dodge_channel(source.g, destination.g), weight),
        mix_channel(destination.b, color_dodge_channel(source.b, destination.b), weight)
        );
}

uvec3 blend_colour_dodge_5(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        color_dodge_channel(fade_channel(source.r, weight), destination.r),
        color_dodge_channel(fade_channel(source.g, weight), destination.g),
        color_dodge_channel(fade_channel(source.b, weight), destination.b)
        );
}

uvec3 blend_colour_burn(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, color_burn_channel(source.r, destination.r), weight),
        mix_channel(destination.g, color_burn_channel(source.g, destination.g), weight),
        mix_channel(destination.b, color_burn_channel(source.b, destination.b), weight)
        );
}

uvec3 blend_lighten(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, max(source.r, destination.r), weight),
        mix_channel(destination.g, max(source.g, destination.g), weight),
        mix_channel(destination.b, max(source.b, destination.b), weight)
        );
}

uvec3 blend_darken(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, min(source.r, destination.r), weight),
        mix_channel(destination.g, min(source.g, destination.g), weight),
        mix_channel(destination.b, min(source.b, destination.b), weight)
        );
}

uvec3 blend_difference(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, abs_difference(source.r, destination.r), weight),
        mix_channel(destination.g, abs_difference(source.g, destination.g), weight),
        mix_channel(destination.b, abs_difference(source.b, destination.b), weight)
        );
}

uvec3 blend_difference_5(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        abs_difference(fade_channel(source.r, weight), destination.r),
        abs_difference(fade_channel(source.g, weight), destination.g),
        abs_difference(fade_channel(source.b, weight), destination.b)
        );
}

uvec3 blend_exclusion(uvec3 source, uvec3 destination, uint weight) {
    return uvec3(
        mix_channel(destination.r, exclusion_channel(destination.r, source.r), weight),
        mix_channel(destination.g, exclusion_channel(destination.g, source.g), weight),
        mix_channel(destination.b, exclusion_channel(destination.b, source.b), weight)
        );
}

void main() {
    vec4 source_sample = texture(u_source, in_uv) * pc.tint;
    vec4 destination_sample = texture(u_destination, gl_FragCoord.xy / pc.target_size);

    // The channels are whole bytes: the blend's arithmetic is whole-byte
    // arithmetic, so the samples are quantized before they are blended.
    uvec3 source = uvec3(uint(round(source_sample.r * 255.0)),
                          uint(round(source_sample.g * 255.0)),
                          uint(round(source_sample.b * 255.0)));
    uvec3 destination = uvec3(uint(round(destination_sample.r * 255.0)),
                             uint(round(destination_sample.g * 255.0)),
                             uint(round(destination_sample.b * 255.0)));

    // The weight the source's alpha mixes the mode's channel with: the
    // source's alpha, scaled by the opacity.
    uint source_alpha = uint(round(source_sample.a * 255.0));
    uint opacity = uint(round(pc.opacity * 255.0));
    uint weight = opacity >= 255u ? source_alpha : (source_alpha * opacity) >> 8;

    // The weight the modes outside the family mix with.
    float factor = source_sample.a * pc.opacity;

    uvec3 blended;
    if (pc.mode == 0u) {
        blended = blend_normal(source, destination, weight);
    } else    if (pc.mode == 1u) {
        blended = blend_additive(source, destination, weight);
    } else    if (pc.mode == 2u) {
        blended = blend_subtractive(source, destination, weight);
    } else    if (pc.mode == 3u) {
        blended = blend_multiplicative(source, destination, weight);
    } else    if (pc.mode == 4u) {
        blended = blend_screen(source, destination, weight);
    } else    if (pc.mode == 5u) {
        blended = blend_overlay(source, destination, weight);
    } else    if (pc.mode == 6u) {
        blended = blend_hard_light(source, destination, weight);
    } else    if (pc.mode == 7u) {
        blended = blend_soft_light(source, destination, weight);
    } else    if (pc.mode == 8u) {
        blended = blend_colour_dodge(source, destination, weight);
    } else    if (pc.mode == 9u) {
        blended = blend_colour_dodge_5(source, destination, weight);
    } else    if (pc.mode == 10u) {
        blended = blend_colour_burn(source, destination, weight);
    } else    if (pc.mode == 11u) {
        blended = blend_lighten(source, destination, weight);
    } else    if (pc.mode == 12u) {
        blended = blend_darken(source, destination, weight);
    } else    if (pc.mode == 13u) {
        blended = blend_difference(source, destination, weight);
    } else    if (pc.mode == 14u) {
        blended = blend_difference_5(source, destination, weight);
    } else    if (pc.mode == 15u) {
        blended = blend_exclusion(source, destination, weight);
    } else if (pc.mode == 16u) {
        blended = blend_plain_color_dodge(source_sample.rgb, destination_sample.rgb, factor);
    } else if (pc.mode == 17u) {
        blended = blend_plain_darken(source_sample.rgb, destination_sample.rgb, factor);
    } else {
        blended = blend_plain_lighten(source_sample.rgb, destination_sample.rgb, factor);
    }

    out_color = vec4(vec3(blended) / 255.0, destination_sample.a);
}
