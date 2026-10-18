//! Column radiation budget used to drive the bulk weather model.
//!
//! This is intentionally a bulk, two-stream-style budget rather than a
//! spectral layer solver: the weather core needs diurnal surface forcing and
//! cloud albedo, while the renderer owns angular radiance. The functions keep
//! the approximations explicit and testable.

use crate::thermo::{G, P0, RD, saturation_specific_humidity};

pub const SOLAR_CONSTANT_W_M2: f64 = 1361.0;
pub const STEFAN_BOLTZMANN: f64 = 5.670_374_419e-8;

/// Cloud optical depth from liquid water path (kg/m2) and effective radius (m).
pub fn cloud_optical_depth(lwp_kg_m2: f64, r_eff_m: f64) -> f64 {
    3.0 * lwp_kg_m2.max(0.0) / (2.0 * 1000.0 * r_eff_m.max(1.0e-6))
}

/// Bulk shortwave cloud albedo. For conservative scattering the two-stream
/// albedo approaches one as optical depth grows; the `0.14` factor stands in
/// for the diffuse/direct angular integral in the visible.
pub fn cloud_albedo(lwp_kg_m2: f64, r_eff_m: f64) -> f64 {
    let tau = cloud_optical_depth(lwp_kg_m2, r_eff_m);
    (0.14 * tau / (1.0 + 0.14 * tau)).clamp(0.0, 0.92)
}

/// Top-of-atmosphere shortwave flux on a horizontal surface.
pub fn top_of_atmosphere_shortwave(sun_elevation_deg: f64) -> f64 {
    let sine = sun_elevation_deg.to_radians().sin().max(0.0);
    SOLAR_CONSTANT_W_M2 * sine
}

/// Net shortwave absorbed by the surface after clear-sky transmission,
/// cloud reflection and surface albedo.
pub fn surface_net_shortwave(
    sun_elevation_deg: f64,
    surface_albedo: f64,
    cloud_fraction: f64,
    cloud_albedo: f64,
) -> f64 {
    let toa = top_of_atmosphere_shortwave(sun_elevation_deg);
    let clear_transmission = 0.75;
    let cloud_transmission = 1.0 - cloud_fraction.clamp(0.0, 1.0) * cloud_albedo.clamp(0.0, 1.0);
    toa * clear_transmission * cloud_transmission * (1.0 - surface_albedo.clamp(0.0, 1.0))
}

/// Net longwave at the surface (positive into the surface).
pub fn surface_net_longwave(surface_temp_k: f64, air_temp_k: f64, cloud_fraction: f64) -> f64 {
    let eps_surface = 0.97;
    let eps_cloud = (0.55 + 0.35 * cloud_fraction.clamp(0.0, 1.0)).clamp(0.0, 0.98);
    let down = eps_cloud * STEFAN_BOLTZMANN * air_temp_k.powi(4);
    let up = eps_surface * STEFAN_BOLTZMANN * surface_temp_k.powi(4);
    down - up
}

/// Approximate cloud-top longwave cooling (W/m2, positive cooling).
pub fn cloud_top_cooling(cloud_fraction: f64, cloud_top_temp_k: f64) -> f64 {
    let epsilon = (0.35 + 0.45 * cloud_fraction.clamp(0.0, 1.0)).clamp(0.0, 0.9);
    epsilon * STEFAN_BOLTZMANN * cloud_top_temp_k.powi(4) * 0.12
}

/// Simple conservative two-stream reflectance/transmittance for a diffuse
/// cloud layer. Used for diagnostics and sanity tests, not for the rendered
/// image (sky-clouds owns angular transport).
pub fn two_stream_fluxes(tau: f64, asymmetry: f64) -> (f64, f64) {
    let tau = tau.max(0.0);
    let k = (1.0 - asymmetry.clamp(-0.99, 0.99)).max(1.0e-4);
    let r = k * tau / (1.0 + k * tau);
    let t = 1.0 / (1.0 + k * tau);
    (r.clamp(0.0, 1.0), t.clamp(0.0, 1.0))
}

