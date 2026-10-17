//! Offline handling of the generated shader pack: reading baked texture tables,
//! parsing generated constants, expanding the `#include` directives that
//! OptiFine/Iris provide but plain OpenGL does not.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct TableFile {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Bytes per texel: 8 for RGBA16F, 16 for RGBA32F.
    pub bytes_per_texel: u32,
}

impl TableFile {
    pub fn bytes(&self) -> Result<Vec<u8>, String> {
        let data =
            std::fs::read(&self.path).map_err(|e| format!("read {}: {e}", self.path.display()))?;
        let expected = (self.width * self.height) as usize * self.bytes_per_texel as usize;
        if data.len() != expected {
            return Err(format!(
                "{}: {} bytes, expected {expected} for {}x{}",
                self.path.display(),
                data.len(),
                self.width,
                self.height
            ));
        }
        Ok(data)
    }
}

pub struct PackData {
    pub shaders: PathBuf,
    #[allow(dead_code)]
    pub constants: HashMap<String, String>,
    pub lut_size: u32,
    pub runtime_steps: u32,
    pub tau: TableFile,
    pub atlas: TableFile,
    pub source: TableFile,
    pub aerosol_scale: f32,
    pub ground_albedo: f32,
}

impl PackData {
    pub fn load(root: &Path) -> Result<Self, String> {
        let shaders = root.join("shaders");
        if !shaders.is_dir() {
            return Err(format!(
                "{} is not a shader pack (missing shaders/)",
                root.display()
            ));
        }
        let data_path = shaders.join("lib/atmosphere_data.glsl");
        let data_text = std::fs::read_to_string(&data_path)
            .map_err(|e| format!("read {}: {e}", data_path.display()))?;
        let constants = parse_defines(&data_text);
        let lut_size = get_u32(&constants, "ATMOS_SKY_LUT_SIZE")?;
        let runtime_steps = get_u32(&constants, "ATMOS_RUNTIME_STEPS")?;
        let table = |name: &str,
                     width: &str,
                     height: &str,
                     bytes_per_texel: u32|
         -> Result<TableFile, String> {
            Ok(TableFile {
                path: shaders.join("textures").join(name),
                width: get_u32(&constants, width)?,
                height: get_u32(&constants, height)?,
                bytes_per_texel,
            })
        };
        let tau = table("tau.bin", "ATMOS_OPTICAL_COLS", "ATMOS_OPTICAL_ROWS", 8)?;
        let atlas = table("atlas.bin", "ATLAS_WIDTH", "ATLAS_ROWS", 16)?;
        let source = table("source.bin", "ATMOS_SOURCE_WIDTH", "ATMOS_SOURCE_HEIGHT", 8)?;
        let manifest_text = std::fs::read_to_string(root.join("manifest.json"))
            .map_err(|e| format!("read manifest.json: {e}"))?;
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest_text).map_err(|e| format!("manifest.json: {e}"))?;
        let atmosphere = &manifest["atmosphere"];
        let aerosol_scale = atmosphere["aerosol_scale"]
            .as_f64()
            .ok_or("manifest: missing atmosphere.aerosol_scale")?
            as f32;
        let ground_albedo = atmosphere["ground_albedo"]
            .as_f64()
            .ok_or("manifest: missing atmosphere.ground_albedo")?
            as f32;
        Ok(Self {
            shaders,
            tau,
            atlas,
            source,
            constants,
            lut_size,
            runtime_steps,
            aerosol_scale,
            ground_albedo,
        })
    }

    pub fn shader_text(&self, relative: &str) -> Result<String, String> {
        let path = self.shaders.join(relative);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Ok(text.trim_start_matches('\u{feff}').to_owned())
    }

    /// Expand `#include` directives and splice extra defines after `#version`.
    /// `strip` entries are replaced with a commented-out form, which lets the
    /// harness turn pack shader options on or off without editing the shipped
    /// file.
    pub fn prepare_fragment(
        &self,
        relative: &str,
        defines: &[&str],
        strip: &[&str],
    ) -> Result<String, String> {
        let mut text = self.shader_text(relative)?;
        for pattern in strip {
            text = text.replace(pattern, &format!("// {pattern}"));
        }
        let expanded = expand_includes(&text, &self.shaders, 0)?;
        let newline = expanded
            .find('\n')
            .ok_or_else(|| format!("{relative}: no newline after #version"))?;
        let first_line = &expanded[..newline];
        if !first_line.trim_start().starts_with("#version") {
            return Err(format!("{relative}: missing #version"));
        }
        let mut injected = String::new();
        injected.push_str(first_line);
        injected.push('\n');
        for define in defines {
            injected.push_str(define);
            injected.push('\n');
        }
        injected.push_str(&expanded[newline + 1..]);
        Ok(injected)
    }
}

pub fn parse_defines(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("#define ") else {
            continue;
        };
        let mut parts = rest.splitn(2, char::is_whitespace);
        if let Some(name) = parts.next() {
            let value = parts.next().unwrap_or("1").trim().to_owned();
            map.insert(name.to_owned(), value);
        }
    }
    map
}

pub fn get_u32(map: &HashMap<String, String>, name: &str) -> Result<u32, String> {
    map.get(name)
        .ok_or_else(|| format!("missing generated constant {name}"))?
        .parse::<u32>()
        .map_err(|e| format!("constant {name}: {e}"))
}

pub fn expand_text(text: &str, root: &Path) -> Result<String, String> {
    expand_includes(text, root, 0)
}

fn expand_includes(text: &str, root: &Path, depth: u32) -> Result<String, String> {
    if depth > 32 {
        return Err("include depth exceeded".into());
    }
    let mut out = String::with_capacity(text.len() + 1024);
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("#include") {
            let rest = rest.trim();
            let relative = rest
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .ok_or_else(|| format!("malformed include: {line}"))?;
            let path = if let Some(absolute) = relative.strip_prefix('/') {
                root.join(absolute)
            } else {
                root.join(relative)
            };
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("include {}: {e}", path.display()))?;
            let content = content.trim_start_matches('\u{feff}');
            let expanded = expand_includes(content, root, depth + 1)?;
            out.push_str(&expanded);
            if !expanded.ends_with('\n') {
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}
