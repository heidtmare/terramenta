// A single WMS imagery tile on the globe's surface.
//
// The tile carries no day/night or cloud imagery of its own — a WMS layer is
// just a picture of the ground — so it reproduces the sunlight terms from
// `globe.wgsl` and nothing else. The constants below are deliberately the same
// as that shader's, so a tile and the globe showing through beside it are lit
// identically; change one and change the other.

#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

struct TileUniform {
    sun_direction: vec3<f32>,
    rim_strength: f32,
    terminator_softness: f32,
    sun_shading: f32,
    padding: vec2<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> tile: TileUniform;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var imagery_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var imagery_sampler: sampler;

const SUN_COLOR: vec3<f32> = vec3<f32>(1.0, 0.97, 0.92);
const SKY_COLOR: vec3<f32> = vec3<f32>(0.30, 0.55, 1.0);
const AMBIENT: vec3<f32> = vec3<f32>(0.020, 0.028, 0.045);
/// How much of the imagery still reads on the night side. Unlike the base
/// globe there are no city lights to take over, so the dark side is lifted
/// just enough to stay legible rather than going to black.
const NIGHT_FLOOR: f32 = 0.06;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let normal = normalize(in.world_normal);
    let to_view = normalize(view.world_position.xyz - in.world_position.xyz);
    let to_sun = normalize(tile.sun_direction);

    let albedo = textureSample(imagery_texture, imagery_sampler, in.uv);

    let sun_dot = dot(normal, to_sun);
    let terminator = smoothstep(-tile.terminator_softness, tile.terminator_softness, sun_dot);
    // Shading off lights the imagery flatly, which is the only way to read a
    // layer over ground that happens to be in darkness.
    let daylight = mix(1.0, terminator, tile.sun_shading);
    let diffuse = mix(1.0, max(sun_dot, 0.0), tile.sun_shading);

    var color = albedo.rgb * (AMBIENT + SUN_COLOR * diffuse);
    color = mix(albedo.rgb * NIGHT_FLOOR, color, daylight);

    let fresnel = pow(1.0 - clamp(dot(normal, to_view), 0.0, 1.0), 3.0);
    color += SKY_COLOR * fresnel * tile.rim_strength * daylight;

    return vec4<f32>(color, albedo.a);
}
