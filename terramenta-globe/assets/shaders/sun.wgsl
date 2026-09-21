// The Sun's surface: procedural granulation and limb darkening, animated as
// slow convective turbulence rather than a flat emissive disc. Unlit — the
// Sun lights itself, so nothing here reads from Bevy's own lighting.

#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

struct SunUniform {
    time: f32,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> sun: SunUniform;

fn hash(p: vec3<f32>) -> f32 {
    var p3 = fract(p * 0.1031);
    p3 += dot(p3, p3.zyx + 31.32);
    return fract((p3.x + p3.y) * p3.z);
}

// Trilinearly-interpolated value noise — cheap and seamless across a sphere
// since it is sampled by direction rather than by UV.
fn noise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);

    let x00 = mix(hash(i + vec3<f32>(0.0, 0.0, 0.0)), hash(i + vec3<f32>(1.0, 0.0, 0.0)), u.x);
    let x10 = mix(hash(i + vec3<f32>(0.0, 1.0, 0.0)), hash(i + vec3<f32>(1.0, 1.0, 0.0)), u.x);
    let x01 = mix(hash(i + vec3<f32>(0.0, 0.0, 1.0)), hash(i + vec3<f32>(1.0, 0.0, 1.0)), u.x);
    let x11 = mix(hash(i + vec3<f32>(0.0, 1.0, 1.0)), hash(i + vec3<f32>(1.0, 1.0, 1.0)), u.x);
    let y0 = mix(x00, x10, u.y);
    let y1 = mix(x01, x11, u.y);
    return mix(y0, y1, u.z);
}

fn fbm(p: vec3<f32>) -> f32 {
    var value = 0.0;
    var amplitude = 0.5;
    var freq = p;
    for (var i = 0; i < 5; i += 1) {
        value += amplitude * noise(freq);
        freq *= 2.02;
        amplitude *= 0.5;
    }
    return value;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // The mesh is a sphere with no rotation or scale, so its world normal is
    // already the direction to sample the surface noise by.
    let direction = normalize(in.world_normal);
    let drift = vec3<f32>(sun.time * 0.025, sun.time * 0.017, 0.0);

    let large_cells = fbm(direction * 3.2 + drift);
    let fine_grain = fbm(direction * 8.5 - drift * 1.6 + vec3<f32>(5.2, 1.3, 9.1));
    let turbulence = mix(large_cells, fine_grain, 0.4);

    let granulation = smoothstep(0.32, 0.78, turbulence);
    let deep = vec3<f32>(0.75, 0.14, 0.02);
    let bright = vec3<f32>(1.55, 1.05, 0.55);
    let surface = mix(deep, bright, granulation);

    // Occasional brighter flecks — small flares riding on the granulation.
    let flare = pow(clamp(fbm(direction * 11.0 + drift * 3.0), 0.0, 1.0), 7.0) * 3.0;

    // Real limb darkening: the disc dims toward the edge, where the sightline
    // only reaches cooler, higher layers of the photosphere.
    let to_view = normalize(view.world_position.xyz - in.world_position.xyz);
    let mu = clamp(dot(direction, to_view), 0.0, 1.0);
    let limb = mix(0.32, 1.0, pow(mu, 0.4));

    let color = surface * limb + vec3<f32>(1.3, 0.95, 0.55) * flare;
    return vec4<f32>(color, 1.0);
}
