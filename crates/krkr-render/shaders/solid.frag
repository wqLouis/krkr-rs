#version 450

// Solid quad fragment stage: output the per-node tint directly.

layout(location = 0) out vec4 out_color;

layout(push_constant) uniform PushConstants {
    mat4 mvp;
    vec4 tint;
    vec4 uv_rect;
} pc;

void main() {
    out_color = pc.tint;
}
