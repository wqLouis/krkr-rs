#version 450

// Destination-dependent blend fragment stage.
//
// `u_source` is the source quad's texture and `u_destination` is the copy of
// the destination the blend reads. The push constant's mode index selects the
// per-channel function; every mode mixes the source with the destination the
// way the engine's blend shaders do, and the destination's alpha is held.
//
// The mode indices are:
//
//   0  colour dodge          7  lighten (Photoshop)
//   1  colour dodge (Photo)  8  difference
//   2  colour dodge 5        9  difference 5
//   3  colour burn          10  exclusion
//   4  darken               11  overlay
//   5  darken (Photoshop)   12  hard light
//   6  lighten              13  soft light
//  14  Photoshop subtractive (the linear burn)
//
// The source tint carries the node's colour only; the composed opacity
// travels in its own push constant, because the blend functions mix the
// source's alpha with the opacity separately.

layout(location = 0) in vec2 in_uv;
layout(location = 0) out vec4 out_color;

layout(set = 0, binding = 0) uniform sampler2D u_source;
layout(set = 0, binding = 1) uniform sampler2D u_destination;

layout(push_constant) uniform PushConstants {
    mat4 mvp;
    vec4 tint;
    vec4 uv_rect;
    uint mode;
    float opacity;
    vec2 target_size;
} pc;

void main() {
    vec4 source = texture(u_source, in_uv) * pc.tint;
    vec4 destination = texture(u_destination, gl_FragCoord.xy / pc.target_size);
    vec3 d = destination.rgb;
    float opacity = pc.opacity;
    float factor = source.a * opacity;
    vec3 rgb = d;

    if (pc.mode == 0u) {
        // Colour dodge: the destination scaled by the reciprocal of the
        // source's complement.
        rgb = d * (1.0 / max(1.0 - source.rgb * opacity, 1.0 / 255.0));
    } else if (pc.mode == 1u) {
        // Colour dodge: the source darkens the destination.
        vec3 k = d / max(1.0 - (source.rgb - 0.001), d);
        rgb = mix(d, k, factor);
    } else if (pc.mode == 2u) {
        // The colour-dodge-5 approximation: the source fades first and the
        // result replaces the destination.
        rgb = d / max(1.0 - (source.rgb * factor - 0.001), d);
    } else if (pc.mode == 3u) {
        // Colour burn: the source darkens the destination.
        vec3 k = source.rgb + 0.001;
        rgb = mix(d, 1.0 - min(1.0 - d, k) / k, factor);
    } else if (pc.mode == 4u) {
        // Darken: the darker channel, mixed by the opacity.
        rgb = mix(d, min(source.rgb, d), opacity);
    } else if (pc.mode == 5u) {
        // Darken: the darker channel, mixed by the source's alpha.
        rgb = mix(d, min(source.rgb, d), factor);
    } else if (pc.mode == 6u) {
        // Lighten: the lighter channel, mixed by the opacity.
        rgb = mix(d, max(source.rgb, d), opacity);
    } else if (pc.mode == 7u) {
        // Lighten: the lighter channel, mixed by the source's alpha.
        rgb = mix(d, max(source.rgb, d), factor);
    } else if (pc.mode == 8u) {
        // Difference: the absolute difference, mixed by the source's alpha.
        rgb = mix(d, abs(source.rgb - d), factor);
    } else if (pc.mode == 9u) {
        // The difference-5 approximation: the source fades first and the
        // result replaces the destination.
        rgb = abs(source.rgb * factor - d);
    } else if (pc.mode == 10u) {
        // Exclusion.
        rgb = d + (source.rgb - source.rgb * d * 2.0) * factor;
    } else if (pc.mode == 11u) {
        // Overlay.
        vec3 sd = source.rgb * d;
        vec3 k = step(0.5, d) * ((source.rgb + d) * 2.0 - sd * 4.0 - 1.0) + sd * 2.0;
        rgb = mix(d, k, factor);
    } else if (pc.mode == 12u) {
        // Hard light: the overlay with the operands swapped.
        vec3 sd = source.rgb * d;
        vec3 k = step(0.5, source.rgb) * ((source.rgb + d) * 2.0 - sd * 4.0 - 1.0) + sd * 2.0;
        rgb = mix(d, k, factor);
    } else if (pc.mode == 13u) {
        // Soft light.
        vec3 t = 0.5 / (source.rgb + 0.001);
        vec3 k = pow(d, (1.0 - step(0.5, source.rgb)) * ((1.0 - source.rgb) * 2.0 - t) + t);
        rgb = mix(d, k, factor);
    } else {
        // Photoshop subtractive: the linear burn. The source is inverted and
        // the difference saturates, so the channel is `max(0, dst + src - 1)`.
        rgb = mix(d, clamp(d + source.rgb - 1.0, 0.0, 1.0), factor);
    }

    out_color = vec4(rgb, destination.a);
}
