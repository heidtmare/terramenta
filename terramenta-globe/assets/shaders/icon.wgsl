// A placemark: an image pinned to one point on the globe and drawn at a fixed
// size on screen.
//
// The same trick the overlay geometry uses, for the same reason — see
// `vector.wgsl`. The mesh is one quad with all four corners on the anchor, and
// they are spread here in the plane of the screen by however much world a
// device pixel covers at that depth, so a 32 px icon is 32 px across from orbit
// and from a low pass alike.
//
// Unlit, like everything else drawn over the surface: an icon on the night side
// has to stay as readable as the same icon at noon.

#import bevy_pbr::forward_io::{Vertex, VertexOutput}
#import bevy_pbr::mesh_functions
#import bevy_pbr::mesh_view_bindings::view
#import bevy_pbr::view_transformations::position_world_to_clip

struct IconUniform {
    // Multiplied into the texture. Linear, straight alpha.
    tint: vec4<f32>,
    // Half the icon's size on screen, in device pixels.
    size_px: f32,
    padding: vec3<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> icon: IconUniform;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var icon_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var icon_sampler: sampler;

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

    // Spread over the view's own axes, so the icon faces the camera and stays
    // upright however the globe under it is turned.
    let extent = world_per_pixel(world_position.xyz) * icon.size_px;
    let right = view.world_from_view[0].xyz;
    let up = view.world_from_view[1].xyz;
    world_position = vec4<f32>(
        world_position.xyz + (right * vertex.uv.x + up * vertex.uv.y) * extent,
        1.0,
    );

    out.world_position = world_position;
    out.position = position_world_to_clip(world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        vertex.instance_index,
    );
#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_TANGENTS
    out.world_tangent = vertex.tangent;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // The quad's corners run -1 to 1 either way, and the vertical axis points
    // up the screen where a texture's runs down it.
    let texcoord = vec2<f32>(in.uv.x * 0.5 + 0.5, 0.5 - in.uv.y * 0.5);
    let texel = textureSample(icon_texture, icon_sampler, texcoord);
    return vec4<f32>(texel.rgb * icon.tint.rgb, texel.a * icon.tint.a);
}
