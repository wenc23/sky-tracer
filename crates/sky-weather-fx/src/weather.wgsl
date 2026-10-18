// Behaviour-first, stateless screen-space weather effects.
//
// This shader is a single full-screen compute pass. It reads the application's
// sky background with textureLoad and writes a linear Rec.2020 Rgba32Float
// target. Everything else is procedural: rain/sleet streaks, snow/hail flakes,
// fog/haze/dust, cloud darkening, a rainbow and a jagged lightning bolt.
//
// The all-zero weather case has an explicit short-circuit that copies the
// background exactly; noise never reaches the background in that path.
//
// Conventions: screen pixel coordinates are `(x + 0.5, y + 0.5)` with y pointing
// down. `params.camera_origin.w` carries `time_s`. Wind is a horizontal vector in
// application units; positive x pushes precipitation to screen right.

const PI: f32 = 3.141592653589793;
const TAU: f32 = 6.283185307179586;
const RAINBOW_PRIMARY_ANGLE: f32 = 0.7330383; // 42.0 degrees
const RAINBOW_SECONDARY_ANGLE: f32 = 0.8901179; // 51.0 degrees
const RAINBOW_WIDTH: f32 = 0.030;
const BOLT_MAX_SEGMENTS: u32 = 16u;

struct Params {
    // xyz world origin in metres; w elapsed time in seconds.
    camera_origin: vec4<f32>,
    // xyz unit forward; w tan(fov_y / 2).
    camera_forward: vec4<f32>,
    camera_right: vec4<f32>,
    camera_up: vec4<f32>,
    // xyz unit direction toward the sun.
    sun_direction: vec4<f32>,
    // xy wind; zw unused.
    wind: vec4<f32>,
    // rain, snow, hail, sleet intensities in [0, 1].
    weather: vec4<f32>,
    // fog, dust, cloud_darkening, rainbow in [0, 1].
    atmosphere: vec4<f32>,
    // x flash envelope in [0, 1]; yzw unused.
    lightning: vec4<f32>,
    // rain, snow, hail, sleet layer counts.
    counts: vec4<u32>,
    // x width, y height, z bolt segments, w lightning seed.
    viewport: vec4<u32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var background_texture: texture_2d<f32>;
@group(0) @binding(2) var output_texture: texture_storage_2d<rgba32float, write>;

fn hash_u32(value: u32) -> u32 {
    var h = value;
    h = (h ^ (h >> 16u)) * 0x7feb352du;
    h = (h ^ (h >> 15u)) * 0x846ca68bu;
    return h ^ (h >> 16u);
}

fn hash_unit(value: u32) -> f32 {
    return f32(value >> 8u) * (1.0 / 16777216.0);
}

fn hash2(seed: u32, a: u32, b: u32) -> f32 {
    var h = seed;
    h = h ^ hash_u32(a);
    h = h ^ hash_u32(b);
    return hash_unit(hash_u32(h));
}

fn hash2v(seed: u32, a: u32, b: u32) -> vec2<f32> {
    return vec2<f32>(
        hash2(seed, a, b),
        hash2(seed ^ 0x9e3779b9u, a, b + 0x85ebca6bu),
    );
}

fn cell_index(value: f32) -> u32 {
    return u32(i32(floor(value)));
}

// Smooth deterministic 2D value noise in [0, 1]; no texture is used.
fn haze_noise(p: vec2<f32>) -> f32 {
    let cell = vec2<i32>(i32(floor(p.x)), i32(floor(p.y)));
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash2(0x51ed270bu, u32(cell.x), u32(cell.y));
    let b = hash2(0x51ed270bu, u32(cell.x + 1), u32(cell.y));
    let c = hash2(0x51ed270bu, u32(cell.x), u32(cell.y + 1));
    let d = hash2(0x51ed270bu, u32(cell.x + 1), u32(cell.y + 1));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn camera_ray(pixel: vec2<f32>) -> vec3<f32> {
    let viewport = vec2<f32>(params.viewport.xy);
    let uv = pixel / viewport * 2.0 - vec2<f32>(1.0);
    let forward = params.camera_forward.xyz;
    let right = params.camera_right.xyz;
    let up = params.camera_up.xyz;
    let tan_half = params.camera_forward.w;
    let aspect = viewport.x / viewport.y;
    return normalize(forward + right * (uv.x * tan_half * aspect) - up * (uv.y * tan_half));
}

// Exact texel copy when the background matches the viewport, otherwise a
// clamped bilinear resample. The exact path keeps the zero-weather output
// bit-identical to the background.
fn sample_background(pixel: vec2<u32>) -> vec3<f32> {
    let size = vec2<i32>(textureDimensions(background_texture));
    let full = vec2<i32>(params.viewport.xy);
    if (size.x == full.x && size.y == full.y) {
        return textureLoad(background_texture, vec2<i32>(pixel), 0).xyz;
    }
    let uv = (vec2<f32>(pixel) + vec2<f32>(0.5)) / vec2<f32>(params.viewport.xy);
    let coordinate = uv * vec2<f32>(size) - vec2<f32>(0.5);
    let base = vec2<i32>(floor(coordinate));
    let fraction = fract(coordinate);
    let x0 = clamp(base.x, 0, size.x - 1);
    let x1 = clamp(base.x + 1, 0, size.x - 1);
    let y0 = clamp(base.y, 0, size.y - 1);
    let y1 = clamp(base.y + 1, 0, size.y - 1);
    let a = textureLoad(background_texture, vec2<i32>(x0, y0), 0).xyz;
    let b = textureLoad(background_texture, vec2<i32>(x1, y0), 0).xyz;
    let c = textureLoad(background_texture, vec2<i32>(x0, y1), 0).xyz;
    let d = textureLoad(background_texture, vec2<i32>(x1, y1), 0).xyz;
    let row0 = a + (b - a) * fraction.x;
    let row1 = c + (d - c) * fraction.x;
    return row0 + (row1 - row0) * fraction.y;
}

fn weather_is_zero() -> bool {
    return params.weather.x == 0.0
        && params.weather.y == 0.0
        && params.weather.z == 0.0
        && params.weather.w == 0.0
        && params.atmosphere.x == 0.0
        && params.atmosphere.y == 0.0
        && params.atmosphere.z == 0.0
        && params.atmosphere.w == 0.0
        && params.lightning.x == 0.0;
}

fn sun_up() -> f32 {
    return clamp(params.sun_direction.y * 2.0, 0.0, 1.0);
}

// Procedural cloud-shadow darkening of the background. This is a stylised
// screen-space pattern, not a transported shadow.
fn cloud_shadow_factor(ray: vec3<f32>) -> f32 {
    let amount = clamp(params.atmosphere.z, 0.0, 1.0);
    if (amount <= 0.0) {
        return 1.0;
    }
    let time = params.camera_origin.w;
    let denominator = max(abs(ray.y) + 0.08, 0.08);
    var p = ray.xz / denominator;
    p = clamp(p, vec2<f32>(-64.0), vec2<f32>(64.0));
    p = p * 1.3 + params.camera_origin.xz * 0.004 + params.wind.xy * (time * 0.01);
    let detail =
        0.65 * haze_noise(p * 1.7 + vec2<f32>(time * 0.020, 0.0))
        + 0.35 * haze_noise(p * 4.3 - vec2<f32>(0.0, time * 0.015));
    return 1.0 - amount * (0.32 + 0.58 * smoothstep(0.25, 0.85, detail));
}

// Screen-space fog, haze and dust blend. Both terms thicken toward the horizon.
fn apply_haze(color: vec3<f32>, ray: vec3<f32>) -> vec3<f32> {
    let time = params.camera_origin.w;
    let fog_amount = clamp(params.atmosphere.x, 0.0, 1.0);
    let dust_amount = clamp(params.atmosphere.y, 0.0, 1.0);
    let elevation = clamp(ray.y, -0.4, 1.0);
    let horizon = 1.0 - smoothstep(0.02, 0.75, elevation);
    let lit = sun_up();
    let grain_position = clamp(
        ray.xz / max(abs(ray.y) + 0.25, 0.25) * 2.0
            + vec2<f32>(time * 0.03, -time * 0.02),
        vec2<f32>(-64.0),
        vec2<f32>(64.0),
    );
    let grain = haze_noise(grain_position);
    var result = color;
    if (fog_amount > 0.0) {
        let fog_color = mix(
            vec3<f32>(0.50, 0.55, 0.62),
            vec3<f32>(0.78, 0.78, 0.74),
            lit,
        );
        let fog_alpha = clamp(
            fog_amount * (0.18 + 0.82 * horizon) * (0.85 + 0.30 * grain),
            0.0,
            1.0,
        );
        result = mix(result, fog_color, fog_alpha);
    }
    if (dust_amount > 0.0) {
        let dust_color = mix(
            vec3<f32>(0.45, 0.36, 0.26),
            vec3<f32>(0.72, 0.58, 0.38),
            lit,
        );
        let dust_alpha = clamp(
            dust_amount * (0.15 + 0.85 * horizon) * (0.70 + 0.50 * grain),
            0.0,
            1.0,
        );
        result = mix(result, dust_color, dust_alpha * 0.8);
    }
    return result;
}

fn rainbow_spectrum(coordinate: f32) -> vec3<f32> {
    let t = clamp(coordinate, 0.0, 1.0);
    let blue = vec3<f32>(0.25, 0.40, 1.00);
    let green = vec3<f32>(0.35, 0.90, 0.45);
    let red = vec3<f32>(1.00, 0.35, 0.20);
    let low = mix(blue, green, smoothstep(0.0, 0.5, t));
    return mix(low, red, smoothstep(0.5, 1.0, t));
}

// Additive primary (42 degree) and secondary (51 degree) rainbow around the
// anti-solar direction, faded by solar elevation.
fn rainbow_radiance(ray: vec3<f32>) -> vec3<f32> {
    let intensity = clamp(params.atmosphere.w, 0.0, 1.0);
    if (intensity <= 0.0) {
        return vec3<f32>(0.0);
    }
    let visibility = smoothstep(-0.02, 0.15, params.sun_direction.y);
    if (visibility <= 0.0) {
        return vec3<f32>(0.0);
    }
    let anti_solar = -params.sun_direction.xyz;
    let angle = acos(clamp(dot(ray, anti_solar), -1.0, 1.0));
    let primary_offset = (angle - RAINBOW_PRIMARY_ANGLE) / RAINBOW_WIDTH;
    let primary_band = exp(-primary_offset * primary_offset);
    let spectrum_coordinate = clamp(primary_offset * 0.5 + 0.5, 0.0, 1.0);
    let primary = rainbow_spectrum(spectrum_coordinate) * primary_band * 0.55;
    let secondary_offset = (angle - RAINBOW_SECONDARY_ANGLE) / (RAINBOW_WIDTH * 1.6);
    let secondary_band = exp(-secondary_offset * secondary_offset);
    let secondary = rainbow_spectrum(1.0 - spectrum_coordinate) * secondary_band * 0.20;
    return (primary + secondary) * intensity * visibility;
}

fn segment_distance(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1.0e-6), 0.0, 1.0);
    return length(p - (a + ab * t));
}

