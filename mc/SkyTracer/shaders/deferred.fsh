#version 330 compatibility

// Observer SkyView rebuild for the four-wave atmosphere.
//
// The fixed medium (optical depth + directional multiple-scattering source) is
// baked offline by crates/sky-realtime. Only the Sun moves, so this pass
// re-evaluates the full view sphere at 256x256 once per frame. The composite
// pass then reads the LUT instead of integrating every screen pixel.

uniform sampler2D colortex4; // optical depth,  RGBA16F (custom raw texture)
uniform sampler2D colortex6; // coefficient atlas, RGBA32F (custom raw texture)
uniform sampler2D colortex7; // MS source,       RGBA16F (custom raw texture)

uniform mat4 gbufferModelViewInverse;
uniform vec3 sunPosition;
uniform float eyeAltitude;
uniform int dimension;

in vec2 texcoord;

/* RENDERTARGETS: 5 */
layout(location = 0) out vec4 sky_view;

#include "/lib/sky_lut.glsl"

// Iris/OptiFine scan this directive from the source text; the format token is
// not real GLSL, so it lives inside a block comment. The directive must be on
// its own line: Iris 1.7.6 ignores it when `const` is preceded by comment
// markers on the same line, silently leaving the buffer at RGBA8.
/*
const int colortex5Format = RGBA16F;
*/

void main() {
    if (dimension != 0 || dot(sunPosition, sunPosition) < 1e-6) {
        sky_view = vec4(0.0);
        return;
    }
    vec3 sun_world = normalize(mat3(gbufferModelViewInverse) * (sunPosition / 100.0));
    float height_km = max(eyeAltitude, 0.0) * 0.001;
    vec3 radiance = sky_lut_build_radiance(
        colortex4, colortex6, colortex7, texcoord, sun_world, height_km);
    sky_view = sky_lut_encode(radiance);
}
