#version 450

// Scroll: one source placed by the draw's geometry, shaded straight through.
//
// Each scroll pass draws one source over its own destination rectangle, so this
// program samples a single texture through `u_rect0` and applies the composed
// opacity to the alpha the blend state consumes. The uniform block matches
// `crossfade.frag` word for word, so one descriptor layout and one uniform ring
// serve every effect program.

layout(location = 0) in vec2 in_uv;
layout(location = 0) out vec4 out_color;

layout(set = 0, binding = 0) uniform Params {
    vec4 u_rect0;
    vec4 u_rect1;
    vec4 u_extra0;
    vec4 u_extra1;
} params;

layout(set = 0, binding = 1) uniform sampler2D src1;

void main() {
    vec2 uv = mix(params.u_rect0.xy, params.u_rect0.zw, in_uv);
    vec4 c = texture(src1, uv);
    out_color = vec4(c.rgb, c.a * params.u_extra1.w);
}