// Rain and sleet share this slanted streak layer. Layer 0 is the far layer and
// the last layer is the near one; parallax scales speed, size and brightness.
fn streak_layer(
    pixel: vec2<f32>,
    layer: u32,
    layer_count: u32,
    seed: u32,
    intensity: f32,
    tint: vec3<f32>,
    speed_scale: f32,
    length_scale: f32,
) -> vec4<f32> {
    let layers = max(f32(layer_count), 1.0);
    let depth = (f32(layer) + 0.5) / layers;
    let time = params.camera_origin.w;
    let fall_speed = mix(700.0, 1900.0, depth) * speed_scale * (0.85 + 0.25 * intensity);
    let column_width = mix(72.0, 118.0, depth);
    let streak_length = mix(110.0, 230.0, depth) * length_scale;
    let slant = params.wind.x * (0.020 + 0.025 * depth);
    let direction = normalize(vec2<f32>(slant, 1.0));
    let perpendicular = vec2<f32>(-direction.y, direction.x);
    let along = dot(pixel, direction);
    let across = dot(pixel, perpendicular);
    let layer_seed = seed ^ (layer * 0x9e3779b9u);
    let column = cell_index(across / column_width);
    let column_hash = hash2(layer_seed, column, 1u);
    let scroll = along - time * fall_speed - column_hash * streak_length;
    let row = cell_index(scroll / streak_length);
    let row_hash = hash2(layer_seed, column, row + 1u);
    let presence = mix(0.86, 0.30, clamp(intensity, 0.0, 1.0));
    if (row_hash > presence) {
        return vec4<f32>(0.0);
    }
    let phase = fract(scroll / streak_length);
    let vertical = smoothstep(0.0, 0.05, phase) * (1.0 - smoothstep(0.55, 0.70, phase));
    let local_across = fract(across / column_width) - 0.5;
    let half_width = mix(0.010, 0.020, depth);
    let horizontal = 1.0 - smoothstep(half_width, half_width * 2.2, abs(local_across));
    let brightness = mix(0.45, 1.0, depth) * (0.60 + 0.40 * row_hash);
    let light = 0.45 + 0.55 * sun_up();
    let alpha = clamp(intensity * 0.75 * vertical * horizontal * brightness, 0.0, 1.0);
    return vec4<f32>(tint * light * brightness, alpha);
}

