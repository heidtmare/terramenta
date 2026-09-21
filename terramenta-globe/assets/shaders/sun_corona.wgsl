// A thin additive shell around the Sun, on the same terms as
// `atmosphere.wgsl`: drawn back-face-only with depth writes off so it
// accumulates over whatever is behind it, brightest at the limb where the
// sightline grazes the shell rather than passing straight through it.
//
// Unlike the atmosphere it has no sun direction to shade by — the Sun lights
// itself from every side — so the glow is uniform but for a slow flicker.

#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

struct SunCoronaUniform {
    color: vec3<f32>,
    density: f32,
    falloff: f32,
    time: f32,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> corona: SunCoronaUniform;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let normal = normalize(in.world_normal);
    let to_view = normalize(view.world_position.xyz - in.world_position.xyz);

    let grazing = 1.0 - clamp(dot(normal, to_view), 0.0, 1.0);
    let thickness = pow(grazing, corona.falloff);

    let flicker = 0.92 + 0.08 * sin(corona.time * 1.7 + normal.x * 6.0 + normal.y * 4.3);

    let intensity = thickness * corona.density * flicker;
    return vec4<f32>(corona.color * intensity, 1.0);
}
