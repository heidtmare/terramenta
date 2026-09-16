// The Earth's surface: a day/night blend driven by a single sun direction,
// plus city lights, an ocean specular highlight and a drifting cloud deck.
//
// Lighting the globe here rather than through Bevy's PBR pipeline keeps the
// whole planet to one draw call and makes the terminator directly art-directable.

#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

struct GlobeUniform {
    sun_direction: vec3<f32>,
    cloud_offset: f32,
    night_intensity: f32,
    cloud_opacity: f32,
    rim_strength: f32,
    terminator_softness: f32,
    sun_shading: f32,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> globe: GlobeUniform;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var day_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var day_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var night_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var night_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var cloud_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(6) var cloud_sampler: sampler;

const SUN_COLOR: vec3<f32> = vec3<f32>(1.0, 0.97, 0.92);
const SKY_COLOR: vec3<f32> = vec3<f32>(0.30, 0.55, 1.0);
// What the unlit side still receives from starlight and airglow.
const AMBIENT: vec3<f32> = vec3<f32>(0.020, 0.028, 0.045);

// The Blue Marble imagery has no water mask, so infer one: oceans are the only
// large regions where blue dominates the other two channels.
fn ocean_mask(albedo: vec3<f32>) -> f32 {
    let blueness = albedo.b - max(albedo.r, albedo.g);
    return smoothstep(0.0, 0.10, blueness);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let normal = normalize(in.world_normal);
    let to_view = normalize(view.world_position.xyz - in.world_position.xyz);
    let to_sun = normalize(globe.sun_direction);

    let sun_dot = dot(normal, to_sun);
    // A soft terminator: the sun is a disc, not a point, and the atmosphere
    // scatters light some way past the geometric edge.
    let terminator = smoothstep(-globe.terminator_softness, globe.terminator_softness, sun_dot);
    // With shading turned off every face is treated as though the sun were
    // straight above it: no terminator, no night side, and so no city lights.
    let daylight = mix(1.0, terminator, globe.sun_shading);

    let albedo = textureSample(day_texture, day_sampler, in.uv).rgb;
    let city_lights = textureSample(night_texture, night_sampler, in.uv).rgb;
    // The sampler repeats horizontally, so let the coordinate run past 1.0
    // rather than wrapping it here — a `fract` would tear the derivative and
    // leave a seam down the antimeridian.
    let clouds = textureSample(
        cloud_texture,
        cloud_sampler,
        vec2<f32>(in.uv.x + globe.cloud_offset, in.uv.y),
    ).r;

    // Diffuse term, wrapped slightly so the low-angle light near the terminator
    // stays warm instead of falling off a cliff.
    let diffuse = mix(1.0, max(sun_dot, 0.0), globe.sun_shading);
    var color = albedo * (AMBIENT + SUN_COLOR * diffuse);

    // Sun glint off the water, only where it can actually be seen.
    let halfway = normalize(to_sun + to_view);
    let specular = pow(max(dot(normal, halfway), 0.0), 90.0)
        * ocean_mask(albedo)
        * daylight;
    color += SUN_COLOR * specular * 0.55;

    // City lights fade in as daylight fades out, and are dimmed under cloud.
    let night_side = 1.0 - daylight;
    color += city_lights * globe.night_intensity * night_side * (1.0 - clouds * 0.75);

    // Clouds catch the sun a little more strongly than the ground does.
    let cloud_shade = clouds * globe.cloud_opacity * daylight;
    color = mix(color, SUN_COLOR * (0.08 + 0.95 * diffuse), cloud_shade);

    // Air seen at a grazing angle is thicker, so the lit limb turns blue.
    let fresnel = pow(1.0 - clamp(dot(normal, to_view), 0.0, 1.0), 3.0);
    color += SKY_COLOR * fresnel * globe.rim_strength * daylight;

    return vec4<f32>(color, 1.0);
}