// Snow and hail share this flake layer. `stretch` below one elongates the flake
// along screen y (hail motion streaks). A 3x3 hashed-cell neighbourhood keeps
// flakes continuous across cell borders.
fn flake_layer(
    pixel: vec2<f32>,
    layer: u32,
    layer_count: u32,
    seed: u32,
    intensity: f32,
    grid_min: f32,
    grid_max: f32,
    speed_min: f32,
    speed_max: f32,
    radius_min: f32,
    radius_max: f32,
    stretch: f32,
    tint: vec3<f32>,
) -> vec4<f32> {
    let layers = max(f32(layer_count), 1.0);
    let depth = (f32(layer) + 0.5) / layers;
    let grid = mix(grid_min, grid_max, depth);
    let fall_speed = mix(speed_min, speed_max, depth);
    let time = params.camera_origin.w;
    let layer_seed = seed ^ (layer * 0x9e3779b9u);
    let drift = vec2<f32>(params.wind.x * (14.0 + 26.0 * depth), fall_speed);
    let p = (pixel - drift * time) / grid;
    let base = vec2<i32>(i32(floor(p.x)), i32(floor(p.y)));
    var nearest = 1.0e9;
    var nearest_hash = 0.0;
    for (var oy: i32 = -1; oy <= 1; oy = oy + 1) {
        for (var ox: i32 = -1; ox <= 1; ox = ox + 1) {
            let cell = base + vec2<i32>(ox, oy);
            let offset = hash2v(layer_seed, u32(cell.x), u32(cell.y));
            let delta = p - (vec2<f32>(cell) + offset);
            let d = length(vec2<f32>(delta.x, delta.y * stretch));
            if (d < nearest) {
                nearest = d;
                nearest_hash = hash2(layer_seed, u32(cell.x), u32(cell.y) + 91u);
            }
        }
    }
    let radius = mix(radius_min, radius_max, depth);
    let coverage = 1.0 - smoothstep(radius, radius * 1.9, nearest);
    let twinkle = 0.65 + 0.35 * sin(time * 2.0 + nearest_hash * TAU);
    let light = 0.50 + 0.50 * sun_up();
    let alpha = clamp(intensity * coverage * twinkle, 0.0, 1.0);
    return vec4<f32>(tint * light, alpha);
}

