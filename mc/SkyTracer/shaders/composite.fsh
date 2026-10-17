#version 330 compatibility

// Final sky composite for the four-wave realtime atmosphere.
//
// Pixels with no geometry (depth == 1) are the sky: sample the observer
// SkyView rebuilt by the deferred pass, add the physical solar disk, apply the
// demo's exposure / Enhanced-Reinhard display curve and write display-encoded
// sRGB. Geometry and translucent layers are passed through untouched, so the
// pack only changes the sky and keeps vanilla terrain rendering.

uniform sampler2D colortex0; // vanilla scene color, display-encoded sRGB
uniform sampler2D colortex4; // optical depth, RGBA16F (custom raw texture)
uniform sampler2D colortex5; // observer SkyView, RGBA16F log radiance
uniform sampler2D depthtex0; // vanilla depth, 1.0 where no geometry

uniform mat4 gbufferProjectionInverse;
uniform mat4 gbufferModelViewInverse;
uniform vec3 sunPosition;
uniform float eyeAltitude;
uniform float rainStrength;
uniform int dimension;
uniform int isEyeInWater;

in vec2 texcoord;

/* RENDERTARGETS: 0 */
layout(location = 0) out vec4 fragColor;

#define SKY_EXPOSURE 1.0 // Sky exposure multiplier on the calibrated 0.10 baseline [0.5 0.75 1.0 1.5 2.0 3.0 4.0]
#define SKY_NIGHT_GAIN 1.0 // Night-sky gain; the model has no moon or stars [1.0 2.0 4.0 8.0]
#define SKY_SUN_DISK // Draw the solar disk
#define SKY_SUN_SIZE 4.0 // Solar disk size relative to the physical 0.53 degrees [1.0 2.0 3.0 4.0 6.0 8.0]
#define SKY_RAIN_DIMMING // Dim the clear-sky model while it rains

#include "/lib/sky_lut_sample.glsl"
#include "/lib/display.glsl"

const float SKY_EXPOSURE_BASELINE = 0.10;
const float SKY_REINHARD_OVEREXPOSURE = 1.1;

void main() {
    vec4 scene = texture(colortex0, texcoord);
    float depth = texture(depthtex0, texcoord).r;
    if (dimension != 0 || isEyeInWater != 0 || depth < 1.0) {
        fragColor = scene;
        return;
    }

    vec3 sun_world = normalize(mat3(gbufferModelViewInverse) * (sunPosition / 100.0));
    vec3 ndc = vec3(texcoord * 2.0 - 1.0, 1.0);
    vec4 view_position = gbufferProjectionInverse * vec4(ndc, 1.0);
    vec3 view_direction = normalize(view_position.xyz / view_position.w);
    vec3 ray = normalize(mat3(gbufferModelViewInverse) * view_direction);
    float height_km = max(eyeAltitude, 0.0) * 0.001;

    vec3 radiance = sky_lut_sample(colortex5, ray, sun_world);
#ifdef SKY_SUN_DISK
    radiance += ath_sun_disk(colortex4, ray, sun_world, height_km, SKY_SUN_SIZE);
#endif
#ifdef SKY_RAIN_DIMMING
    radiance *= mix(1.0, 0.35, clamp(rainStrength, 0.0, 1.0));
#endif
    float night = smoothstep(0.0, -0.20, sun_world.y);
    radiance *= mix(1.0, SKY_NIGHT_GAIN, night);

#ifdef SKY_PACK_LINEAR_DEBUG
    // Offline validation hook: emit scene-referred linear Rec.2020 radiance.
    fragColor = vec4(radiance, 1.0);
#else
    fragColor = vec4(display_from_rec2020(
        radiance,
        SKY_EXPOSURE_BASELINE * SKY_EXPOSURE,
        SKY_REINHARD_OVEREXPOSURE), 1.0);
#endif
}
