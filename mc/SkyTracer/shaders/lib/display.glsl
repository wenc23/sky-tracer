#ifndef SKY_TRACER_DISPLAY_GLSL
#define SKY_TRACER_DISPLAY_GLSL

// Display transform ported from apps/sky-demo/src/shaders/reinhard_gamut.wgsl
// and present_texture.wgsl:
//   scene Rec.2020 -> exposure -> linear sRGB -> Enhanced Reinhard
//   (Oklab gamut mapping) -> sRGB transfer.
// The same curve the demo uses for the realtime atmosphere is kept so the
// shader pack's colors match the research renderer rather than a filmic look.

const float DISPLAY_RGB_HEADROOM = 0.99999;
const float DISPLAY_MIDDLE_GRAY = 0.18;
const vec3 DISPLAY_RED_ROW = vec3(4.0767416621, -3.3077115913, 0.2309699292);
const vec3 DISPLAY_GREEN_ROW = vec3(-1.2684380046, 2.6097574011, -0.3413193965);
const vec3 DISPLAY_BLUE_ROW = vec3(-0.0041960863, -0.7034186147, 1.7076147010);

vec3 display_rgb_to_oklab(vec3 color) {
    float l = 0.4122214708 * color.r + 0.5363325363 * color.g + 0.0514459929 * color.b;
    float m = 0.2119034982 * color.r + 0.6806995451 * color.g + 0.1073969566 * color.b;
    float s = 0.0883024619 * color.r + 0.2817188376 * color.g + 0.6299787005 * color.b;
    vec3 lms = pow(vec3(l, m, s), vec3(1.0 / 3.0));
    return vec3(
        0.2104542553 * lms.x + 0.7936177850 * lms.y - 0.0040720468 * lms.z,
        1.9779984951 * lms.x - 2.4285922050 * lms.y + 0.4505937099 * lms.z,
        0.0259040371 * lms.x + 0.7827717662 * lms.y - 0.8086757660 * lms.z);
}

vec3 display_oklab_to_rgb(vec3 color) {
    float l_root = color.x + 0.3963377774 * color.y + 0.2158037573 * color.z;
    float m_root = color.x - 0.1055613458 * color.y - 0.0638541728 * color.z;
    float s_root = color.x - 0.0894841775 * color.y - 1.2914855480 * color.z;
    vec3 lms = vec3(l_root * l_root * l_root, m_root * m_root * m_root, s_root * s_root * s_root);
    return vec3(
        dot(DISPLAY_RED_ROW, lms),
        dot(DISPLAY_GREEN_ROW, lms),
        dot(DISPLAY_BLUE_ROW, lms));
}

float display_shoulder_coefficient(float overexposure) {
    float scale = overexposure / (overexposure - DISPLAY_MIDDLE_GRAY);
    return (scale * scale - 1.0) / DISPLAY_MIDDLE_GRAY;
}

float display_map_lightness(float lightness, float overexposure) {
    float brightness = lightness * lightness * lightness;
    float x = display_shoulder_coefficient(overexposure) * brightness;
    float inverse_root = inversesqrt(1.0 + x);
    float mapped = overexposure * x * inverse_root * inverse_root / (1.0 + inverse_root);
    return pow(mapped, 1.0 / 3.0);
}

vec3 display_root_direction(vec2 hue) {
    return vec3(
        0.3963377774 * hue.x + 0.2158037573 * hue.y,
        -0.1055613458 * hue.x - 0.0638541728 * hue.y,
        -0.0894841775 * hue.x - 1.2914855480 * hue.y);
}