// Procedural jagged bolt. The polyline is rebuilt per pixel from the seed, so it
// is stable for a fixed seed and jumps when the application changes the seed.
fn lightning_bolt(pixel: vec2<f32>) -> vec3<f32> {
    let flash = clamp(params.lightning.x, 0.0, 1.0);
    let strength = smoothstep(0.20, 0.75, flash);
    if (strength <= 0.0) {
        return vec3<f32>(0.0);
    }
    let seed = params.viewport.w;
    let width = f32(params.viewport.x);
    let height = f32(params.viewport.y);
    let segments = min(params.viewport.z, BOLT_MAX_SEGMENTS);
    let top_x = width * (0.15 + 0.70 * hash2(seed, 1u, 2u));
    let bottom_y = height * (0.50 + 0.30 * hash2(seed, 3u, 4u));
    var previous = vec2<f32>(top_x, 0.0);
    var x = top_x;
    var nearest = 1.0e9;
    for (var i: u32 = 1u; i <= segments; i = i + 1u) {
        let t = f32(i) / f32(segments);
        x = x + (hash2(seed, i, 5u) - 0.5) * width * 0.08 * (1.0 - 0.35 * t);
        let current = vec2<f32>(x, t * bottom_y);
        nearest = min(nearest, segment_distance(pixel, previous, current));
        previous = current;
    }
    // One short branch from a mid-height node, reconstructed approximately from
    // the same seed; exact continuity is not required for a stylised bolt.
    let branch_t = 0.35 + 0.30 * hash2(seed, 7u, 8u);
    var branch_previous = vec2<f32>(
        top_x + (hash2(seed, 13u, 14u) - 0.5) * width * 0.15,
        branch_t * bottom_y,
    );
    let branch_side = select(-1.0, 1.0, hash2(seed, 9u, 10u) > 0.5);
    for (var j: u32 = 1u; j <= 4u; j = j + 1u) {
        let t = f32(j) / 4.0;
        let branch_x = branch_previous.x
            + branch_side * width * 0.020 * (0.5 + hash2(seed, j + 20u, 11u))
            + (hash2(seed, j + 30u, 12u) - 0.5) * width * 0.03;
        let branch_point = vec2<f32>(branch_x, branch_t * bottom_y + t * height * 0.18);
        nearest = min(nearest, segment_distance(pixel, branch_previous, branch_point));
        branch_previous = branch_point;
    }
    let core = 1.0 - smoothstep(0.6, 2.4, nearest);
    let glow = exp(-nearest * nearest / 40.0);
    let color =
        vec3<f32>(1.00, 0.96, 0.85) * core * 1.8
        + vec3<f32>(0.45, 0.58, 1.00) * glow * 0.7;
    return color * strength;
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (any(id.xy >= params.viewport.xy)) {
        return;
    }
    let pixel_index = vec2<i32>(id.xy);
    let background = sample_background(id.xy);
    if (weather_is_zero()) {
        textureStore(output_texture, pixel_index, vec4<f32>(background, 1.0));
        return;
    }
    let pixel = vec2<f32>(id.xy) + vec2<f32>(0.5);
    let ray = camera_ray(pixel);
    var color = background;

    // Atmosphere first: darken the sky, then blend haze/dust, then add light.
    color = color * cloud_shadow_factor(ray);
    color = apply_haze(color, ray);
    color = color + rainbow_radiance(ray);

    // Rain: far layer first, near layer last.
    let rain = clamp(params.weather.x, 0.0, 1.0);
    if (rain > 0.0) {
        for (var layer: u32 = 0u; layer < params.counts.x; layer = layer + 1u) {
            let streak = streak_layer(
                pixel,
                layer,
                params.counts.x,
                0x2f7d31u,
                rain,
                vec3<f32>(0.62, 0.70, 0.82),
                1.0,
                1.0,
            );
            color = mix(color, streak.rgb, clamp(streak.a, 0.0, 1.0));
        }
    }

    // Snow: slower white flakes with stylised parallax.
    let snow = clamp(params.weather.y, 0.0, 1.0);
    if (snow > 0.0) {
        for (var layer: u32 = 0u; layer < params.counts.y; layer = layer + 1u) {
            let flake = flake_layer(
                pixel,
                layer,
                params.counts.y,
                0x5adb91u,
                snow,
                120.0,
                210.0,
                90.0,
                220.0,
                0.012,
                0.030,
                1.0,
                vec3<f32>(0.88, 0.92, 0.99),
            );
            color = mix(color, flake.rgb, clamp(flake.a, 0.0, 1.0));
        }
    }

    // Hail: small, hard, fast stones with a short motion streak. Keep the
    // blend partial so stones read as ice rather than glowing ellipsoid sprites;
    // the earlier large/opaque version dominated the supercell frame.
    let hail = clamp(params.weather.z, 0.0, 1.0);
    if (hail > 0.0) {
        for (var layer: u32 = 0u; layer < params.counts.z; layer = layer + 1u) {
            let stone = flake_layer(
                pixel,
                layer,
                params.counts.z,
                0x77a1c9u,
                hail,
                220.0,
                320.0,
                900.0,
                1500.0,
                0.005,
                0.010,
                0.42,
                vec3<f32>(0.62, 0.68, 0.78),
            );
            color = mix(color, stone.rgb, clamp(stone.a, 0.0, 1.0) * 0.72);
        }
    }

    // Sleet: white slanted streaks between rain and snow.
    let sleet = clamp(params.weather.w, 0.0, 1.0);
    if (sleet > 0.0) {
        for (var layer: u32 = 0u; layer < params.counts.w; layer = layer + 1u) {
            let streak = streak_layer(
                pixel,
                layer,
                params.counts.w,
                0x91b3d7u,
                sleet,
                vec3<f32>(0.80, 0.85, 0.92),
                0.55,
                0.55,
            );
            color = mix(color, streak.rgb, clamp(streak.a, 0.0, 1.0));
        }
    }

    // Lightning: uniform flash first, then the additive bolt.
    let flash = clamp(params.lightning.x, 0.0, 1.0);
    if (flash > 0.0) {
        color = color * (1.0 + 1.1 * flash) + flash * vec3<f32>(0.30, 0.34, 0.44);
        color = color + lightning_bolt(pixel);
    }

    textureStore(output_texture, pixel_index, vec4<f32>(max(color, vec3<f32>(0.0)), 1.0));
}
