//! Bulk surface exchange and a thin surface slab.

use serde::{Deserialize, Serialize};

use crate::thermo::{CPD, LV, RD, saturation_specific_humidity};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurfaceKind {
    Ocean,
    Grass,
    Forest,
    Desert,
}

#[derive(Clone, Copy, Debug)]
pub struct SurfaceProperties {
    pub albedo: f64,
    pub heat_capacity_j_m2_k: f64,
    pub moisture_availability: f64,
    pub drag_coefficient: f64,
    pub roughness_m: f64,
}

impl SurfaceKind {
    pub fn properties(self) -> SurfaceProperties {
        match self {
            Self::Ocean => SurfaceProperties {
                albedo: 0.06,
                heat_capacity_j_m2_k: 2.0e7,
                moisture_availability: 1.0,
                drag_coefficient: 1.3e-3,
                roughness_m: 1.0e-4,
            },
            Self::Grass => SurfaceProperties {
                albedo: 0.18,
                heat_capacity_j_m2_k: 1.5e6,
                moisture_availability: 0.45,
                drag_coefficient: 2.0e-3,
                roughness_m: 0.03,
            },
            Self::Forest => SurfaceProperties {
                albedo: 0.12,
                heat_capacity_j_m2_k: 3.0e6,
                moisture_availability: 0.8,
                drag_coefficient: 5.0e-3,
                roughness_m: 0.5,
            },
            Self::Desert => SurfaceProperties {
                albedo: 0.35,
                heat_capacity_j_m2_k: 6.0e5,
                moisture_availability: 0.08,
                drag_coefficient: 2.5e-3,
                roughness_m: 0.1,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Fluxes {
    /// W/m2, positive from the surface into the atmosphere.
    pub sensible: f64,
    /// W/m2, positive evaporation.
    pub latent: f64,
    /// N/m2.
    pub momentum: f64,
}

/// Bulk aerodynamic fluxes with fixed transfer coefficients.
pub fn bulk_fluxes(
    surface: SurfaceKind,
    surface_temp_k: f64,
    air_temp_k: f64,
    air_q: f64,
    wind_speed_m_s: f64,
    pressure_pa: f64,
) -> Fluxes {
    let props = surface.properties();
    let wind = wind_speed_m_s.max(0.1);
    let rho = pressure_pa / (RD * air_temp_k.max(150.0));
    let c_h = 1.2e-3;
    let c_e = 1.2e-3 * props.moisture_availability;
    let q_surface = saturation_specific_humidity(surface_temp_k, pressure_pa);
    Fluxes {
        sensible: rho * CPD * c_h * wind * (surface_temp_k - air_temp_k),
        latent: rho * LV * c_e * wind * (q_surface - air_q),
        momentum: rho * props.drag_coefficient * wind * wind,
    }
}

/// Advance the surface skin temperature with net shortwave/longwave forcing
/// (W/m2, positive into the surface) and the bulk fluxes.
pub fn advance_surface(
    surface: SurfaceKind,
    surface_temp_k: f64,
    air_temp_k: f64,
    air_q: f64,
    wind_speed_m_s: f64,
    pressure_pa: f64,
    net_shortwave_w_m2: f64,
    net_longwave_w_m2: f64,
    dt_s: f64,
) -> f64 {
    let props = surface.properties();
    let fluxes = bulk_fluxes(
        surface,
        surface_temp_k,
        air_temp_k,
        air_q,
        wind_speed_m_s,
        pressure_pa,
    );
    let net = net_shortwave_w_m2 + net_longwave_w_m2 - fluxes.sensible - fluxes.latent;
    let capacity = props.heat_capacity_j_m2_k.max(1.0);
    (surface_temp_k + net * dt_s / capacity).clamp(180.0, 350.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmer_surface_has_positive_sensible_heat_flux() {
        let f = bulk_fluxes(SurfaceKind::Grass, 300.0, 295.0, 0.008, 5.0, 100_000.0);
        assert!(f.sensible > 0.0, "{f:?}");
        assert!(f.momentum > 0.0);
    }

    #[test]
    fn saturated_surface_over_dry_air_evaporates() {
        let f = bulk_fluxes(SurfaceKind::Ocean, 300.0, 298.0, 0.005, 6.0, 100_000.0);
        assert!(f.latent > 0.0, "{f:?}");
    }

    #[test]
    fn dry_surface_evaporates_less_than_ocean() {
        let ocean = bulk_fluxes(SurfaceKind::Ocean, 300.0, 298.0, 0.005, 6.0, 100_000.0);
        let desert = bulk_fluxes(SurfaceKind::Desert, 300.0, 298.0, 0.005, 6.0, 100_000.0);
        assert!(ocean.latent > desert.latent);
    }

    #[test]
    fn slab_heats_under_positive_net_radiation() {
        let next = advance_surface(
            SurfaceKind::Grass,
            290.0,
            290.0,
            0.008,
            3.0,
            100_000.0,
            300.0,
            -50.0,
            60.0,
        );
        assert!(next > 290.0, "{next}");
    }
}
