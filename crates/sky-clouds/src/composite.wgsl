// Composite pass: upsample the cloud trace buffer (possibly half resolution)
// over the full-resolution sky/background and apply the overcast/storm
// coupling. The upsample is a 5x5 bilateral filter guided by transmittance and
// radiance: cloud edges stay sharp instead of being bilinearly smeared, while
// small step noise inside smooth regions is still suppressed. There is no
// depth buffer yet, so transmittance is the geometric guide. Aerial
// perspective is not included.
//
// Overcast coupling: the application supplies an occlusion value derived from
// the weather cloud coverage / liquid water path. 1 means a thick overcast
// deck; the pass then dims both the background sky/ground and the cloud
// radiance. This is a documented approximation for the missing transported
// cloud-shadow / diffuse-illumination field, not a physical light transport
// solve.

@group(0) @binding(0) var cloud: texture_2d<f32>;
@group(0) @binding(1) var background: texture_2d<f32>;
@group(0) @binding(2) var output: texture_storage_2d<rgba32float, write>;
// x = overcast occlusion in [0, 1].
@group(0) @binding(3) var<uniform> atmosphere: vec4<f32>;

struct CompositeParams {
    camera_origin: vec4<f32>,
    camera_forward: vec4<f32>,
    camera_right: vec4<f32>,
    camera_up: vec4<f32>,
    // xz origin, xz extent of the cloud-volume (shadow-map) footprint.
    volume_domain: vec4<f32>,
    // x = tan(fov_y/2), y = aspect, z = shadow strength, w unused.
    data: vec4<f32>,
};

@group(0) @binding(4) var<uniform> cp: CompositeParams;
@group(0) @binding(5) var shadow_map: texture_2d<f32>;