/// Saturation-adjusted mixed-layer temperature after radiation is not applied
/// directly; this helper returns the pressure scale height for diagnostics.
pub fn scale_height(temp_k: f64) -> f64 {
    RD * temp_k / G
}

/// Column water-vapour path (kg/m2) for a two-level bulk profile.
pub fn column_water_vapour(q_mixed: f64, depth_m: f64, q_free: f64, free_depth_m: f64) -> f64 {
    let rho0 = P0 / (RD * 290.0);
    0.5 * (q_mixed + q_free) * rho0 * depth_m + q_free * rho0 * free_depth_m.max(0.0)
}

/// Clear-sky downwelling longwave using an effective emissivity; exposed for
/// the audit and for consistency checks against the sky renderer.
pub fn clear_sky_downwelling_longwave(temp_k: f64, q: f64) -> f64 {
    let e = saturation_vapour_pressure(temp_k)
        * (q / saturation_specific_humidity(temp_k, P0)).clamp(0.0, 1.0);
    let emissivity = (0.55 + 0.06 * (e / 100.0).max(0.0).ln_1p()).clamp(0.5, 0.95);
    emissivity * STEFAN_BOLTZMANN * temp_k.powi(4)
}

fn saturation_vapour_pressure(temp_k: f64) -> f64 {
    let tc = temp_k - 273.15;
    611.21 * ((18.678 - tc / 234.5) * (tc / (257.14 + tc))).exp()
}

/// Cosine of the solar zenith angle for a sun elevation.
pub fn cosine_zenith(sun_elevation_deg: f64) -> f64 {
    sun_elevation_deg.to_radians().sin().clamp(0.0, 1.0)
}

/// Diffuse fraction of shortwave (diagnostic only).
pub fn diffuse_fraction(cloud_fraction: f64) -> f64 {
    (0.1 + 0.7 * cloud_fraction.clamp(0.0, 1.0)).clamp(0.0, 0.9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optical_depth_matches_mass_extinction_relation() {
        let tau = cloud_optical_depth(0.1, 10.0e-6);
        assert!((tau - 15.0).abs() < 1.0e-9, "{tau}");
    }

    #[test]
    fn cloud_albedo_increases_with_water_path_and_saturates() {
        let a = cloud_albedo(0.01, 10.0e-6);
        let b = cloud_albedo(0.1, 10.0e-6);
        let c = cloud_albedo(10.0, 10.0e-6);
        assert!(a < b && b < c);
        assert!(c < 0.93);
    }

    #[test]
    fn no_sun_means_no_shortwave() {
        assert_eq!(top_of_atmosphere_shortwave(-5.0), 0.0);
        assert_eq!(surface_net_shortwave(-5.0, 0.2, 0.5, 0.5), 0.0);
    }

    #[test]
    fn cloud_reduces_surface_shortwave_and_increases_downwelling_longwave() {
        let clear = surface_net_shortwave(45.0, 0.2, 0.0, 0.5);
        let cloudy = surface_net_shortwave(45.0, 0.2, 0.8, 0.6);
        assert!(cloudy < clear);
        let lw_clear = surface_net_longwave(290.0, 285.0, 0.0);
        let lw_cloudy = surface_net_longwave(290.0, 285.0, 0.9);
        assert!(lw_cloudy > lw_clear);
    }

    #[test]
    fn conservative_two_stream_fluxes_sum_to_one() {
        for tau in [0.0, 0.1, 1.0, 10.0, 100.0] {
            let (r, t) = two_stream_fluxes(tau, 0.85);
            assert!((r + t - 1.0).abs() < 1.0e-9, "{tau}: {r}+{t}");
        }
    }

    #[test]
    fn scale_height_is_physical() {
        let h = scale_height(288.0);
        assert!((7_000.0..9_000.0).contains(&h), "{h}");
    }
}
