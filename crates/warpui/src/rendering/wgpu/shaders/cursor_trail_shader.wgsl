struct Uniforms {
    viewport_size: vec2<f32>,
    padding: vec2<f32>
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;

struct CursorTrailVertexShaderInput {
    @location(0) vertex_position: vec2<f32>,
    @location(1) top_left: vec2<f32>,
    @location(2) top_right: vec2<f32>,
    @location(3) bottom_right: vec2<f32>,
    @location(4) bottom_left: vec2<f32>,
    @location(5) cursor_bounds: vec4<f32>,
    @location(6) color: vec4<f32>,
};

struct CursorTrailVertexShaderOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) pixel_position: vec2<f32>,
    @location(1) cursor_bounds: vec4<f32>,
    @location(2) color: vec4<f32>,
};

@vertex
fn vs_main(in: CursorTrailVertexShaderInput) -> CursorTrailVertexShaderOutput {
    let top = mix(in.top_left, in.top_right, in.vertex_position.x);
    let bottom = mix(in.bottom_left, in.bottom_right, in.vertex_position.x);
    let pixel_position = mix(top, bottom, in.vertex_position.y);
    let device_position = pixel_position / uniforms.viewport_size * vec2(2.0, -2.0) + vec2(-1.0, 1.0);

    var out: CursorTrailVertexShaderOutput;
    out.position = vec4<f32>(device_position, 0.0, 1.0);
    out.pixel_position = pixel_position;
    out.cursor_bounds = in.cursor_bounds;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: CursorTrailVertexShaderOutput) -> @location(0) vec4<f32> {
    let cursor_min = in.cursor_bounds.xy;
    let cursor_max = in.cursor_bounds.xy + in.cursor_bounds.zw;
    let inside_cursor =
        in.pixel_position.x >= cursor_min.x &&
        in.pixel_position.x <= cursor_max.x &&
        in.pixel_position.y >= cursor_min.y &&
        in.pixel_position.y <= cursor_max.y;

    var color = in.color;
    if inside_cursor {
        color.a = 0.0;
    }
    return color;
}
