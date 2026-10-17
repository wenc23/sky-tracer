//! Fixed layout that bridges the solved `sky-realtime` fields to the shader pack.
//!
//! The runtime shader needs four groups of data:
//!   * the optical-depth table and the directional multiple-scattering source,
//!     shipped as their own raw textures (they are sampled with hardware
//!     bilinear filtering);
//!   * every small coefficient table, packed into one RGBA32F atlas so the
//!     shader pack only binds three custom textures in the full-resolution
//!     pass;
//!   * scalar constants (planet geometry, four-wave calibration, mapping
//!     coefficients) that are generated into `lib/atmosphere_data.glsl`;
//!   * the observer SkyView, which is *not* baked: the pack rebuilds it every
//!     frame with a `deferred` pass, because the sun elevation changes
//!     continuously while the fixed medium does not.

use sky_realtime::{PackExport, PackTables};

/// Side length of the observer SkyView texture rebuilt in the `deferred` pass.
pub const SKY_LUT_SIZE: u32 = 256;
/// Width of the packed coefficient atlas.
pub const ATLAS_WIDTH: u32 = 1024;
/// Profile rows: altitude, extinction, five scattering coefficients.
pub const PROFILE_ROWS: u32 = 7;

/// Row placement inside the coefficient atlas, top to bottom. Widths are the
/// natural table widths; the remaining texels in each row stay zero.
pub struct AtlasLayout {
    pub width: u32,
    pub rows: u32,
    pub row_profile: u32,
    pub row_heights: u32,
    pub row_ground: u32,
    pub row_mean: u32,
    pub row_phase: u32,
    pub row_solar: u32,
    pub row_high: u32,
    pub high_rows: u32,
}

impl AtlasLayout {
    pub fn new(t: &PackTables) -> Self {
        let heights = t.dims[0];
        let high_rows = heights - t.low_count + 1;
        let row_heights = PROFILE_ROWS;
        let row_ground = row_heights + 1;
        let row_mean = row_ground + 1;
        let row_phase = row_mean + heights;
        let row_solar = row_phase + 4;
        let row_high = row_solar + heights;
        Self {
            width: ATLAS_WIDTH,
            rows: row_high + high_rows,
            row_profile: 0,
            row_heights,
            row_ground,
            row_mean,
            row_phase,
            row_solar,
            row_high,
            high_rows,
        }
    }
}

fn f32_at(bytes: &[u8], index: usize) -> f32 {
    f32::from_le_bytes(
        bytes[index * 4..index * 4 + 4]
            .try_into()
            .expect("f32 slice"),
    )
}

/// Repack the solved coefficient tables into the RGBA32F atlas described by
/// [`AtlasLayout`]. `export` must be the matching `PackExport`.
pub fn build_atlas(export: &PackExport) -> (Vec<f32>, AtlasLayout) {
    let t = &export.tables;
    let layout = AtlasLayout::new(t);
    let mut data = vec![0.0f32; (layout.width * layout.rows * 4) as usize];
    let mut put = |row: u32, column: u32, value: [f32; 4]| {
        let base = ((row * layout.width + column) * 4) as usize;
        data[base..base + 4].copy_from_slice(&value);
    };

    for node in 0..t.profile_nodes {
        for row in 0..PROFILE_ROWS {
            let entry = t.profile[(node * PROFILE_ROWS + row) as usize];
            put(layout.row_profile + row, node, entry);
        }
    }
    for (i, &h) in t.heights.iter().enumerate() {
        put(layout.row_heights, i as u32, [h, 0.0, 0.0, 0.0]);
    }
    assert_eq!(export.ground.height, 1, "ground field is one row");
    for si in 0..t.dims[1] {
        let index = (si * 4) as usize;
        let value = [
            f32_at(&export.ground.bytes, index),
            f32_at(&export.ground.bytes, index + 1),
            f32_at(&export.ground.bytes, index + 2),
            f32_at(&export.ground.bytes, index + 3),
        ];
        put(layout.row_ground, si, value);
    }
    for hi in 0..t.dims[0] {
        for si in 0..t.dims[1] {
            let index = ((hi * export.mean.width + si) * 4) as usize;
            let value = [
                f32_at(&export.mean.bytes, index),
                f32_at(&export.mean.bytes, index + 1),
                f32_at(&export.mean.bytes, index + 2),
                f32_at(&export.mean.bytes, index + 3),
            ];
            put(layout.row_mean + hi, si, value);
        }
    }
    let phase_species = 4;
    let phase_bins = t.phase.len() as u32 / phase_species;
    for species in 0..phase_species {
        for bin in 0..phase_bins {
            let entry = t.phase[(species * phase_bins + bin) as usize];
            put(layout.row_phase + species, bin, entry);
        }
    }
    for (layer, &_h) in t.heights.iter().enumerate() {
        for j in 0..4u32 {
            let entry = t.solar[layer * 4 + j as usize];
            put(layout.row_solar + layer as u32, j, entry);
        }
    }
    for y in 0..layout.high_rows {
        for x in 0..t.dims[1] * 5 {
            let index = ((y * export.high.width + x) * 4) as usize;
            let value = [
                f32_at(&export.high.bytes, index),
                f32_at(&export.high.bytes, index + 1),
                f32_at(&export.high.bytes, index + 2),
                f32_at(&export.high.bytes, index + 3),
            ];
            put(layout.row_high + y, x, value);
        }
    }
    (data, layout)
}

