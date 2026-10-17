#ifndef SKY_TRACER_ATMOSPHERE_GLSL
#define SKY_TRACER_ATMOSPHERE_GLSL

// Four-wave realtime atmosphere, ported from crates/sky-realtime
// (`solver.wgsl` + `transport.wgsl`). Samplers are function parameters so a
// program only binds the textures it actually uses:
//
//   tau    optical depth, RGBA16F, linear filtered
//   atlas  coefficient atlas, RGBA32F, nearest (texelFetch)
//   source directional multiple-scattering source, RGBA16F, linear filtered
//
// All distances are kilometres and all radiance is scene-referred linear
// Rec.2020 unless a function name says otherwise. The four spectral lanes are
// carried in vec4 components; they are not RGB plus an extra channel.

#include "/lib/atmosphere_data.glsl"

const float ATH_PI = 3.141592653589793;
const float ATH_HALF_PI = 1.5707963267948966;

struct AthMedium {
    vec4 extinction;
    vec4 scattering[5];
};

struct AthPhase {
    int index;
    float fraction;
};

struct AthTransport {
    vec4 light;
    vec4 transmittance;
};

// ---------------------------------------------------------------------------
// Solid, spherically symmetric planet. Same closed forms as transport.wgsl.
// ---------------------------------------------------------------------------
float ath_horizon(float h) {
    return -sqrt(max(h * (2.0 * ATMOS_GROUND_RADIUS_KM + h), 0.0))
        / (ATMOS_GROUND_RADIUS_KM + h);
}

bool ath_ground(float h, float mu) {
    return mu < 0.0 && mu <= ath_horizon(h);
}

float ath_height_at(float h, float mu, float d) {
    float dh = d * (d + 2.0 * (ATMOS_GROUND_RADIUS_KM + h) * mu);
    float r = ATMOS_GROUND_RADIUS_KM + h;
    return max(0.0, h + dh / (sqrt(max(r * r + dh, 0.0)) + r));
}

float ath_boundary(float h, float mu, bool hit) {
    float b = (ATMOS_GROUND_RADIUS_KM + h) * mu;
    if (hit) {
        float c = h * (2.0 * ATMOS_GROUND_RADIUS_KM + h);
        return max(0.0, c / max(-b + sqrt(max(b * b - c, 0.0)), 1e-20));
    }
    float c = (ATMOS_TOP_KM - h)
        * (2.0 * ATMOS_GROUND_RADIUS_KM + ATMOS_TOP_KM + h);
    float root = sqrt(max(b * b + c, 0.0));
    if (b > 0.0) {
        return max(0.0, c / max(root + b, 1e-20));
    }
    return max(0.0, -b + root);
}

// Piecewise sqrt(height) mapping used by the optical-depth table. The six
// segments keep material boundaries on texel centres.
vec4 ath_height_segment(float x) {
    vec4 c = ATMOS_SEG0;
    if (x > ATMOS_SEG0.x) c = ATMOS_SEG1;
    if (x > ATMOS_SEG1.x) c = ATMOS_SEG2;
    if (x > ATMOS_SEG2.x) c = ATMOS_SEG3;
    if (x > ATMOS_SEG3.x) c = ATMOS_SEG4;
    if (x > ATMOS_SEG4.x) c = ATMOS_SEG5;
    return c;
}

float ath_height_coord(float h) {
    float x = sqrt(clamp(h, 0.0, ATMOS_TOP_KM));
    vec4 c = ath_height_segment(x);
    return clamp(x * c.y + c.z, 0.0, 1.0);
}

// ---------------------------------------------------------------------------
// Coefficient atlas access. x is always the table index, y the packed row.
// ---------------------------------------------------------------------------
vec4 ath_atlas(sampler2D atlas, int x, int y) {
    return texelFetch(atlas, ivec2(x, y), 0);
}

