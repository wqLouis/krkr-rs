#version 450

// Textured quad fragment stage: sample the bound image and modulate it by the
// per-node tint (RGB is linear; A carries the composed opacity).

layout(location = 0) in vec2 in_uv;
layout(location = 0) out vec4 out_color;

layout(set = 0, binding = 0) uniform sampler2D u_texture;

layout(push_constant) uniform PushConstants {
    mat4 mvp;
    vec4 tint;
    vec4 uv_rect;
} pc;

void main() {
    out_color = texture(u_texture, in_uv) * pc.tint;
}