float display_max_saturation(vec2 hue, vec3 direction) {
    float k0;
    float k1;
    float k2;
    float k3;
    float k4;
    vec3 rgb_row;
    if (-1.88170328 * hue.x - 0.80936493 * hue.y > 1.0) {
        k0 = 1.19086277;
        k1 = 1.76576728;
        k2 = 0.59662641;
        k3 = 0.75515197;
        k4 = 0.56771245;
        rgb_row = DISPLAY_RED_ROW;
    } else if (1.81444104 * hue.x - 1.19445276 * hue.y > 1.0) {
        k0 = 0.73956515;
        k1 = -0.45954404;
        k2 = 0.08285427;
        k3 = 0.12541070;
        k4 = 0.14503204;
        rgb_row = DISPLAY_GREEN_ROW;
    } else {
        k0 = 1.35733652;
        k1 = -0.00915799;
        k2 = -1.15130210;
        k3 = -0.50559606;
        k4 = 0.00692167;
        rgb_row = DISPLAY_BLUE_ROW;
    }
    float saturation = k0 + k1 * hue.x + k2 * hue.y
        + k3 * hue.x * hue.x + k4 * hue.x * hue.y;
    vec3 roots = vec3(1.0) + saturation * direction;
    vec3 lms = roots * roots * roots;
    vec3 first = 3.0 * direction * roots * roots;
    vec3 second = 6.0 * direction * direction * roots;
    float f = dot(rgb_row, lms);
    float f1 = dot(rgb_row, first);
    float f2 = dot(rgb_row, second);
    return saturation - f * f1 / (f1 * f1 - 0.5 * f * f2);
}

float display_connected_saturation(vec2 hue, float saturation) {
    vec2 blue_notch_axis = vec2(-0.10362546, -0.99461639);
    float alignment = max(dot(hue, blue_notch_axis), 0.0);
    float alignment2 = alignment * alignment;
    float alignment4 = alignment2 * alignment2;
    float alignment8 = alignment4 * alignment4;
    float alignment16 = alignment8 * alignment8;
    float alignment32 = alignment16 * alignment16;
    float alignment64 = alignment32 * alignment32;
    float alignment128 = alignment64 * alignment64;
    float alignment256 = alignment128 * alignment128;
    return min(saturation, 0.57 + (1.0 - alignment256));
}

float display_cusp_lightness(float saturation, vec3 direction) {
    vec3 roots = vec3(1.0) + saturation * direction;
    vec3 lms = roots * roots * roots;
    vec3 rgb = vec3(
        dot(DISPLAY_RED_ROW, lms),
        dot(DISPLAY_GREEN_ROW, lms),
        dot(DISPLAY_BLUE_ROW, lms));
    return pow(1.0 / max(rgb.r, max(rgb.g, rgb.b)), 1.0 / 3.0);
}

float display_refine_upper_chroma(float chroma, float lightness, vec3 direction) {
    vec3 roots = vec3(lightness) + chroma * direction;
    vec3 lms = roots * roots * roots;
    vec3 first = 3.0 * direction * roots * roots;
    vec3 second = 6.0 * direction * direction * roots;
    vec3 rgb = vec3(
        dot(DISPLAY_RED_ROW, lms),
        dot(DISPLAY_GREEN_ROW, lms),
        dot(DISPLAY_BLUE_ROW, lms));
    vec3 first_rgb = vec3(
        dot(DISPLAY_RED_ROW, first),
        dot(DISPLAY_GREEN_ROW, first),
        dot(DISPLAY_BLUE_ROW, first));
    vec3 second_rgb = vec3(
        dot(DISPLAY_RED_ROW, second),
        dot(DISPLAY_GREEN_ROW, second),
        dot(DISPLAY_BLUE_ROW, second));
    vec3 f = rgb - vec3(1.0);
    vec3 denominator = first_rgb * first_rgb - 0.5 * f * second_rgb;
    vec3 reciprocal_step = first_rgb / denominator;
    vec3 step = -f * reciprocal_step;
    if (reciprocal_step.r < 0.0) step.r = 1.0e20;
    if (reciprocal_step.g < 0.0) step.g = 1.0e20;
    if (reciprocal_step.b < 0.0) step.b = 1.0e20;
    return chroma + min(step.r, min(step.g, step.b));
}

float display_soft_min(float value, float limit, float power) {
    if (value <= 0.0 || limit <= 0.0) {
        return 0.0;
    }
    float lower = min(value, limit);
    float higher = max(value, limit);
    float ratio = lower / higher;
    return lower * pow(1.0 + pow(ratio, power), -1.0 / power);
}

