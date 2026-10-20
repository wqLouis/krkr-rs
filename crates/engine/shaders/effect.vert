#version 450

// Shared effect vertex stage: the shared unit quad through the per-draw MVP.
//
// The quad is the renderer's one vertex buffer, `(0,0) (1,0) (0,1) (1,1)` drawn
// as a triangle strip, so `in_pos` is the normalised position inside the node's
// destination rectangle. A fragment stage maps it through its uniform texture
// rectangles, so no per-vertex attribute beyond the position is needed.

layout(location = 0) in vec2 in_pos;
layout(location = 0) out vec2 out_uv;

layout(push_constant) uniform Push {
    mat4 mvp;
} pc;

void main() {
    gl_Position = pc.mvp * vec4(in_pos, 0.0, 1.0);
    out_uv = in_pos;
}
