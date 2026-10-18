// Real-time volumetric cloud pass.
// Units: metres, seconds, kg/m3, W/m2. Linear Rec.2020 RGB.
// v1 uses gray visible optics, two-lobe HG, and fixed multiple-scattering
// octaves. See docs/clouds_realtime.md for the documented approximations.

const PI: f32 = 3.141592653589793;

struct Params {
    camera_origin: vec4<f32>,
    camera_forward: vec4<f32>,
    camera_right: vec4<f32>,
    camera_up: vec4<f32>,
    volume_origin: vec4<f32>,
    volume_spacing: vec4<f32>,
    volume_dims: vec4<u32>,
    sun_dir: vec4<f32>,
    solar_weights: array<vec4<f32>, 3>,
    viewport: vec4<f32>, // width, height, tan(fov/2), frame
    params: vec4<f32>,   // max_steps, extinction_scale, ambient_scale, ms_octaves
    detail: vec4<f32>,   // erosion_strength, drift_x, drift_z, sun_steps
    // x = aerial distance, y = aerial strength, zw = base-volume visual drift
    // (metres, x/z) that keeps the cloud moving between volume uploads.
    aerial: vec4<f32>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var volume: texture_3d<f32>;
@group(0) @binding(2) var volume_sampler: sampler;
@group(0) @binding(3) var output: texture_storage_2d<rgba32float, write>;
// x = weather overcast occlusion in [0, 1]; thick decks block direct sun.
@group(0) @binding(4) var<uniform> atmosphere: vec4<f32>;
@group(0) @binding(5) var detail_noise: texture_3d<f32>;
@group(0) @binding(6) var detail_sampler: sampler;

@group(1) @binding(0) var environment: texture_2d<f32>;
@group(1) @binding(1) var sun_transmittance: texture_2d<f32>;

fn hash12(seed: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(seed.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn henyey_greenstein(c: f32, g: f32) -> f32 {
    let gg = clamp(g, -0.98, 0.98);
    let cc = clamp(c, -1.0, 1.0);
    let denom = max(1.0 + gg * gg - 2.0 * gg * cc, 1.0e-7);
    return (1.0 - gg * gg) / (4.0 * PI * denom * sqrt(denom));
}

fn dual_lobe_phase(c: f32, g: f32) -> f32 {
    let g1 = clamp(g + 0.03, -0.95, 0.95);
    let g2 = clamp(g - 0.18, -0.95, 0.95);
    return 0.75 * henyey_greenstein(c, g1) + 0.25 * henyey_greenstein(c, g2);
}

fn asymmetric_g(reff: f32, ice: f32) -> f32 {
    let reff_um = clamp(reff * 1.0e6, 2.0, 80.0);
    let water = clamp(0.85 - 0.02 * log2(reff_um / 10.0), 0.78, 0.88);
    return clamp(mix(water, 0.78, clamp(ice, 0.0, 1.0)), 0.0, 0.95);
}

fn volume_min() -> vec3<f32> {
    return p.volume_origin.xyz;
}

fn volume_max() -> vec3<f32> {
    return p.volume_origin.xyz + p.volume_spacing.xyz * vec3<f32>(p.volume_dims.xyz);
}

fn cloud_sample(world: vec3<f32>) -> vec4<f32> {
    let extent = p.volume_spacing.xyz * vec3<f32>(p.volume_dims.xyz);
    // Interpolate the wind advection between weather-volume uploads: the data
    // was reconstructed at the last upload time, so slide the sampling
    // position by the drift accumulated since then. Without this the cloud
    // only moves in discrete jumps at the rebuild cadence (stop-motion at high
    // simulation speed), no matter how smooth the rest of the frame is.
    // The offset is at most a fraction of a voxel per update and resets to zero
    // when a new snapshot is uploaded, so the sampled field stays consistent.
    let drift = vec3<f32>(p.aerial.z, 0.0, p.aerial.w);
    let uvw = (world + drift - p.volume_origin.xyz) / max(extent, vec3<f32>(1.0e-6));
    if (any(uvw < vec3<f32>(0.0)) || any(uvw > vec3<f32>(1.0))) {
        return vec4<f32>(0.0);
    }
    return textureSampleLevel(volume, volume_sampler, uvw, 0.0);
}

fn optical_sigma(s: vec4<f32>, scale: f32) -> f32 {
    if (s.r <= 1.0e-7) {
        return 0.0;
    }
    let reff = max(s.g, 1.0e-7);
    var sigma = 3.0 * s.r / (2.0 * 1000.0 * reff);
    if (s.b > 0.5) {
        sigma *= 0.8;
    }
    return sigma * scale;
}

/// Base density from the weather volume only. The sun shadow march uses this
/// (no sub-grid detail fetch) to keep the light path cheap and stable.
fn sigma_base(world: vec3<f32>, scale: f32) -> f32 {
    return optical_sigma(cloud_sample(world), scale);
}

/// Primary-view extinction including sub-grid erosion. Detail is applied to
/// every populated sample, not only the optically thin edge: the volume is
/// coarse enough (250 m x 146 m voxels) that both the silhouette and the
/// internal structure need the extra octaves. The sun shadow march keeps using
/// the base density (see `sigma_base`) so the light path stays cheap/stable.
fn sigma_at(world: vec3<f32>, base_sigma: f32, distance: f32) -> f32 {
    return max(base_sigma * cloud_detail(world, distance, base_sigma), 0.0);
}

/// Distance LOD for the four sub-grid detail scales.
/// x = broad (~256-2048 m features, R at the 8 km tiling),
/// y = medium (~128-1024 m, R at the 4 km tiling),
/// z = fine (~128-256 m, G at the 4 km tiling),
/// w = micro (~32-64 m, G at the 1 km tiling).
fn detail_lod(d: f32) -> vec4<f32> {
    // The broad/medium scales stay at full strength out to 25-30 km and are
    // only faded near the far edge of the 32 km domain: a 250 m weather voxel
    // is still ~9 px at 25 km, so the silhouette has to stay broken all the
    // way to the horizon. The fine scales fade out before they shrink below a
    // couple of trace pixels; without a mip chain an unfaded 32-128 m octave
    // at 5+ km is pure temporal sparkle.
    let broad = 1.0;
    let medium = 1.0 - smoothstep(25000.0, 36000.0, d);
    let fine = 1.0 - smoothstep(3000.0, 6500.0, d);
    let micro = 1.0 - smoothstep(900.0, 2600.0, d);
    return vec4<f32>(broad, medium, fine, micro);
}

/// Sub-grid erosion of the coarse weather volume.
///
/// The weather volume is 128x96x128 over 32x14x32 km, so the raw cloud
/// boundary reaches the screen as single-voxel staircases and thick decks read
/// as smooth fog. Four octaves of the tileable 3D detail texture carve billows
/// and holes out of the interpolated density; each octave fades with its own
/// distance LOD while the broad/medium octaves stay active across the whole
/// domain.
///
/// This is a "membership warp": the high-contrast remap turns the smooth
/// octave sum into a hole/billow mask. Density falls to ~0.15x inside the
/// carved gaps and rises to ~2.0x in the billow cores. The mask is thresholded
/// so about half of the samples are pushed below their base density: at
/// 10-30 km a single coarse step carries enough optical depth that only a
/// large local reduction can actually move the tau~1 isosurface, which is what
/// breaks the voxel staircase into a ragged, fractal outline. The window is
/// offset slightly above 1.0 on average (weak mean compensation) so the carved
/// edges stay crisp instead of dimming the whole cloud.
/// Thick cloud bodies are carved only weakly: turning an optically thick
/// interior into speckle buys nothing and costs stability. This is a visual
/// turbulence model, not conserved liquid water.
fn cloud_detail(world: vec3<f32>, distance: f32, base_sigma: f32) -> f32 {
    let strength = clamp(p.detail.x, 0.0, 1.0);
    // Carve only where the base density is optically thin: 1.0 at
    // sigma < 0.05 m^-1, 0 above 0.3 m^-1 (storm interiors stay coherent).
    // The amplitude is known before the fetches, so the expensive 3D detail
    // samples are skipped entirely for thick interiors. Output-identical:
    // `mix(1.0, erode, 0.0)` is exactly 1.0.
    let carve = 1.0 - smoothstep(0.05, 0.30, base_sigma);
    let amount = clamp(strength * 1.25, 0.0, 1.0) * carve;
    if (amount <= 0.0) {
        return 1.0;
    }
    let d = max(distance, 0.0);
    // The detail texture is advected with the weather wind so the billows
    // travel with the cloud instead of swimming through it.
    let drift = vec3<f32>(p.detail.y, 0.0, p.detail.z);
    let q = world + drift;
    let w = detail_lod(d);
    // Broad + medium are always sampled: they are what breaks the voxel
    // silhouette from 3 km to the far edge of the volume. The finer octaves
    // ride on top with lower weight so they only perturb the carve locally.
    var n = textureSampleLevel(detail_noise, detail_sampler, q * (1.0 / 8192.0), 0.0).r * w.x;
    n += textureSampleLevel(
        detail_noise,
        detail_sampler,
        q * (1.0 / 4096.0) + vec3<f32>(0.37, 0.11, 0.73),
        0.0
    ).r * (1.0 * w.y);
    var wsum = w.x + 1.0 * w.y;
    // Fine scales are distance-culled with explicit-LOD fetches; the dynamic
    // branch is coherent over whole screen tiles (same distance band). Keep
    // their weight low: the silhouette should break into 0.3-1 km cauliflower,
    // not into 50-150 m torn-paper fringes.
    if (w.z > 0.0) {
        n += textureSampleLevel(
            detail_noise,
            detail_sampler,
            q * (1.0 / 4096.0) + vec3<f32>(0.71, 0.53, 0.19),
            0.0
        ).g * (0.30 * w.z);
        wsum += 0.30 * w.z;
    }
    if (w.w > 0.0) {
        n += textureSampleLevel(
            detail_noise,
            detail_sampler,
            q * (1.0 / 1024.0) + vec3<f32>(0.13, 0.87, 0.41),
            0.0
        ).g * (0.12 * w.w);
        wsum += 0.12 * w.w;
    }
    n = clamp(n / max(wsum, 1.0e-6), 0.0, 1.0);
    // Soft threshold so the octave sum carves readable billows instead of
    // gently modulating the density.
    let shaped = smoothstep(0.30, 0.70, n);
    let erode = mix(0.15, 2.0, shaped);
    return mix(1.0, erode, amount);
}

fn ray_box(origin: vec3<f32>, direction: vec3<f32>) -> vec2<f32> {
    let lo = volume_min();
    let hi = volume_max();
    var t0 = 0.0;
    var t1 = 1.0e30;
    // X
    if (abs(direction.x) < 1.0e-8) {
        if (origin.x < lo.x || origin.x > hi.x) { return vec2<f32>(1.0, -1.0); }
    } else {
        var a = (lo.x - origin.x) / direction.x;
        var b = (hi.x - origin.x) / direction.x;
        if (a > b) { let tmp = a; a = b; b = tmp; }
        t0 = max(t0, a);
        t1 = min(t1, b);
        if (t1 <= t0) { return vec2<f32>(1.0, -1.0); }
    }
    // Y
    if (abs(direction.y) < 1.0e-8) {
        if (origin.y < lo.y || origin.y > hi.y) { return vec2<f32>(1.0, -1.0); }
    } else {
        var a = (lo.y - origin.y) / direction.y;
        var b = (hi.y - origin.y) / direction.y;
        if (a > b) { let tmp = a; a = b; b = tmp; }
        t0 = max(t0, a);
        t1 = min(t1, b);
        if (t1 <= t0) { return vec2<f32>(1.0, -1.0); }
    }
    // Z
    if (abs(direction.z) < 1.0e-8) {
        if (origin.z < lo.z || origin.z > hi.z) { return vec2<f32>(1.0, -1.0); }
    } else {
        var a = (lo.z - origin.z) / direction.z;
        var b = (hi.z - origin.z) / direction.z;
        if (a > b) { let tmp = a; a = b; b = tmp; }
        t0 = max(t0, a);
        t1 = min(t1, b);
        if (t1 <= t0) { return vec2<f32>(1.0, -1.0); }
    }
    return vec2<f32>(t0, t1);
}

fn sun_optical_depth(pos: vec3<f32>, steps: u32, scale: f32) -> f32 {
    let dir = p.sun_dir.xyz;
    let t = ray_box(pos, dir);
    if (t.y <= t.x) {
        return 0.0;
    }
    let dt = (t.y - t.x) / f32(max(steps, 1u));
    var tau = 0.0;
    for (var i = 0u; i < steps; i++) {
        let sample_pos = pos + dir * (t.x + (f32(i) + 0.5) * dt);
        tau += sigma_base(sample_pos, scale) * dt;
    }
    return tau;
}

fn solar_rgb() -> vec3<f32> {
    let t = textureLoad(sun_transmittance, vec2<i32>(0, 0), 0);
    let r = max(dot(t, p.solar_weights[0]), 0.0);
    let g = max(dot(t, p.solar_weights[1]), 0.0);
    let b = max(dot(t, p.solar_weights[2]), 0.0);
    return vec3<f32>(r, g, b);
}

fn environment_radiance(direction: vec3<f32>) -> vec3<f32> {
    let dims = textureDimensions(environment);
    if (dims.x == 0u || dims.y == 0u) {
        return vec3<f32>(0.0);
    }
    let lon = atan2(direction.x, direction.z);
    let lat = acos(clamp(direction.y, -1.0, 1.0));
    let u = clamp(i32((lon / (2.0 * PI) + 0.5) * f32(dims.x)), 0, i32(dims.x) - 1);
    let v = clamp(i32((lat / PI) * f32(dims.y)), 0, i32(dims.y) - 1);
    return textureLoad(environment, vec2<i32>(u, v), 0).rgb;
}

fn cloud_lighting(
    pos: vec3<f32>,
    ray_dir: vec3<f32>,
    s: vec4<f32>,
    sun_rgb: vec3<f32>,
    sigma_t: f32,
) -> vec3<f32> {
    let reff = max(s.g, 1.0e-7);
    let ice = s.b;
    let g = asymmetric_g(reff, ice);
    let cos_scatter = clamp(dot(p.sun_dir.xyz, -ray_dir), -1.0, 1.0);
    let sigma_s = sigma_t * 0.999;
    let single_phase = dual_lobe_phase(cos_scatter, g);
    let sun_steps = u32(clamp(p.detail.w, 1.0, 12.0));
    let tau = sun_optical_depth(pos, sun_steps, 1.0);
    let sun_t = exp(-tau);
    // The 4-step sun optical depth above the cloud sample is a coarse estimate;
    // for a thick overcast deck it must not leak a direct-sun contribution.
    // The application supplies a diagnostic overcast occlusion, so fade the
    // direct-sun terms and most of the ambient as the deck thickens. This is a
    // documented approximation for the missing transported cloud shadow.
    let overcast = clamp(atmosphere.x, 0.0, 1.0);
    let storm_severity = clamp(atmosphere.y, 0.0, 1.0);
    let sun_visibility = 1.0 - 0.97 * overcast;
    let ambient_visibility = 1.0 - 0.75 * overcast;
    // Beer-Powder: optically thin edges receive less direct light, so the
    // billows read as volumes instead of flat cotton. The length scale is a
    // visual parameter; spectral multiple scattering is still the octave model.
    let powder = 1.0 - exp(-2.0 * sigma_t * 350.0);
    let powder_gain = mix(0.62, 1.0, powder);
    var source =
        sigma_s * single_phase * sun_rgb * sun_t * 0.7 * sun_visibility * powder_gain;

    let octaves = u32(clamp(p.params.w, 1.0, 4.0));
    for (var k = 1u; k <= octaves; k++) {
        let kf = f32(k);
        let scale = pow(0.5, kf);
        let weight = pow(0.5, kf);
        let phase = dual_lobe_phase(cos_scatter, pow(g, kf + 1.0));
        let t = exp(-tau * scale);
        source +=
            sigma_s * phase * sun_rgb * t * weight * 0.3 * sun_visibility * powder_gain;
    }

    let env = environment_radiance(ray_dir);
    // Overcast light is close to spectrally flat; desaturate the clear-sky
    // ambient so thick cloud bases read neutral grey instead of blue.
    let env_grey = dot(env, vec3<f32>(0.2126, 0.7152, 0.0722));
    var ambient =
        mix(env, vec3<f32>(env_grey), 0.85 * overcast) * p.params.z * ambient_visibility;
    // Cheap sky occlusion: one extra base-density fetch ~550 m straight above
    // the sample. Cloud between the sample and the sky blocks the ambient/sky
    // term, so bases and undersides darken and the body reads as a volume.
    // exp(-tau) is bounded to [0, 1] and monotonic (no ringing); the floor
    // keeps bases from going pitch black because this single-scatter model
    // has no diffuse transmission down from the sunlit top.
    let above = cloud_sample(pos + vec3<f32>(0.0, 550.0, 0.0));
    let tau_up = optical_sigma(above, 1.0) * 550.0 * 0.55;
    let sky_occlusion = clamp(exp(-tau_up), 0.0, 1.0);
    ambient *= mix(0.32, 1.0, sky_occlusion);
    source += sigma_s * ambient;
    // Height-based base darkening, applied to every cloud type (not only the
    // overcast/storm branch below) so bases read darker than tops.
    let low = volume_min();
    let extent = max(volume_max() - low, vec3<f32>(1.0));
    let h = clamp((pos.y - low.y) / extent.y, 0.0, 1.0);
    source *= mix(0.85, 1.0, smoothstep(0.0, 0.32, h));
    // Very thick overcast decks converge to a flat grey wall because the
    // ambient term dominates the radiance. Add a cheap low-frequency
    // trigonometric modulation plus a darker base gradient so a "black cloud
    // pressing down on the city" still reads as structured cloud, not fog.
    // This is a stylised detail term, not a transported density/shadow field.
    let n1 =
        sin(pos.x * 0.0019 + 1.3) * sin(pos.z * 0.0023 + 0.7) * sin(pos.y * 0.0041);
    let n2 =
        sin(pos.x * 0.0047 + 2.1) * sin(pos.z * 0.0039 + 1.1) * sin(pos.y * 0.0068 + 0.3);
    let structure = 0.5 + 0.35 * n1 + 0.15 * n2;
    let base_gradient = smoothstep(0.0, 0.30, h);
    let modulation = mix(0.45, 1.25, structure) * mix(0.55, 1.05, base_gradient);
    source *= mix(1.0, modulation, 0.85 * max(overcast * 0.5, min(storm_severity, 1.0)));
    // Multiple-scattering octave weights are being recalibrated for analytic
    // step integration; global scale is part of the fit against the previous
    // brightness targets (fair/partly/storm).
    return source * 0.55;
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let out_dims = textureDimensions(output);
    if (id.x >= out_dims.x || id.y >= out_dims.y) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(out_dims) * 2.0 - 1.0;
    let aspect = f32(out_dims.x) / f32(out_dims.y);
    let tan_half = p.viewport.z;
    let direction = normalize(
        p.camera_forward.xyz
        + p.camera_right.xyz * (uv.x * tan_half * aspect)
        - p.camera_up.xyz * (uv.y * tan_half)
    );
    let origin = p.camera_origin.xyz;

    let interval = ray_box(origin, direction);
    if (interval.y <= interval.x) {
        textureStore(output, vec2<i32>(id.xy), vec4<f32>(0.0, 0.0, 0.0, 1.0));
        return;
    }

    let steps = u32(clamp(p.params.x, 1.0, 256.0));
    let samples = max(p.volume_dims.w, 1u);
    let sun_rgb = solar_rgb();
    var accum_radiance = vec3<f32>(0.0);
    var accum_transmittance = 0.0;
    // Radiance-weighted distance to the cloud, used for a cheap aerial
    // perspective blend toward the sky along the view ray.
    var accum_depth = 0.0;
    var accum_depth_weight = 0.0;

    for (var s = 0u; s < samples; s++) {
        var transmittance = 1.0;
        var radiance = vec3<f32>(0.0);
        var previous = interval.x;
        var sample_depth = 0.0;
        var sample_depth_weight = 0.0;
        // Stratified offsets average the step-boundary noise without a
        // temporal reprojection pass. samples=1 is a fixed (band-limited)
        // query; the application uses 2/4 by default.
        for (var i = 0u; i < steps; i++) {
            // Concentrate steps near the camera: low clouds at 0.5-3 km occupy
            // a small fraction of a 32 km ray. The power distribution keeps the
            // near-field sampling useful without a full occupancy structure.
            // Each step gets an independent jitter so the power-warp shells do
            // not survive as concentric bands. The frame index also perturbs
            // the offsets so temporal resolve integrates new samples instead
            // of repeating one fixed pattern.
            let jitter = hash12(
                vec2<f32>(f32(id.x), f32(id.y))
                + f32(s) * 13.0
                + f32(i) * 7.0
                + p.viewport.w * 0.6180339
            );
            let u = clamp((f32(i) + jitter) / f32(steps), 0.0, 1.0);
            let next = interval.x + (interval.y - interval.x) * pow(u, 3.0);
            let dt = max(next - previous, 0.0);
            let distance = 0.5 * (previous + next);
            previous = next;
            let pos = origin + direction * distance;
            let s_cloud = cloud_sample(pos);
            if (s_cloud.r > 1.0e-7) {
                let base_sigma = optical_sigma(s_cloud, 1.0);
                let sigma = sigma_at(pos, base_sigma, distance);
                if (sigma > 0.0) {
                    let source = cloud_lighting(pos, direction, s_cloud, sun_rgb, sigma);
                    let step_radiance = transmittance * source * dt;
                    radiance += step_radiance;
                    let depth_weight =
                        dot(step_radiance, vec3<f32>(0.2126, 0.7152, 0.0722));
                    sample_depth += distance * depth_weight;
                    sample_depth_weight += depth_weight;
                    transmittance *= exp(-sigma * dt);
                    if (transmittance < 1.0e-4) {
                        break;
                    }
                }
            }
        }
        accum_radiance += radiance;
        accum_transmittance += transmittance;
        accum_depth += sample_depth;
        accum_depth_weight += sample_depth_weight;
    }

    let base_radiance = accum_radiance / f32(samples);
    let transmittance = accum_transmittance / f32(samples);
    // Aerial perspective: blend the cloud radiance toward the sky colour over
    // an exponential distance scale. This compensates for the fact that the
    // cloud pass integrates in vacuum while the background sky already
    // contains the atmosphere; it is a visual approximation, not a transport
    // solve (see docs/clouds_realtime.md).
    let mean_distance = accum_depth / max(accum_depth_weight, 1.0e-6);
    let aerial_distance = max(p.aerial.x, 0.0);
    let aerial_strength = clamp(p.aerial.y, 0.0, 1.0);
    var radiance = base_radiance;
    if (aerial_distance > 1.0 && accum_depth_weight > 0.0) {
        let factor = (1.0 - exp(-mean_distance / aerial_distance)) * aerial_strength;
        radiance = mix(base_radiance, environment_radiance(direction), factor);
    }
    // The composite pass adds the background and applies a small
    // edge-preserving spatial filter to this buffer.
    textureStore(
        output,
        vec2<i32>(id.xy),
        vec4<f32>(radiance, transmittance),
    );
}


