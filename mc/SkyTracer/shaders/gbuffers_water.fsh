#version 330 compatibility

uniform sampler2D gtexture;
uniform sampler2D lightmap;
uniform float alphaTestRef;

uniform vec3 fogColor;
uniform float fogStart;
uniform float fogEnd;
uniform float fogDensity;
uniform int fogMode;

in vec2 texcoord;
in vec2 lmcoord;
in vec4 tint;
in vec3 normal;
in float view_distance;

/* RENDERTARGETS: 0 */
layout(location = 0) out vec4 color;

void main() {
    vec4 albedo = texture(gtexture, texcoord) * tint;
    albedo *= texture(lightmap, lmcoord);
    if (albedo.a < alphaTestRef) {
        discard;
    }
    float fog_factor;
    if (fogMode == 2049) {
        float x = view_distance * fogDensity;
        fog_factor = exp(-x * x);
    } else if (fogMode == 2048) {
        fog_factor = exp(-view_distance * fogDensity);
    } else {
        fog_factor = (fogEnd - view_distance) / max(fogEnd - fogStart, 1e-6);
    }
    fog_factor = clamp(fog_factor, 0.0, 1.0);
    color = vec4(mix(fogColor, albedo.rgb, fog_factor), albedo.a);
}
