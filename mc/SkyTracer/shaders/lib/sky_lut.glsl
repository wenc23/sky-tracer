#ifndef SKY_TRACER_SKY_LUT_BUILD_GLSL
#define SKY_TRACER_SKY_LUT_BUILD_GLSL

// Observer SkyView rebuild. The fixed medium is baked offline, but the Sun
// moves continuously, so the pack re-evaluates the full-sphere view at
// 256x256 in the `deferred` pass and stores log(radiance).

#include "/lib/atmosphere.glsl"
#include "/lib/sky_mapping.glsl"

// Radiance for one SkyView texel. `uv` is the normalized LUT coordinate.
vec3 sky_lut_build_radiance(
    sampler2D tau,
    sampler2D atlas,
    sampler2D source,
    vec2 uv,
    vec3 sun_world,
    float height_km
) {
    float sin_e = sky_lut_sin_elevation(uv.y);
    float cos_e = sqrt(max(1.0 - sin_e * sin_e, 0.0));
    float phi = 2.0 * asin(clamp(uv.x, 0.0, 1.0));
    vec3 ray = vec3(cos_e * sin(phi), sin_e, cos_e * cos(phi));
    // Work in the Sun's azimuth frame: the fixed medium is rotationally
    // symmetric, so only the Sun elevation enters the transport.
    float sun_sin = clamp(sun_world.y, -1.0, 1.0);
    vec3 sun = vec3(0.0, sun_sin, sqrt(max(1.0 - sun_sin * sun_sin, 0.0)));
    AthTransport transport = ath_integrate(tau, atlas, source, ray, sun, height_km);
    return ath_rec2020(transport.light);
}

vec4 sky_lut_encode(vec3 radiance) {
    return vec4(log(max(radiance, vec3(1e-30))), 1.0);
}

#endif