fn sample_shadow(pixel: vec2<f32>) -> f32 {
    let dims = textureDimensions(shadow_map);
    let last = vec2<i32>(i32(dims.x) - 1, i32(dims.y) - 1);
    let base = vec2<i32>(floor(pixel));
    let frac = clamp(pixel - vec2<f32>(base), vec2<f32>(0.0), vec2<f32>(1.0));
    let c00 = textureLoad(shadow_map, clamp(base, vec2<i32>(0), last), 0).r;
    let c10 = textureLoad(shadow_map, clamp(base + vec2<i32>(1, 0), vec2<i32>(0), last), 0).r;
    let c01 = textureLoad(shadow_map, clamp(base + vec2<i32>(0, 1), vec2<i32>(0), last), 0).r;
    let c11 = textureLoad(shadow_map, clamp(base + vec2<i32>(1, 1), vec2<i32>(0), last), 0).r;
    return mix(mix(c00, c10, frac.x), mix(c01, c11, frac.x), frac.y);
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let out_dims = textureDimensions(output);
    if (id.x >= out_dims.x || id.y >= out_dims.y) {
        return;
    }
    let coord = vec2<i32>(id.xy);

    let trace_dims = textureDimensions(cloud);
    let trace_last = vec2<i32>(i32(trace_dims.x) - 1, i32(trace_dims.y) - 1);
    // Full-resolution pixel centre in trace-texel coordinates (0.5 offsets keep
    // the mapping exact when trace_dims == out_dims).
    let p = ((vec2<f32>(coord) + vec2<f32>(0.5)) / vec2<f32>(out_dims))
        * vec2<f32>(trace_dims)
        - vec2<f32>(0.5);
    let nearest = vec2<i32>(round(p));
    let base = vec2<i32>(floor(p));
    let frac = p - vec2<f32>(base);
    // Bilinear reference: a single nearest texel is itself noisy, and using a
    // noisy reference in the bilateral similarity can reject the good
    // neighbours instead of the bad ones.
    let c00 = textureLoad(cloud, clamp(base, vec2<i32>(0), trace_last), 0);
    let c10 = textureLoad(
        cloud,
        clamp(base + vec2<i32>(1, 0), vec2<i32>(0), trace_last),
        0
    );
    let c01 = textureLoad(
        cloud,
        clamp(base + vec2<i32>(0, 1), vec2<i32>(0), trace_last),
        0
    );
    let c11 = textureLoad(
        cloud,
        clamp(base + vec2<i32>(1, 1), vec2<i32>(0), trace_last),
        0
    );
    let center = mix(
        mix(c00, c10, frac.x),
        mix(c01, c11, frac.x),
        frac.y
    );

    var radiance = vec3<f32>(0.0);
    var transmittance = 0.0;
    var weight_sum = 0.0;
    // 5x5 bilateral in trace space (~10x10 output pixels at half resolution).
    // Half-resolution tracing has 4x fewer first-hit rays, so a wider
    // edge-aware reconstruction is needed to keep the upsampled cloud from
    // showing the per-trace-pixel jitter as speckle.
    for (var dy = -2; dy <= 2; dy++) {
        for (var dx = -2; dx <= 2; dx++) {
            let tap = textureLoad(
                cloud,
                clamp(nearest + vec2<i32>(dx, dy), vec2<i32>(0), trace_last),
                0
            );
            let offset = vec2<f32>(f32(dx), f32(dy)) - (p - vec2<f32>(nearest));
            let spatial = exp(-dot(offset, offset) * 0.45);
            let d_transmittance = abs(tap.a - center.a);
            let d_radiance = dot(
                abs(tap.rgb - center.rgb),
                vec3<f32>(0.2126, 0.7152, 0.0722)
            );
            let similarity = 1.0 / (1.0 + 8.0 * d_transmittance + 4.0 * d_radiance);
            let weight = spatial * similarity;
            radiance += tap.rgb * weight;
            transmittance += tap.a * weight;
            weight_sum += weight;
        }
    }
    radiance /= max(weight_sum, 1.0e-6);
    transmittance /= max(weight_sum, 1.0e-6);

    let bg_dims = textureDimensions(background);
    let bg_coord = clamp(
        coord,
        vec2<i32>(0),
        vec2<i32>(i32(bg_dims.x) - 1, i32(bg_dims.y) - 1)
    );
    var background_color = textureLoad(background, bg_coord, 0).rgb;

    // Cloud shadow on the ground: reconstruct the camera ray, intersect the
    // y = 0 plane, and multiply the background by the sun transmittance from
    // the projected shadow map. Sky pixels and rays above the horizon are
    // unaffected; the shadow map is a single-scattering top-down transmittance.
    let shadow_strength = clamp(cp.data.z, 0.0, 1.0);
    if (shadow_strength > 0.0) {
        let uv_ndc = (vec2<f32>(coord) + 0.5) / vec2<f32>(out_dims) * 2.0 - 1.0;
        let aspect = f32(out_dims.x) / f32(out_dims.y);
        let tan_half = cp.data.x;
        let direction = normalize(
            cp.camera_forward.xyz
            + cp.camera_right.xyz * (uv_ndc.x * tan_half * aspect)
            - cp.camera_up.xyz * (uv_ndc.y * tan_half)
        );
        if (direction.y < -1.0e-4) {
            let t = -cp.camera_origin.y / direction.y;
            let ground = cp.camera_origin.xyz + direction * t;
            let domain = cp.volume_domain;
            let local = vec2<f32>(
                (ground.x - domain.x) / max(domain.z, 1.0e-6),
                (ground.z - domain.y) / max(domain.w, 1.0e-6)
            );
            if (all(local >= vec2<f32>(0.0)) && all(local <= vec2<f32>(1.0))) {
                let shadow_dims = textureDimensions(shadow_map);
                let shadow_pixel = local * vec2<f32>(shadow_dims) - vec2<f32>(0.5);
                // 4-tap PCF: soften the 512^2 map's texel steps while staying
                // cheaper than a full per-pixel 3x3 kernel.
                let radius = 0.75;
                var transmittance = 0.0;
                transmittance += sample_shadow(shadow_pixel + vec2<f32>(-radius, -radius));
                transmittance += sample_shadow(shadow_pixel + vec2<f32>(radius, -radius));
                transmittance += sample_shadow(shadow_pixel + vec2<f32>(-radius, radius));
                transmittance += sample_shadow(shadow_pixel + vec2<f32>(radius, radius));
                transmittance *= 0.25;
                // Cloud shadows darken the direct sun, but the sky still
                // illuminates the ground: cap the darkening so shadows read as
                // soft grey rather than black holes.
                let shade = 1.0 - shadow_strength * (1.0 - transmittance);
                background_color *= max(shade, 1.0 - 0.55 * shadow_strength);
            }
        }
    }

    let overcast = clamp(atmosphere.x, 0.0, 1.0);
    // Storm severity is supplied by the application (lightning / hail / very
    // heavy precip / deep water-loaded towers), so only convective storms get
    // the extreme "black cloud pressing down on the city" drop; plain overcast
    // stays daylight grey. Severity can exceed 1 for supercell-like towers:
    // the part above 1 feeds an extra near-dark tier.
    let severity = clamp(atmosphere.y, 0.0, 1.5);
    let base_severity = min(severity, 1.0);
    let extra = clamp(severity - 1.0, 0.0, 0.5);
    let background_scale = mix(1.0, 0.26, overcast)
        * mix(1.0, 0.30, base_severity)
        * mix(1.0, 0.50, extra * 2.0);
    let cloud_scale = mix(1.0, 0.42, overcast)
        * mix(1.0, 0.34, base_severity)
        * mix(1.0, 0.40, extra * 2.0);
    // Overcast skylight is close to grey; pull the sky/ground tint toward
    // neutral as the deck thickens.
    let bg_luma = dot(background_color, vec3<f32>(0.2126, 0.7152, 0.0722));
    let background_tinted = mix(background_color, vec3<f32>(bg_luma), 0.6 * overcast);
    textureStore(
        output,
        coord,
        vec4<f32>(
            radiance * cloud_scale + transmittance * background_tinted * background_scale,
            1.0
        ),
    );
}