float display_soft_min4(float value, float limit) {
    if (value <= 0.0 || limit <= 0.0) {
        return 0.0;
    }
    float lower = min(value, limit);
    float higher = max(value, limit);
    float ratio = lower / higher;
    float ratio2 = ratio * ratio;
    float root = sqrt(1.0 + ratio2 * ratio2);
    return lower * inversesqrt(root);
}

float display_saturation_cap(float lightness, float maximum_saturation, vec3 direction) {
    if (lightness <= 0.0) {
        return maximum_saturation;
    }
    if (lightness >= 1.0) {
        return 0.0;
    }
    float cusp_lightness = display_cusp_lightness(maximum_saturation, direction);
    float black_chroma = lightness * maximum_saturation;
    float white_chroma = cusp_lightness * maximum_saturation
        * (1.0 - lightness) / (1.0 - cusp_lightness);
    white_chroma = display_refine_upper_chroma(white_chroma, lightness, direction);
    float t = clamp((lightness - cusp_lightness) / (1.0 - cusp_lightness), 0.0, 1.0);
    float shoulder = t * (1.0 - t);
    white_chroma *= 1.0 - 0.0035 * 16.0 * shoulder * shoulder;
    float rounded_chroma = display_soft_min4(black_chroma, white_chroma);
    return max(rounded_chroma / lightness, 0.0);
}

float display_chroma_retention(float lightness) {
    float lightness2 = lightness * lightness;
    float lightness4 = lightness2 * lightness2;
    float lightness8 = lightness4 * lightness4;
    return 1.0 - lightness8 * lightness4;
}

float display_rounding_power(float lightness) {
    float endpoint_distance = lightness * (1.0 - lightness);
    return 32.0 - 256.0 * endpoint_distance * endpoint_distance;
}

vec3 display_map(vec3 color, float overexposure) {
    vec3 oklab = display_rgb_to_oklab(max(color, vec3(0.0)));
    if (oklab.x <= 0.0) {
        return vec3(0.0);
    }
    float output_lightness = display_map_lightness(oklab.x, overexposure);
    float input_chroma = length(oklab.yz);
    if (input_chroma <= 1.0e-8) {
        return DISPLAY_RGB_HEADROOM
            * display_oklab_to_rgb(vec3(output_lightness, 0.0, 0.0));
    }
    vec2 hue = oklab.yz / input_chroma;
    vec3 direction = display_root_direction(hue);
    float input_saturation = input_chroma / oklab.x;
    float maximum_saturation = display_connected_saturation(
        hue,
        display_max_saturation(hue, direction));
    float desired_saturation = input_saturation
        * display_chroma_retention(output_lightness);
    float saturation_cap = display_saturation_cap(
        output_lightness,
        maximum_saturation,
        direction);
    float output_saturation = display_soft_min(
        desired_saturation,
        saturation_cap,
        display_rounding_power(output_lightness));
    return DISPLAY_RGB_HEADROOM * display_oklab_to_rgb(vec3(
        output_lightness,
        output_lightness * output_saturation * hue));
}

vec3 display_rec2020_to_linear_srgb(vec3 rgb) {
    return vec3(
        1.6604910 * rgb.r - 0.5876411 * rgb.g - 0.0728499 * rgb.b,
        -0.1245505 * rgb.r + 1.1328999 * rgb.g - 0.0083494 * rgb.b,
        -0.0181508 * rgb.r - 0.1005789 * rgb.g + 1.1187297 * rgb.b);
}

vec3 display_srgb_encode(vec3 linear) {
    vec3 cutoff = vec3(0.0031308);
    vec3 low = 12.92 * linear;
    vec3 high = 1.055 * pow(max(linear, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return mix(high, low, lessThanEqual(linear, cutoff));
}

// Final scene-referred Rec.2020 radiance -> display-encoded sRGB, matching the
// demo's exposure/tonemap order. Exposure 0.10 is the calibrated baseline.
vec3 display_from_rec2020(vec3 rec2020, float exposure, float overexposure) {
    vec3 scene = max(rec2020 * exposure, vec3(0.0));
    vec3 linear_srgb = max(display_rec2020_to_linear_srgb(scene), vec3(0.0));
    vec3 mapped = display_map(linear_srgb, overexposure);
    return display_srgb_encode(clamp(mapped, vec3(0.0), vec3(1.0)));
}

#endif


