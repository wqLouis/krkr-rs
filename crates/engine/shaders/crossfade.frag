#version 450

// Cross-fade: the per-channel lerp of two sources at the phase weight.
//
// Uniform block layout (16 words, shared by every effect program):
//   words  0..4  u_rect0.xyzw = (u0, v0, u1, v1) of the source at binding 1
//   words  4..8  u_rect1.xyzw = the same for the source at binding 2
//   words  8..12 u_extra0     = parameters; x is the blend weight
//   words 12..16 u_extra1     = parameters; w is the composed draw opacity
// The renderer overwrites the declared opacity word (15) with the layer and
// window opacity for the frame, so the program never reads a stale value.
//
// The two sources are sampled from sRGB textures, so the sampler hands this
// stage linear light. The blend this program reproduces is the byte-space one
// (`s1 + ((s2 - s1) * opa) >> 8`), so the stage converts each sample back to
// its encoded value, mixes there, and converts the mixed value back to linear
// for the sRGB target. The alpha channel carries no transfer function, so it
// is mixed as it comes.

layout(location = 0) in vec2 in_uv;
layout(location = 0) out vec4 out_color;

layout(set = 0, binding = 0) uniform Params {
    vec4 u_rect0;
    vec4 u_rect1;
    vec4 u_extra0;
    vec4 u_extra1;
} params;

layout(set = 0, binding = 1) uniform sampler2D src1;
layout(set = 0, binding = 2) uniform sampler2D src2;

// Linear light to the encoded value the byte-space blend works on.
vec3 to_encoded(vec3 linear) {
    vec3 low = linear * 12.92;
    vec3 high = 1.055 * pow(max(linear, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return mix(low, high, step(vec3(0.0031308), linear));
}

// The encoded value back to linear light.
vec3 to_linear(vec3 encoded) {
    vec3 low = encoded / 12.92;
    vec3 high = pow(max(encoded + 0.055, vec3(0.0)) / 1.055, vec3(2.4));
    return mix(low, high, step(vec3(0.04045), encoded));
}

void main() {
    vec2 uv0 = mix(params.u_rect0.xy, params.u_rect0.zw, in_uv);
    vec2 uv1 = mix(params.u_rect1.xy, params.u_rect1.zw, in_uv);
    vec4 a = texture(src1, uv0);
    vec4 b = texture(src2, uv1);
    // The byte-space lerp, in the form the reference's arithmetic takes: the
    // difference first, so two equal samples mix to exactly that sample.
    vec3 first = to_encoded(a.rgb);
    vec3 second = to_encoded(b.rgb);
    vec3 encoded = first + (second - first) * params.u_extra0.x;
    float alpha = a.a + (b.a - a.a) * params.u_extra0.x;
    // Two equal samples are that sample; keeping the value the sampler gave
    // spares the conversion's rounding where one source is copied verbatim.
    vec3 rgb = all(equal(a.rgb, b.rgb)) ? a.rgb : to_linear(encoded);
    // The target is an sRGB surface, so the mixed encoded value goes back to
    // linear light; the composed opacity scales the alpha the blend consumes.
    out_color = vec4(rgb, alpha * params.u_extra1.w);
}
