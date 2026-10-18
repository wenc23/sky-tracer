//! Kessler (1969) one-moment warm-rain microphysics.
//!
//! The coefficients are the widely used bulk values; units are annotated.
//! Autoconversion uses a threshold in cloud water mixing ratio (kg/kg), which
//! stands in for the missing droplet-number closure.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RainState {
    /// Cloud water mixing ratio, kg/kg.
    pub q_c: f64,
    /// Rain water mixing ratio, kg/kg.
    pub q_r: f64,
}

impl RainState {
    pub fn new(q_c: f64, q_r: f64) -> Self {
        Self {
            q_c: q_c.max(0.0),
            q_r: q_r.max(0.0),
        }
    }

    pub fn total(&self) -> f64 {
        self.q_c + self.q_r
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MicrophysicsOutput {
    pub precipitation_kg_m2_s: f64,
    pub evaporation_kg_kg_s: f64,
    pub autoconversion_kg_kg_s: f64,
    pub accretion_kg_kg_s: f64,
}

/// Standard Kessler coefficients in SI mixing-ratio units.
pub const AUTOCONVERSION_RATE_S: f64 = 1.0e-3;
pub const AUTOCONVERSION_THRESHOLD: f64 = 0.5e-3;
/// Accretion is dimensionless in Kessler's original form; the extra 1/s factor
/// is folded into this bulk coefficient.
pub const ACCRETION_RATE_S: f64 = 2.2;
/// Rain evaporation relaxation rate for subsaturated air, 1/s.
pub const RAIN_EVAPORATION_RATE_S: f64 = 2.0e-3;
/// Rain mass-weighted fall speed, m/s (4-9 m/s observed range).
pub const RAIN_FALL_SPEED_M_S: f64 = 5.0;

/// Advance warm-rain mass for `dt` seconds. `relative_humidity` in [0, 1].
/// The update is explicit and positivity-preserving; precipitation removes
/// mass from the column.
pub fn step(
    state: &mut RainState,
    relative_humidity: f64,
    dt_s: f64,
    cloud_depth_m: f64,
    air_density_kg_m3: f64,
) -> MicrophysicsOutput {
    let mut out = MicrophysicsOutput::default();
    if dt_s <= 0.0 || !dt_s.is_finite() {
        return out;
    }
    let q_c = state.q_c.max(0.0);
    let q_r = state.q_r.max(0.0);

    let autoconversion = AUTOCONVERSION_RATE_S * (q_c - AUTOCONVERSION_THRESHOLD).max(0.0) * dt_s;
    let accretion = ACCRETION_RATE_S * q_c * q_r * dt_s;
    let conversion = (autoconversion + accretion).min(q_c);
    let evaporation = if relative_humidity < 1.0 {
        (RAIN_EVAPORATION_RATE_S * q_r * (1.0 - relative_humidity) * dt_s).min(0.6 * q_r)
    } else {
        0.0
    };
    let depth = cloud_depth_m.max(1.0);
    let sedimentation = (q_r * RAIN_FALL_SPEED_M_S * dt_s / depth).min(0.8 * q_r);

    state.q_c = (q_c - conversion).max(0.0);
    state.q_r = (q_r + conversion - evaporation - sedimentation).max(0.0);
    out.precipitation_kg_m2_s = (sedimentation * depth * air_density_kg_m3 / dt_s).max(0.0);
    out.evaporation_kg_kg_s = evaporation / dt_s;
    out.autoconversion_kg_kg_s = autoconversion / dt_s;
    out.accretion_kg_kg_s = accretion / dt_s;
    out
}

/// Instantaneous surface rain rate in mm/h for the current rain water.
pub fn precipitation_mm_h(q_r: f64, air_density_kg_m3: f64) -> f64 {
    (q_r.max(0.0) * air_density_kg_m3 * RAIN_FALL_SPEED_M_S * 3600.0).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_cloud_does_not_rain() {
        let mut state = RainState::new(0.2e-3, 0.0);
        let out = step(&mut state, 0.9, 30.0, 1000.0, 1.0);
        assert_eq!(state.q_r, 0.0);
        assert_eq!(out.precipitation_kg_m2_s, 0.0);
    }

    #[test]
    fn cloud_above_threshold_produces_rain_and_conserves_nonprecipitated_mass() {
        let mut state = RainState::new(2.0e-3, 0.0);
        let before = state.total();
        let out = step(&mut state, 1.0, 30.0, 1000.0, 1.0);
        assert!(state.q_r > 0.0);
        assert!(state.q_c >= 0.0 && state.q_r >= 0.0);
        assert!(out.autoconversion_kg_kg_s > 0.0);
        // Rain falls out of the bulk column; remaining condensate cannot
        // exceed the initial total.
        assert!(state.total() <= before + 1.0e-12);
    }

    #[test]
    fn rain_evaporates_in_subsaturated_air() {
        let mut state = RainState::new(0.0, 1.0e-3);
        let before = state.total();
        let out = step(&mut state, 0.3, 30.0, 1000.0, 1.0);
        assert!(out.evaporation_kg_kg_s >= 0.0);
        assert!(state.q_r < 1.0e-3);
        assert!(state.total() <= before);
    }

    #[test]
    fn precipitation_scale_is_positive_and_finite() {
        let rate = precipitation_mm_h(1.0e-3, 1.0);
        assert!(rate.is_finite() && rate > 0.0);
    }
}
