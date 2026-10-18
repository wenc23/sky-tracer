// Temporal resolve pass for the volumetric cloud trace buffer.
//
// The workbench camera only rotates (no translation), so cloud radiance can be
// reprojected between frames from ray direction alone: rotating the current
// world-space ray into the previous camera basis is exact for a static camera
// and any cloud distance. The previous frame is clamped to the current 3x3
// neighbourhood min/max before blending, which keeps fast cloud advection and
// sun/overcast changes from smearing across the image. History is invalidated
// by the renderer on resize, large camera/sun jumps and the first frame.
//
// This is a lightweight TAA for clouds: it replaces per-pixel supersampling
// with temporal accumulation (see Toft et al. 2016) and leaves the composite
// pass to upsample the resolved trace buffer.

struct Temporal {
    camera_forward: vec4<f32>,
    camera_right: vec4<f32>,
    camera_up: vec4<f32>,
    prev_forward: vec4<f32>,
    prev_right: vec4<f32>,
    prev_up: vec4<f32>,
    // x = tan(fov_y/2), y = aspect, z = history_valid, w = blend alpha
    data: vec4<f32>,
};

@group(0) @binding(0) var raw: texture_2d<f32>;
@group(0) @binding(1) var history: texture_2d<f32>;
@group(0) @binding(2) var resolved: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> t: Temporal;

fn load_history(pixel: vec2<i32>) -> vec4<f32> {
    let dims = textureDimensions(history);
    return textureLoad(
        history,
        clamp(pixel, vec2<i32>(0), vec2<i32>(i32(dims.x) - 1, i32(dims.y) - 1)),
        0
    );
}

/// Bilinear history fetch in trace-pixel coordinates (Rgba32Float is not
/// filterable, so this is done manually).
fn sample_history(pixel: vec2<f32>) -> vec4<f32> {
    let base = vec2<i32>(floor(pixel));
    let frac = pixel - vec2<f32>(base);
    let c00 = load_history(base);
    let c10 = load_history(base + vec2<i32>(1, 0));
    let c01 = load_history(base + vec2<i32>(0, 1));
    let c11 = load_history(base + vec2<i32>(1, 1));
    return mix(
        mix(c00, c10, frac.x),
        mix(c01, c11, frac.x),
        frac.y
    );
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(resolved);
    if (id.x >= dims.x || id.y >= dims.y) {
        return;
    }
    let coord = vec2<i32>(id.xy);
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims) * 2.0 - 1.0;
    let tan_half = t.data.x;
    let aspect = t.data.y;
    let direction = normalize(
        t.camera_forward.xyz
        + t.camera_right.xyz * (uv.x * tan_half * aspect)
        - t.camera_up.xyz * (uv.y * tan_half)
    );

    var previous_pixel = vec2<f32>(f32(id.x), f32(id.y));
    let forward_dot = dot(direction, t.prev_forward.xyz);
    if (forward_dot > 1.0e-4) {
        let x = dot(direction, t.prev_right.xyz) / forward_dot;
        let y = dot(direction, t.prev_up.xyz) / forward_dot;
        let ndc = vec2<f32>(x / (tan_half * aspect), y / tan_half);
        previous_pixel = (ndc * 0.5 + 0.5) * vec2<f32>(dims) - vec2<f32>(0.5);
    }

    let current = textureLoad(raw, coord, 0);

    // Current-frame neighbourhood bounds; clamping history to these bounds
    // rejects stale radiance after fast cloud motion or lighting changes.
    var lo = current.rgb;
    var hi = current.rgb;
    var lo_t = current.a;
    var hi_t = current.a;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let c = textureLoad(
                raw,
                clamp(coord + vec2<i32>(dx, dy), vec2<i32>(0), vec2<i32>(i32(dims.x) - 1, i32(dims.y) - 1)),
                0
            );
            lo = min(lo, c.rgb);
            hi = max(hi, c.rgb);
            lo_t = min(lo_t, c.a);
            hi_t = max(hi_t, c.a);
        }
    }

    let history_sample = sample_history(previous_pixel);
    let history_rgb = clamp(history_sample.rgb, lo, hi);
    let history_t = clamp(history_sample.a, lo_t, hi_t);
    let alpha = select(1.0, clamp(t.data.w, 0.0, 1.0), t.data.z > 0.5);
    let out_rgb = mix(history_rgb, current.rgb, alpha);
    let out_t = mix(history_t, current.a, alpha);
    textureStore(resolved, coord, vec4<f32>(out_rgb, out_t));
}
