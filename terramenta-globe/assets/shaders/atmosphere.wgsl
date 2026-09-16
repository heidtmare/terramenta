// A thin additive shell standing in for Rayleigh scattering.
//
// The shell is drawn back-face-only with depth writes off, so it accumulates
// over the planet and over empty space alike: dense at the limb where the line
// of sight passes through the most air, invisible where you look straight down.

#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

struct AtmosphereUniform {
    sun_direction: vec3<f32>,
    density: f32,
    color: vec3<f32>,
    falloff: f32,
    sun_shading: f32,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> atmosphere: AtmosphereUniform;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let normal = normalize(in.world_normal);
    let to_view = normalize(view.world_position.xyz - in.world_position.xyz);
    let to_sun = normalize(atmosphere.sun_direction);

    // Path length through the shell, approximated by how edge-on we see it.
    let grazing = 1.0 - clamp(dot(normal, to_view), 0.0, 1.0);
    let thickness = pow(grazing, atmosphere.falloff);

    // Only air that the sun reaches glows, with a soft wrap so the glow tapers
    // past the terminator into a thin twilight arc instead of ending abruptly.
    let sun_dot = dot(normal, to_sun);
    // Follows the surface: with shading off the whole limb glows, rather than
    // leaving a terminator hanging in the air over a globe that has none.
    let lit = mix(1.0, smoothstep(-0.45, 0.25, sun_dot), atmosphere.sun_shading);

    // Forward scattering: looking toward the sun through the limb is brightest.
    let forward = pow(clamp(dot(to_view, -to_sun) * 0.5 + 0.5, 0.0, 1.0), 2.0);

    let intensity = thickness * lit * atmosphere.density * (0.55 + 0.85 * forward);
    return vec4<f32>(atmosphere.color * intensity, 1.0);
}
