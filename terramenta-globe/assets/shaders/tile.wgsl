// A single WMS imagery tile on the globe's surface.
//
// Reproduces the sunlight, ocean glint and city-light terms from `globe.wgsl`.
// City lights come from the base globe's night texture, sampled by geographic
// position since the tile's own UV covers only the tile. Constants match
// `globe.wgsl`; change one and change the other.

#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

struct TileUniform {
    sun_direction: vec3<f32>,
    rim_strength: f32,
    terminator_softness: f32,
    sun_shading: f32,
    night_intensity: f32,
    /// Earth-fixed → world rotation about +Y, radians.
    earth_yaw: f32,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> tile: TileUniform;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var imagery_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var imagery_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var night_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var night_sampler: sampler;

const SUN_COLOR: vec3<f32> = vec3<f32>(1.0, 0.97, 0.92);
const SKY_COLOR: vec3<f32> = vec3<f32>(0.30, 0.55, 1.0);
const AMBIENT: vec3<f32> = vec3<f32>(0.020, 0.028, 0.045);
/// How much of the imagery still reads on the night side. Unlike the base
/// globe there are no city lights to take over, so the dark side is lifted
/// just enough to stay legible rather than going to black.
const NIGHT_FLOOR: f32 = 0.06;
const PI: f32 = 3.14159265;

fn ocean_mask(albedo: vec3<f32>) -> f32 {
    let blueness = albedo.b - max(albedo.r, albedo.g);
    return smoothstep(0.0, 0.10, blueness);
}

/// Equirectangular UV of a world-space normal, matching `equirectangular_sphere`.
fn geographic_uv(normal: vec3<f32>) -> vec2<f32> {
    let lon = atan2(normal.x, normal.z) - tile.earth_yaw;
    let lat = asin(clamp(normal.y, -1.0, 1.0));
    return vec2<f32>(fract((lon + PI) / (2.0 * PI)), 0.5 - lat / PI);
}

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

    let halfway = normalize(to_sun + to_view);
    let specular = pow(max(dot(normal, halfway), 0.0), 90.0) * ocean_mask(albedo.rgb) * daylight;
    color += SUN_COLOR * specular * 0.55;

    color = mix(albedo.rgb * NIGHT_FLOOR, color, daylight);

    // Explicit level: the `fract` seam would otherwise spike the derivative.
    let city_lights = textureSampleLevel(night_texture, night_sampler, geographic_uv(normal), 0.0).rgb;
    color += city_lights * tile.night_intensity * (1.0 - daylight);

    let fresnel = pow(1.0 - clamp(dot(normal, to_view), 0.0, 1.0), 3.0);
    color += SKY_COLOR * fresnel * tile.rim_strength * daylight;

    return vec4<f32>(color, albedo.a);
}
