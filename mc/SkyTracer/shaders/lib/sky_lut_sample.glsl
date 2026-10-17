#ifndef SKY_TRACER_SKY_LUT_SAMPLE_GLSL
#define SKY_TRACER_SKY_LUT_SAMPLE_GLSL

// SkyView sampling used by the composite pass. The LUT is rebuilt every frame
// by the `deferred` pass, so it always matches the current Sun and observer
// altitude. Radiance is decoded from the log encoding; the solar disk is added
// afterwards because it is far below one LUT texel.

#include "/lib/atmosphere.glsl"
#include "/lib/sky_mapping.glsl"

vec2 sky_lut_uv(vec3 ray, vec3 sun_world) {
    float v = sky_lut_v_from_sin(ray.y);
    float horizontal = length(ray.xz);
    vec2 sun_flat = sun_world.xz;
    float sun_flat_length = length(sun_flat);
    float cos_phi = 1.0;
    if (horizontal > 1e-6 && sun_flat_length > 1e-6) {
        cos_phi = dot(ray.xz / horizontal, sun_flat / sun_flat_length);
    }
    float phi = acos(clamp(cos_phi, -1.0, 1.0));
    float u = sin(0.5 * phi);
    return (vec2(u, v) * float(ATMOS_SKY_LUT_SIZE - 1) + 0.5)
        / float(ATMOS_SKY_LUT_SIZE);
}

vec3 sky_lut_sample(sampler2D sky_lut, vec3 ray, vec3 sun_world) {
    return exp(texture(sky_lut, sky_lut_uv(ray, sun_world)).rgb);
}

// Full sky radiance in linear Rec.2020: atmosphere plus the visible solar disk.
vec3 sky_radiance(
    sampler2D sky_lut,
    sampler2D tau,
    vec3 ray,
    vec3 sun_world,
    float height_km,
    float sun_radius_scale
) {
    vec3 radiance = sky_lut_sample(sky_lut, ray, sun_world);
    radiance += ath_sun_disk(tau, ray, sun_world, height_km, sun_radius_scale);
    return radiance;
}

#endif

