#version 330 compatibility

out vec2 texcoord;
out vec2 lmcoord;
out vec4 tint;
out vec3 normal;
out float view_distance;

void main() {
    gl_Position = ftransform();
    texcoord = (gl_TextureMatrix[0] * gl_MultiTexCoord0).xy;
    lmcoord = (gl_TextureMatrix[1] * gl_MultiTexCoord1).xy;
    tint = gl_Color;
    normal = gl_NormalMatrix * gl_Normal;
    view_distance = length((gl_ModelViewMatrix * gl_Vertex).xyz);
}
