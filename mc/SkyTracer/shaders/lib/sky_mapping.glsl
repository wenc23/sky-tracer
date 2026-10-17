#ifndef SKY_TRACER_SKY_MAPPING_GLSL
#define SKY_TRACER_SKY_MAPPING_GLSL

// Fixed analytic SkyView coordinate warp shared by the rebuild and lookup
// passes. Elevation rows are dense near the horizon, azimuth columns are dense
// near the Sun:
//
//   elevation: sin(e) = sign(2v-1) * (2v-1)^2
//   azimuth:   u = sin(phi/2), phi = angle from the Sun's azimuth half-plane

float sky_lut_sin_elevation(float v) {
    float t = clamp(2.0 * v - 1.0, -1.0, 1.0);
    return sign(t) * t * t;
}

float sky_lut_v_from_sin(float sin_elevation) {
    float s = clamp(sin_elevation, -1.0, 1.0);
    return 0.5 + 0.5 * sign(s) * sqrt(abs(s));
}

#endif

