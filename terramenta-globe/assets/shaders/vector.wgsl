// GeoJSON overlay geometry: markers, lines and filled rings.
//
// One shader for all three, because they differ only in how a vertex is moved
// before it is projected — and they have to agree exactly on how wide a pixel
// is, or a line would not meet the marker at its end.
//
// The point of doing any of this on the GPU is that a marker and a line are
// measured in *pixels*, not in ground distance. A dot 6 px across is legible
// from anywhere, where a dot 6 km across is a continent from orbit and
// invisible from a low pass; the same goes for a 2 px track. So the mesh holds
// one anchor per marker and one spine per line, and the corners are pushed out
// from there in the plane of the screen, by a distance worked back from how
// much world one pixel covers at that depth.
//
// The overlay is drawn unlit. It is annotation rather than imagery: a track
// across the night side has to stay as readable as the same track at noon.
//
// Colour and size come from the material, one draw at a time, except where a
// GeoJSON document styled its own features (simplestyle-spec 1.1.0) — then the
// mesh carries a colour and a size per vertex as well, and each vertex takes
// whichever of the two its own feature asked for. A vertex whose feature asked
// for nothing carries a negative alpha and a negative size, neither of which is
// a value anything could otherwise mean, and falls back to the material. That
// is what lets one draw hold a layer where nine hundred rings follow the
// interface's colour and one is the red the document asked for — and what lets
// recolouring the layer move the nine hundred without touching the red one.
//
// The two attributes are either on every vertex of a mesh or on none of it, so
// a layer that needs neither is compiled without them and its vertices stay the
// size they were.

#import bevy_pbr::forward_io::{Vertex, VertexOutput}
#import bevy_pbr::mesh_functions
#import bevy_pbr::mesh_view_bindings::view
#import bevy_pbr::view_transformations::position_world_to_clip

struct VectorUniform {
    // Linear, premultiplied by nothing: alpha is applied at the end.
    color: vec4<f32>,
    // Marker radius, or half a line's width. Device pixels.
    size_px: f32,
    mode: u32,
    padding: vec2<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> vector: VectorUniform;

const MODE_MARKER: u32 = 0u;
const MODE_LINE: u32 = 1u;
const MODE_FILL: u32 = 2u;

/// The colour a vertex is drawn in: its own, or the material's where it has
/// none of its own to give.
fn resolve_color(own: vec4<f32>) -> vec4<f32> {
    return select(vector.color, own, own.a >= 0.0);
}

/// The same for a size. Halved, because the mesh spreads each corner one unit
/// either way and the shader works in radii — which the uniform's own value was
/// halved for before it was written.
fn resolve_size(own: f32) -> f32 {
    return select(vector.size_px, max(own * 0.5, 0.1), own >= 0.0);
}

// Where a marker's dark rim starts and ends, as a fraction of its radius.
const RIM_INNER: f32 = 0.60;
const RIM_OUTER: f32 = 0.86;

/// How much world one device pixel covers at a world position, under this view.
///
/// The viewport is `2 * z * tan(fov / 2)` across at depth `z`, and
/// `clip_from_view[1][1]` is `1 / tan(fov / 2)` for a perspective projection —
/// so the two cancel into a single division.
fn world_per_pixel(world_position: vec3<f32>) -> f32 {
    let depth = -(view.view_from_world * vec4<f32>(world_position, 1.0)).z;
    return 2.0 * max(depth, 1.0e-6) / max(view.clip_from_view[1][1] * view.viewport.w, 1.0e-6);
}

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;

    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    var world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(vertex.position, 1.0),
    );

    var size_px = vector.size_px;
#ifdef VERTEX_UVS_B
    size_px = resolve_size(vertex.uv_b.x);
#endif
    let extent = world_per_pixel(world_position.xyz) * size_px;

    if vector.mode == MODE_MARKER {
        // A disc facing the camera, spread over the view's own axes so it stays
        // circular however the globe under it is turned.
        let right = view.world_from_view[0].xyz;
        let up = view.world_from_view[1].xyz;
        world_position = vec4<f32>(
            world_position.xyz + (right * vertex.uv.x + up * vertex.uv.y) * extent,
            1.0,
        );
    } else if vector.mode == MODE_LINE {
        // A ribbon: the spine carries the direction the line is running in, and
        // each vertex steps off it to whichever side `tangent.w` names. Across
        // the line of sight, so the ribbon turns to face the camera as the
        // globe rotates and never collapses to nothing.
        let along = (world_from_local * vec4<f32>(vertex.tangent.xyz, 0.0)).xyz;
        let to_view = view.world_position.xyz - world_position.xyz;
        let offset = cross(along, to_view);
        let length_squared = dot(offset, offset);
        if length_squared > 1.0e-12 {
            world_position = vec4<f32>(
                world_position.xyz + offset * inverseSqrt(length_squared) * vertex.tangent.w * extent,
                1.0,
            );
        }
    }

    out.world_position = world_position;
    out.position = position_world_to_clip(world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        vertex.instance_index,
    );
#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = vertex.uv_b;
#endif
#ifdef VERTEX_TANGENTS
    out.world_tangent = vertex.tangent;
#endif
#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    var painted = vector.color;
#ifdef VERTEX_COLORS
    painted = resolve_color(in.color);
#endif
    var size_px = vector.size_px;
#ifdef VERTEX_UVS_B
    size_px = resolve_size(in.uv_b.x);
#endif

    var color = painted.rgb;
    var alpha = painted.a;

    // One device pixel, in the units the quad's own coordinates are measured
    // in, which is what the edge is softened over. Below a pixel or so across
    // the feathering would eat the whole shape, so it is capped.
    let feather = clamp(1.0 / max(size_px, 1.0e-3), 0.0, 0.5);

    if vector.mode == MODE_MARKER {
        let radius = length(in.uv);
        alpha *= 1.0 - smoothstep(1.0 - feather, 1.0, radius);
        // A dark rim, so a marker still reads as a marker over bright desert or
        // cloud, where a flat dot of colour would wash out.
        color = mix(color, color * 0.25, smoothstep(RIM_INNER, RIM_OUTER, radius));
    } else if vector.mode == MODE_LINE {
        // `uv.x` runs -1 to 1 across the ribbon.
        alpha *= 1.0 - smoothstep(1.0 - feather, 1.0, abs(in.uv.x));
    }

    return vec4<f32>(color, alpha);
}
