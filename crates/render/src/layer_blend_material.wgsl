// Fragment shader for krkr_render's LayerBlendMaterial (see blend.rs).
//
// Samples the layer's straight-alpha texture and multiplies the straight-
// alpha tint (which carries the composed window × layer opacity). No alpha
// math happens here: the compositing equation itself (source-over / add /
// reverse-subtract / replace) is chosen per pipeline variant via the
// fixed-function BlendState in `LayerBlendMaterial::specialize`.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct LayerMaterial {
    color: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> material: LayerMaterial;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var color_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var color_sampler: sampler;

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    return material.color * textureSample(color_texture, color_sampler, mesh.uv);
}