AthMedium ath_medium(sampler2D atlas, float h) {
    int lo = 0;
    int hi = ATMOS_PROFILE_NODES - 1;
    while (hi - lo > 1) {
        int mid = (lo + hi) / 2;
        if (ath_atlas(atlas, mid, ATLAS_ROW_PROFILE).x <= h) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    float h0 = ath_atlas(atlas, lo, ATLAS_ROW_PROFILE).x;
    float h1 = ath_atlas(atlas, hi, ATLAS_ROW_PROFILE).x;
    float t = clamp((h - h0) / max(h1 - h0, 1e-20), 0.0, 1.0);
    AthMedium m;
    m.extinction = mix(
        ath_atlas(atlas, lo, ATLAS_ROW_PROFILE + 1),
        ath_atlas(atlas, hi, ATLAS_ROW_PROFILE + 1),
        t);
    for (int k = 0; k < 5; k++) {
        m.scattering[k] = mix(
            ath_atlas(atlas, lo, ATLAS_ROW_PROFILE + 2 + k),
            ath_atlas(atlas, hi, ATLAS_ROW_PROFILE + 2 + k),
            t);
    }
    return m;
}

// Fitted solar-elevation coordinate for one height node (calibrated mapping).
float ath_solar_layer(sampler2D atlas, int layer, float e) {
    int row = ATLAS_ROW_SOLAR + layer;
    vec4 base = ath_atlas(atlas, 0, row);
    float d = max(ATH_HALF_PI - e, 0.0);
    float y = base.x * e + base.y - base.z * sqrt(d / (d + 0.0872664626));
    for (int j = 1; j <= 3; j++) {
        vec4 c = ath_atlas(atlas, j, row);
        float x = e - c.x;
        y += c.z * x / (abs(x) + c.y);
    }
    return clamp(y, 0.0, 1.0);
}

// Direct-sun transmittance from a point at altitude h with zenith cosine mu.
vec4 ath_sun_t(sampler2D tau, float h, float mu) {
    if (ath_ground(h, mu)) {
        return vec4(0.0);
    }
    float horizon = ath_horizon(h);
    float x = clamp((mu - horizon) / (1.0 - horizon), 0.0, 1.0);
    float u = pow(x, 1.0 / 3.0);
    vec2 pixel = vec2(
        u * float(ATMOS_OPTICAL_COLS - 1),
        ath_height_coord(h) * float(ATMOS_OPTICAL_ROWS - 1)) + 0.5;
    vec2 size = vec2(float(ATMOS_OPTICAL_COLS), float(ATMOS_OPTICAL_ROWS));
    return exp(-texture(tau, pixel / size));
}

// ---------------------------------------------------------------------------
// Phase functions. Rayleigh is analytic; four aerosol species are tabulated.
// ---------------------------------------------------------------------------
AthPhase ath_phase_stencil(float mu) {
    float x = clamp(
        pow(max((1.0 - mu) * 0.5, 0.0), 1.0 / 3.0) * 1024.0 - 0.5,
        0.0,
        1023.0);
    int i = min(int(x), 1022);
    AthPhase phase;
    phase.index = i;
    phase.fraction = x - float(i);
    return phase;
}

vec4 ath_tabulated_phase(sampler2D atlas, AthPhase stencil, int species) {
    int row = ATLAS_ROW_PHASE + species;
    vec4 lo = ath_atlas(atlas, stencil.index, row);
    vec4 hi = ath_atlas(atlas, min(stencil.index + 1, 1023), row);
    return mix(lo, hi, stencil.fraction);
}

float ath_phase_coord(float nu) {
    return ATMOS_PHASE_WEIGHT * acos(clamp(nu, -1.0, 1.0)) / ATH_PI
        + (1.0 - ATMOS_PHASE_WEIGHT)
            * pow(max((1.0 - nu) * 0.5, 0.0), 1.0 / 3.0);
}

vec4 ath_high_basis(float mu, float mu_s, float nu) {
    float y = clamp(mu, -1.0, 1.0);
    float radial = sqrt(max(1.0 - y * y, 0.0));
    float z = clamp(
        (nu - y * mu_s) / sqrt(max(1.0 - mu_s * mu_s, 1e-20)),
        -radial,
        radial);
    return vec4(1.0, y * y, z * z, y * z);
}

vec4 ath_high_shape(sampler2D atlas, int hi, int si, vec4 basis) {
    int row = ATLAS_ROW_HIGH + (hi + 1 - int(ATMOS_LOW_COUNT));
    vec4 m0 = ath_atlas(atlas, si * 5 + 0, row);
    vec4 m1 = ath_atlas(atlas, si * 5 + 1, row);
    vec4 m2 = ath_atlas(atlas, si * 5 + 2, row);
    vec4 m3 = ath_atlas(atlas, si * 5 + 3, row);
    vec4 m4 = ath_atlas(atlas, si * 5 + 4, row);
    return (m0 + m1 * basis.y + m2 * basis.z + m3 * basis.w)
        / max(dot(m4, basis), 1e-20);
}

// Normalized directional source shape for one incidence/azimuth coordinate.
vec4 ath_shape(sampler2D source, int hi, int si, vec2 uv) {
    vec2 pixel = vec2(
        float(si * ATMOS_PHASE_NODES) + uv.x * float(ATMOS_PHASE_NODES - 1),
        float(hi * ATMOS_CONE_NODES) + uv.y * float(ATMOS_CONE_NODES - 1)) + 0.5;
    vec2 size = vec2(float(ATMOS_SOURCE_WIDTH), float(ATMOS_SOURCE_HEIGHT));
    return texture(source, pixel / size);
}

vec4 ath_ground_light(sampler2D atlas, float mu_s) {
    float x = ath_solar_layer(atlas, 0, asin(clamp(mu_s, -1.0, 1.0)))
        * float(ATMOS_SUN_NODES - 1);
    int i = min(int(x), ATMOS_SUN_NODES - 2);
    return mix(
        ath_atlas(atlas, i, ATLAS_ROW_GROUND),
        ath_atlas(atlas, i + 1, ATLAS_ROW_GROUND),
        x - float(i));
}

// Local multiple-scattering source: log-mean brightness times normalized
// directional shape, exactly as the GPU solver stores it.
vec4 ath_indirect(
    sampler2D atlas,
    sampler2D source,
    float h,
    float mu,
    float mu_s,
    float nu,
    AthMedium c
) {
    int lo = 0;
    int hi = ATMOS_HEIGHT_NODES - 1;
    while (hi - lo > 1) {
        int mid = (lo + hi) / 2;
        if (ath_atlas(atlas, mid, ATLAS_ROW_HEIGHTS).x <= h) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    float h0 = ath_atlas(atlas, lo, ATLAS_ROW_HEIGHTS).x;
    float h1 = ath_atlas(atlas, hi, ATLAS_ROW_HEIGHTS).x;
    float th = clamp((h - h0) / (h1 - h0), 0.0, 1.0);
    float elevation = asin(clamp(mu_s, -1.0, 1.0));
    float s0 = ath_solar_layer(atlas, lo, elevation) * float(ATMOS_SUN_NODES - 1);
    float s1 = ath_solar_layer(atlas, hi, elevation) * float(ATMOS_SUN_NODES - 1);
    int i0 = min(int(s0), ATMOS_SUN_NODES - 2);
    int i1 = min(int(s1), ATMOS_SUN_NODES - 2);
    float t0 = s0 - float(i0);
    float t1 = s1 - float(i1);
    vec4 mean_lo_0 = ath_atlas(atlas, i0, ATLAS_ROW_MEAN + lo);
    vec4 mean_lo_1 = ath_atlas(atlas, i0 + 1, ATLAS_ROW_MEAN + lo);
    vec4 mean_hi_0 = ath_atlas(atlas, i1, ATLAS_ROW_MEAN + hi);
    vec4 mean_hi_1 = ath_atlas(atlas, i1 + 1, ATLAS_ROW_MEAN + hi);
    vec4 brightness = exp(mix(
        mix(mean_lo_0, mean_lo_1, t0),
        mix(mean_hi_0, mean_hi_1, t1),
        th));
    if (ATMOS_NEED_HIGH != 0 && h >= 35.0) {
        vec4 basis = ath_high_basis(mu, mu_s, nu);
        vec4 v = mix(
            mix(
                ath_high_shape(atlas, lo, i0, basis),
                ath_high_shape(atlas, lo, i0 + 1, basis),
                t0),
            mix(
                ath_high_shape(atlas, hi, i1, basis),
                ath_high_shape(atlas, hi, i1 + 1, basis),
                t1),
            th);
        return max(v * brightness * c.scattering[0], vec4(0.0));
    }
    float cp = clamp(
        (mu - mu_s * nu)
            / sqrt(max((1.0 - mu_s * mu_s) * (1.0 - nu * nu), 1e-20)),
        -1.0,
        1.0);
    float t = (1.0 - cp) * 0.5;
    float angle = t + ATMOS_CONE_WARP * t * (1.0 - t) * (2.0 * t - 1.0);
    vec2 uv = vec2(ath_phase_coord(nu), angle);
    vec4 v = mix(
        mix(ath_shape(source, lo, i0, uv), ath_shape(source, lo, i0 + 1, uv), t0),
        mix(ath_shape(source, hi, i1, uv), ath_shape(source, hi, i1 + 1, uv), t1),
        th);
    vec4 sigma = c.scattering[0] + c.scattering[1] + c.scattering[2]
        + c.scattering[3] + c.scattering[4];
    return max(v * brightness * sigma, vec4(0.0));
}

// ---------------------------------------------------------------------------
// Path integration. `segment` > 0 limits the path for finite-distance fog;
// the sky uses segment = 0 (full atmosphere). Steps are a compile-time
// constant so the loop unrolls.
// ---------------------------------------------------------------------------
AthTransport ath_integrate_at(
    sampler2D tau,
    sampler2D atlas,
    sampler2D source,
    vec3 ray,
    vec3 sun,
    float initial_height,
    float segment,
    int steps
) {
    AthTransport result;
    result.light = vec4(0.0);
    result.transmittance = vec4(1.0);
    float h = initial_height;
    float mu = ray.y;
    float mu_s = sun.y;
    float nu = dot(ray, sun);
    float entry = 0.0;
    vec3 up = vec3(0.0, 1.0, 0.0);
    if (h > ATMOS_TOP_KM) {
        float radius = ATMOS_GROUND_RADIUS_KM + h;
        float b = radius * mu;
        float c = (h - ATMOS_TOP_KM)
            * (radius + ATMOS_GROUND_RADIUS_KM + ATMOS_TOP_KM);
        float disc = b * b - c;
        if (b >= 0.0 || disc <= 0.0) {
            return result;
        }
        entry = c / (-b + sqrt(disc));
        if (segment > 0.0 && entry >= segment) {
            return result;
        }
        up = normalize(up * radius + ray * entry);
        h = ATMOS_TOP_KM;
        mu = dot(ray, up);
        mu_s = dot(sun, up);
    }
    bool hit = ath_ground(h, mu);
    float full_length = ath_boundary(h, mu, hit);
    float length = full_length;
    if (segment > 0.0) {
        length = min(length, max(segment - entry, 0.0));
    }
    int count = steps;
    float b = (ATMOS_GROUND_RADIUS_KM + h) * mu;
    float middle = clamp(-b, 0.0, length);
    float hmin = ath_height_at(h, mu, middle);
    float hend = ath_height_at(h, mu, length);
    float scale = max(ATMOS_PATH_HEIGHT_SCALE, 1e-10);
    float before = 0.0;
    float after = 0.0;
    if (ATMOS_PATH_HEIGHT_SCALE > 0.0) {
        before = log(1.0 + max(h - hmin, 0.0) / scale);
        after = log(1.0 + max(hend - hmin, 0.0) / scale);
    }
    int split = int(round(float(count) * before / max(before + after, 1e-20)));
    if (middle <= 0.0) {
        split = 0;
    } else if (middle >= length) {
        split = count;
    } else {
        split = clamp(split, 1, count - 1);
    }
    vec4 solar_phase[5];
    float cosine = clamp(nu, -1.0, 1.0);
    AthPhase stencil = ath_phase_stencil(cosine);
    solar_phase[0] = vec4(3.0 * (1.0 + cosine * cosine) / (16.0 * ATH_PI));
    for (int k = 0; k < 4; k++) {
        solar_phase[k + 1] = ath_tabulated_phase(atlas, stencil, k);
    }
    float previous = 0.0;
    for (int i = 1; i <= count; i++) {
        float edge = length * float(i) / float(count);
        if (before + after >= 1e-5 && i < count) {
            if (i == split) {
                edge = middle;
            } else {
                bool incoming = i < split;
                float lh = after * float(i - split)
                    / max(float(count - split), 1.0);
                if (incoming) {
                    lh = before * (1.0 - float(i) / float(split));
                }
                float hh = hmin + scale * (exp(lh) - 1.0);
                float c = (h - hh) * (2.0 * ATMOS_GROUND_RADIUS_KM + h + hh);
                float root = sqrt(max(b * b - c, 0.0));
                if (incoming) {
                    edge = c / max(-b + root, 1e-20);
                } else if (b > 0.0) {
                    edge = -c / max(root + b, 1e-20);
                } else {
                    edge = -b + root;
                }
                edge = clamp(edge, previous, length);
            }
        }
        float dt = edge - previous;
        float d = (edge + previous) * 0.5;
        previous = edge;
        float hp = ath_height_at(h, mu, d);
        vec3 local_up = (up * (ATMOS_GROUND_RADIUS_KM + h) + ray * d)
            / (ATMOS_GROUND_RADIUS_KM + hp);
        AthMedium c = ath_medium(atlas, hp);
        vec4 direct = vec4(0.0);
        for (int k = 0; k < 5; k++) {
            direct += c.scattering[k] * solar_phase[k];
        }
        direct *= ath_sun_t(tau, hp, dot(local_up, sun));
        direct *= ATMOS_SOLAR;
        vec4 source_value = direct;
        source_value += ath_indirect(
            atlas, source, hp, dot(local_up, ray), dot(local_up, sun), nu, c);
        vec4 optical_depth = c.extinction * dt;
        vec4 trans = exp(-optical_depth);
        vec4 integral;
        if (all(lessThan(optical_depth, vec4(0.001)))) {
            integral = dt * (vec4(1.0) - optical_depth * 0.5
                + optical_depth * optical_depth / 6.0);
        } else {
            integral = (vec4(1.0) - trans) / max(c.extinction, vec4(1e-30));
        }
        result.light += result.transmittance * source_value * integral;
        result.transmittance *= trans;
    }
    if (hit && length >= full_length) {
        vec3 end_up = (up * (ATMOS_GROUND_RADIUS_KM + h) + ray * length)
            / ATMOS_GROUND_RADIUS_KM;
        result.light += result.transmittance
            * ath_ground_light(atlas, dot(end_up, sun));
    }
    return result;
}

AthTransport ath_integrate(
    sampler2D tau,
    sampler2D atlas,
    sampler2D source,
    vec3 ray,
    vec3 sun,
    float height_km
) {
    return ath_integrate_at(
        tau, atlas, source, ray, sun, height_km, 0.0, ATMOS_RUNTIME_STEPS);
}

// Atmosphere-attenuated solar transmittance along a full view ray. Used for the
// separately composited solar disk, mirroring `view_transmittance` in WGSL.
vec4 ath_view_transmittance(sampler2D tau, vec3 ray, float h) {
    float mu = ray.y;
    if (ath_ground(h, mu)) {
        return vec4(0.0);
    }
    if (h > ATMOS_TOP_KM) {
        float radius = ATMOS_GROUND_RADIUS_KM + h;
        float b = radius * mu;
        float c = (h - ATMOS_TOP_KM)
            * (radius + ATMOS_GROUND_RADIUS_KM + ATMOS_TOP_KM);
        float disc = b * b - c;
        if (b >= 0.0 || disc <= 0.0) {
            return vec4(1.0);
        }
        mu = -sqrt(disc) / (ATMOS_GROUND_RADIUS_KM + ATMOS_TOP_KM);
        h = ATMOS_TOP_KM;
    }
    return ath_sun_t(tau, h, mu);
}

vec3 ath_rec2020(vec4 light) {
    return ATMOS_RGB0 * light.x + ATMOS_RGB1 * light.y
        + ATMOS_RGB2 * light.z + ATMOS_RGB3 * light.w;
}

// Uniform-disk solar radiance; the model deliberately keeps the visible disk
// separate from the atmosphere's sky radiance. `radius_scale` keeps the total
// disk flux constant while changing the apparent size: the physical Sun is
// 0.53 degrees across, which reads as a tiny dot next to Minecraft's stylized
// sun, so the pack exposes a size multiplier.
vec3 ath_sun_disk(sampler2D tau, vec3 ray, vec3 sun, float h, float radius_scale) {
    float radius = ATMOS_SUN_RADIUS_RAD * radius_scale;
    float half_radius = sin(clamp(radius, 1e-5, ATH_PI * 0.5) * 0.5);
    if (dot(ray - sun, ray - sun) > 4.0 * half_radius * half_radius) {
        return vec3(0.0);
    }
    if (ath_ground(h, ray.y)) {
        return vec3(0.0);
    }
    vec4 transmittance = ath_view_transmittance(tau, ray, h);
    return ath_rec2020(transmittance * ATMOS_SOLAR)
        / (4.0 * ATH_PI * half_radius * half_radius);
}

#endif

