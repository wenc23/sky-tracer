//! Cloud optical properties and phase functions used by the real-time shader.
//!
//! Visible water clouds are close to conservative scatterers: extinction is
//! derived from liquid water content and effective radius, the single
//! scattering albedo is ~0.999, and the asymmetry parameter is 0.84-0.88.
//! The phase function is a two-lobe Henyey-Greenstein fit. This is a v1
//! approximation; a tabulated Mie/Hu-Stamnes table is a documented follow-up.

use std::f64::consts::PI;

pub const WATER_DENSITY_KG_M3: f64 = 1000.0;
pub const VISIBLE_SINGLE_SCATTERING_ALBEDO: f64 = 0.999;

/// Extinction coefficient (1/m) from liquid water content (kg/m3) and
/// effective radius (m): sigma = 3 LWC / (2 rho_w r_eff).
pub fn water_extinction(lwc_kg_m3: f64, reff_m: f64) -> f64 {
    let lwc = lwc_kg_m3.max(0.0);
    let reff = reff_m.clamp(1.0e-7, 2.0e-4);
    3.0 * lwc / (2.0 * WATER_DENSITY_KG_M3 * reff)
}

/// Asymmetry parameter for visible water/ice clouds.
pub fn asymmetry_parameter(reff_m: f64, ice_fraction: f64) -> f64 {
    let reff_um = (reff_m * 1.0e6).clamp(2.0, 80.0);
    let water = (0.85 - 0.02 * (reff_um / 10.0).log2()).clamp(0.78, 0.88);
    let ice = 0.78;
    (water * (1.0 - ice_fraction.clamp(0.0, 1.0)) + ice * ice_fraction.clamp(0.0, 1.0))
        .clamp(0.0, 0.95)
}

/// Henyey-Greenstein phase function, normalized so the integral over the
/// sphere is one.
pub fn henyey_greenstein(cos_theta: f64, g: f64) -> f64 {
    let g = g.clamp(-0.98, 0.98);
    let c = cos_theta.clamp(-1.0, 1.0);
    let denom = (1.0 + g * g - 2.0 * g * c).max(1.0e-9);
    (1.0 - g * g) / (4.0 * PI * denom * denom.sqrt())
}

/// Two-lobe phase: 75% near the physical asymmetry, 25% in a broader
/// back/forward lobe. The mean cosine is close to the requested `g`.
pub fn dual_lobe_phase(cos_theta: f64, g: f64) -> f64 {
    let g1 = (g + 0.03).clamp(-0.95, 0.95);
    let g2 = (g - 0.18).clamp(-0.95, 0.95);
    0.75 * henyey_greenstein(cos_theta, g1) + 0.25 * henyey_greenstein(cos_theta, g2)
}

/// Delta-Eddington scaling of a forward peak: returns (scaled_g, forward
/// fraction). The forward fraction is assumed to be handled by the sun NEE
/// term; the residual phase uses the scaled asymmetry.
pub fn delta_eddington_scaling(g: f64) -> (f64, f64) {
    let f = (g * g).clamp(0.0, 0.99);
    let scaled = ((g - f) / (1.0 - f).max(1.0e-6)).clamp(-0.95, 0.95);
    (scaled, f)
}

/// Multiple-scattering octave parameters: extinction scale and phase
/// asymmetry for order `k` (1-based). Fixed in v1, not yet fitted to a full
/// reference grid; documented as such.
pub fn octave_parameters(g: f64, octave: u32) -> (f64, f64, f64) {
    let k = octave.max(1) as f64;
    let extinction_scale = 0.5f64.powf(k);
    let weight = 0.5f64.powf(k);
    let phase_g = g.powf(k + 1.0);
    (extinction_scale, weight, phase_g)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extinction_matches_standard_relation() {
        let sigma = water_extinction(0.3e-3, 10.0e-6);
        assert!((sigma - 0.045).abs() < 1.0e-12, "{sigma}");
    }

    #[test]
    fn phase_functions_integrate_to_one() {
        for g in [0.0, 0.5, 0.85, 0.9] {
            let mut sum = 0.0;
            let n = 20_000;
            for i in 0..n {
                let c = -1.0 + 2.0 * (i as f64 + 0.5) / n as f64;
                sum += henyey_greenstein(c, g) * 2.0 * PI * (2.0 / n as f64);
            }
            assert!((sum - 1.0).abs() < 1.0e-4, "{g}: {sum}");
        }
    }

    #[test]
    fn dual_lobe_is_positive_and_forward_peaked() {
        let g = 0.85;
        assert!(dual_lobe_phase(1.0, g) > dual_lobe_phase(-1.0, g));
        assert!(dual_lobe_phase(0.0, g) > 0.0);
    }

    #[test]
    fn delta_scaling_removes_forward_peak() {
        let (g, f) = delta_eddington_scaling(0.85);
        assert!(f > 0.7);
        assert!(g.abs() < 0.85);
    }

    #[test]
    fn asymmetry_is_in_physical_range() {
        for reff in [2.0e-6, 10.0e-6, 40.0e-6] {
            let g = asymmetry_parameter(reff, 0.0);
            assert!((0.7..=0.92).contains(&g), "{reff}: {g}");
        }
    }

    #[test]
    fn octave_parameters_decay() {
        let (e1, w1, g1) = octave_parameters(0.85, 1);
        let (e2, w2, g2) = octave_parameters(0.85, 2);
        assert!(e1 > e2 && w1 > w2 && g1 > g2);
    }
}
