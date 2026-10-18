// Cloud shadow-map pass: top-down sun transmittance through the weather
// volume, used by the composite pass to darken the ground under the deck.
//
// The map is a 256x256 R32Float texture covering the volume's XZ footprint.
// Each texel marches from the ground (y = 0) along the sun direction through
// the volume and stores exp(-tau), where tau is the cloud optical depth. The
// composite pass reconstructs the ground hit point from the camera ray and
// multiplies the background by the sampled transmittance.
//
// This is a single-scattering shadow map, not a multiple-scattering / cloud
// interreflection solve; it also ignores cloud shadows on the sky itself.

struct ShadowParams {
    sun: vec4<f32>,
    volume_origin: vec4<f32>,
    volume_spacing: vec4<f32>,
    volume_dims: vec4<u32>,
    // xz origin and xz extent of the shadow domain (normally the volume box).
    domain: vec4<f32>,
};

@group(0) @binding(0) var volume: texture_3d<f32>;
@group(0) @binding(1) var volume_sampler: sampler;
@group(0) @binding(2) var<uniform> p: ShadowParams;
@group(0) @binding(3) var output: texture_storage_2d<r32float, write>;

fn volume_min() -> vec3<f32> {
    return p.volume_origin.xyz;
}

fn volume_max() -> vec3<f32> {
    return p.volume_origin.xyz + p.volume_spacing.xyz * vec3<f32>(p.volume_dims.xyz);
}

fn cloud_sample(world: vec3<f32>) -> vec4<f32> {
    let lo = volume_min();
    let extent = max(volume_max() - lo, vec3<f32>(1.0));
    let uvw = (world - lo) / extent;
    if (any(uvw < vec3<f32>(0.0)) || any(uvw > vec3<f32>(1.0))) {
        return vec4<f32>(0.0);
    }
    return textureSampleLevel(volume, volume_sampler, uvw, 0.0);
}

fn sigma_at(world: vec3<f32>) -> f32 {
    let s = cloud_sample(world);
    if (s.r <= 1.0e-7) {
        return 0.0;
    }
    let reff = max(s.g, 1.0e-7);
    var sigma = 3.0 * s.r / (2.0 * 1000.0 * reff);
    if (s.b > 0.5) {
        sigma *= 0.8;
    }
    return sigma;
}

/// Slab intersection of the ray with the volume box.
fn ray_box(origin: vec3<f32>, direction: vec3<f32>) -> vec2<f32> {
    let lo = volume_min();
    let hi = volume_max();
    var t0 = 0.0;
    var t1 = 1.0e30;
    for (var axis = 0u; axis < 3u; axis++) {
        let o = origin[axis];
        let d = direction[axis];
        let l = lo[axis];
        let h = hi[axis];
        if (abs(d) < 1.0e-8) {
            if (o < l || o > h) {
                return vec2<f32>(1.0, -1.0);
            }
        } else {
            let a = (l - o) / d;
            let b = (h - o) / d;
            t0 = max(t0, min(a, b));
            t1 = min(t1, max(a, b));
            if (t1 <= t0) {
                return vec2<f32>(1.0, -1.0);
            }
        }
    }
    return vec2<f32>(t0, t1);
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output);
    if (id.x >= dims.x || id.y >= dims.y) {
        return;
    }
    let sun = normalize(p.sun.xyz);
    if (sun.y <= 0.02) {
        textureStore(output, vec2<i32>(id.xy), vec4<f32>(1.0));
        return;
    }
    let u = (f32(id.x) + 0.5) / f32(dims.x);
    let v = (f32(id.y) + 0.5) / f32(dims.y);
    let x = p.domain.x + u * p.domain.z;
    let z = p.domain.y + v * p.domain.w;
    let ground = vec3<f32>(x, 0.0, z);
    let interval = ray_box(ground, sun);
    if (interval.y <= interval.x) {
        textureStore(output, vec2<i32>(id.xy), vec4<f32>(1.0));
        return;
    }
    let steps = 96u;
    let dt = (interval.y - interval.x) / f32(steps);
    var tau = 0.0;
    for (var i = 0u; i < steps; i++) {
        let t = interval.x + (f32(i) + 0.5) * dt;
        tau += sigma_at(ground + sun * t) * dt;
    }
    textureStore(output, vec2<i32>(id.xy), vec4<f32>(exp(-tau)));
}