fn f32_const(value: f32) -> String {
    if value.is_finite() {
        format!("{value:.9}")
    } else {
        panic!("non-finite constant in generated shader data: {value}");
    }
}

fn vec4(v: [f32; 4]) -> String {
    format!(
        "vec4({}, {}, {}, {})",
        f32_const(v[0]),
        f32_const(v[1]),
        f32_const(v[2]),
        f32_const(v[3])
    )
}

fn vec3(v: [f32; 3]) -> String {
    format!(
        "vec3({}, {}, {})",
        f32_const(v[0]),
        f32_const(v[1]),
        f32_const(v[2])
    )
}

/// Generated shader constants. The shader port assumes the fitted coordinate
/// mapping (`mapping_flags == 63`); the bake tool refuses other configs.
pub fn generated_constants(tables: &PackTables, layout: &AtlasLayout, source: [u32; 2]) -> String {
    let t = tables;
    assert_eq!(
        t.mapping_flags, 63,
        "shader pack requires the fitted mapping"
    );
    let mut out = String::new();
    out.push_str("// Generated by `cargo run -p sky-pack -- bake`; do not edit.\n");
    out.push_str("// Four-wave realtime atmosphere baked from crates/sky-realtime.\n\n");
    out.push_str(&format!(
        "#define ATMOS_TOP_KM {}\n",
        f32_const(t.top_height_km)
    ));
    out.push_str(&format!(
        "#define ATMOS_GROUND_RADIUS_KM {}\n",
        f32_const(t.ground_radius_km)
    ));
    out.push_str(&format!(
        "#define ATMOS_SUN_RADIUS_RAD {}\n",
        f32_const(t.sun_radius_rad)
    ));
    out.push_str(&format!(
        "#define ATMOS_RUNTIME_STEPS {}\n",
        t.runtime_steps
    ));
    out.push_str(&format!(
        "#define ATMOS_PATH_HEIGHT_SCALE {}\n",
        f32_const(t.path_height_scale)
    ));
    out.push_str(&format!("#define ATMOS_HEIGHT_NODES {}\n", t.dims[0]));
    out.push_str(&format!("#define ATMOS_SUN_NODES {}\n", t.dims[1]));
    out.push_str(&format!("#define ATMOS_PHASE_NODES {}\n", t.phase_nodes));
    out.push_str(&format!("#define ATMOS_LOW_COUNT {}\n", t.low_count));
    out.push_str(&format!(
        "#define ATMOS_PROFILE_NODES {}\n",
        t.profile_nodes
    ));
    out.push_str(&format!("#define ATMOS_OPTICAL_ROWS {}\n", t.dims[2]));
    out.push_str(&format!("#define ATMOS_OPTICAL_COLS {}\n", t.dims[3]));
    out.push_str("#define ATMOS_MAPPING_FLAGS 63\n");
    out.push_str(&format!(
        "#define ATMOS_PHASE_WEIGHT {}\n",
        f32_const(t.angular_mapping[0])
    ));
    out.push_str(&format!(
        "#define ATMOS_CONE_WARP {}\n",
        f32_const(t.angular_mapping[1])
    ));
    out.push_str(&format!("#define ATMOS_CONE_NODES {}\n", t.cone_nodes));
    out.push_str(&format!(
        "#define ATMOS_NEED_HIGH {}\n",
        u32::from(t.low_count < t.dims[0])
    ));
    out.push('\n');
    out.push_str(&format!("#define ATLAS_WIDTH {}\n", layout.width));
    out.push_str(&format!("#define ATLAS_ROWS {}\n", layout.rows));
    out.push_str(&format!(
        "#define ATLAS_ROW_PROFILE {}\n",
        layout.row_profile
    ));
    out.push_str(&format!(
        "#define ATLAS_ROW_HEIGHTS {}\n",
        layout.row_heights
    ));
    out.push_str(&format!("#define ATLAS_ROW_GROUND {}\n", layout.row_ground));
    out.push_str(&format!("#define ATLAS_ROW_MEAN {}\n", layout.row_mean));
    out.push_str(&format!("#define ATLAS_ROW_PHASE {}\n", layout.row_phase));
    out.push_str(&format!("#define ATLAS_ROW_SOLAR {}\n", layout.row_solar));
    out.push_str(&format!("#define ATLAS_ROW_HIGH {}\n", layout.row_high));
    out.push_str(&format!("#define ATLAS_HIGH_ROWS {}\n", layout.high_rows));
    out.push('\n');
    out.push_str(&format!("#define ATMOS_SOURCE_WIDTH {}\n", source[0]));
    out.push_str(&format!("#define ATMOS_SOURCE_HEIGHT {}\n", source[1]));
    out.push_str(&format!("#define ATMOS_SKY_LUT_SIZE {}\n", SKY_LUT_SIZE));
    out.push('\n');
    out.push_str(&format!(
        "const vec4 ATMOS_SOLAR = {};\n",
        vec4(t.solar_irradiance)
    ));
    for k in 0..4 {
        out.push_str(&format!("const vec3 ATMOS_RGB{k} = {};\n", vec3(t.rgb[k])));
    }
    for (j, segment) in t.optical_segments.iter().enumerate() {
        out.push_str(&format!("const vec4 ATMOS_SEG{j} = {};\n", vec4(*segment)));
    }
    out
}
