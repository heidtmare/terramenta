// A procedural sky drawn on the inside of a very large sphere.
//
// Stars come from a hashed 3D cell grid: each cell may hold one star at a
// jittered position, which scatters them evenly without a texture and without
// the pinching a UV-mapped star texture would show at the poles.

#import bevy_pbr::forward_io::VertexOutput

struct StarfieldUniform {
    time: f32,
    // Radians the sky is turned through about the poles. The stars are
    // inertial, so this turns them with the Earth-fixed frame and leaves them
    // alone in the inertial one.
    rotation: f32,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> starfield: StarfieldUniform;

const CELLS: f32 = 240.0;
// Fraction of cells that contain a star.
const STAR_DENSITY: f32 = 0.055;

fn hash3(cell: vec3<f32>) -> vec3<f32> {
    var p = vec3<f32>(
        dot(cell, vec3<f32>(127.1, 311.7, 74.7)),
        dot(cell, vec3<f32>(269.5, 183.3, 246.1)),
        dot(cell, vec3<f32>(113.5, 271.9, 124.6)),
    );
    p = fract(sin(p) * 43758.5453);
    return p;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Sample the sky in its own inertial frame: undo the rotation the world is
    // being drawn with, rather than turning the mesh, which a sphere would not
    // notice.
    let world_direction = normalize(in.world_position.xyz);
    let sin_rotation = sin(starfield.rotation);
    let cos_rotation = cos(starfield.rotation);
    let direction = vec3<f32>(
        world_direction.x * cos_rotation - world_direction.z * sin_rotation,
        world_direction.y,
        world_direction.x * sin_rotation + world_direction.z * cos_rotation,
    );
    let scaled = direction * CELLS;
    let cell = floor(scaled);
    let local = fract(scaled);

    var color = vec3<f32>(0.0);

    // Check the neighbouring cells too, so a star near a boundary is not clipped.
    for (var dx = -1; dx <= 1; dx += 1) {
        for (var dy = -1; dy <= 1; dy += 1) {
            for (var dz = -1; dz <= 1; dz += 1) {
                let offset = vec3<f32>(f32(dx), f32(dy), f32(dz));
                let random = hash3(cell + offset);
                if (random.z > STAR_DENSITY) {
                    continue;
                }

                // Rarer cells hold brighter stars.
                let brightness = pow(1.0 - random.z / STAR_DENSITY, 2.5);
                let star_position = offset + vec3<f32>(random.x, random.y, fract(random.z * 91.7));
                let distance = length(local - star_position);

                let core = exp(-distance * distance * 5000.0);
                let halo = exp(-distance * distance * 400.0) * 0.12;
                let twinkle = 0.85 + 0.15 * sin(starfield.time * 2.2 + random.x * 100.0);

                // Blue-white through to amber, keyed off the same hash.
                let tint = mix(
                    vec3<f32>(0.75, 0.84, 1.0),
                    vec3<f32>(1.0, 0.88, 0.72),
                    random.y,
                );
                color += tint * (core + halo) * brightness * twinkle;
            }
        }
    }

    // A faint galactic band, tilted away from the ecliptic.
    let band_axis = normalize(vec3<f32>(0.35, 0.86, -0.37));
    let band = pow(1.0 - abs(dot(direction, band_axis)), 14.0);
    color += vec3<f32>(0.05, 0.06, 0.09) * band;

    return vec4<f32>(color, 1.0);
}
