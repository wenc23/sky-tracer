//! Moist thermodynamics used by the bulk weather model.
//!
//! Formulas: Buck (1981) saturation vapour pressure, Bolton (1980) dew point
//! and equivalent potential temperature, dry-lift numerical LCL (an
//! approximation to Romps 2017), and a moist-adiabat CAPE/CIN integration.

pub const G: f64 = 9.806_65;
pub const RD: f64 = 287.05;
pub const RV: f64 = 461.5;
pub const CPD: f64 = 1005.0;
pub const LV: f64 = 2.501e6;
pub const EPS: f64 = RD / RV;
pub const KAPPA: f64 = RD / CPD;
pub const P0: f64 = 100_000.0;
pub const T0_K: f64 = 273.15;

/// Saturation vapour pressure over liquid water in Pa (Buck 1981).
pub fn saturation_vapour_pressure(temp_k: f64) -> f64 {
    let tc = temp_k - T0_K;
    611.21 * ((18.678 - tc / 234.5) * (tc / (257.14 + tc))).exp()
}

/// Saturation mixing ratio (kg/kg) at temperature and pressure.
pub fn saturation_mixing_ratio(temp_k: f64, pressure_pa: f64) -> f64 {
    let es = saturation_vapour_pressure(temp_k).min(0.99 * pressure_pa);
    EPS * es / (pressure_pa - es)
}

/// Saturation specific humidity (kg/kg).
pub fn saturation_specific_humidity(temp_k: f64, pressure_pa: f64) -> f64 {
    let r = saturation_mixing_ratio(temp_k, pressure_pa);
    r / (1.0 + r)
}

/// Vapour pressure from specific humidity and pressure.
pub fn vapour_pressure(q: f64, pressure_pa: f64) -> f64 {
    let q = q.clamp(0.0, 0.999);
    q * pressure_pa / (EPS + (1.0 - EPS) * q)
}

/// Specific humidity from vapour pressure.
pub fn specific_humidity(e: f64, pressure_pa: f64) -> f64 {
    let r = EPS * e / (pressure_pa - e).max(1.0);
    r / (1.0 + r)
}

/// Relative humidity in [0, 1.2] (supersaturation is clipped only for display
/// and cloud diagnostics, not for the thermodynamic functions).
pub fn relative_humidity(q: f64, temp_k: f64, pressure_pa: f64) -> f64 {
    let qs = saturation_specific_humidity(temp_k, pressure_pa);
    if qs <= 0.0 {
        0.0
    } else {
        (q / qs).clamp(0.0, 1.2)
    }
}

/// Dew point in K from vapour pressure (Bolton 1980 inverse form).
pub fn dew_point_from_vapour_pressure(e_pa: f64) -> f64 {
    let e_hpa = (e_pa / 100.0).max(1.0e-6);
    let ln = (e_hpa / 6.112).ln();
    let td_c = 243.5 * ln / (17.67 - ln);
    td_c + T0_K
}

pub fn dew_point(q: f64, pressure_pa: f64) -> f64 {
    dew_point_from_vapour_pressure(vapour_pressure(q, pressure_pa))
}

/// Potential temperature (K) from temperature (K) and pressure (Pa).
pub fn potential_temperature(temp_k: f64, pressure_pa: f64) -> f64 {
    temp_k * (P0 / pressure_pa).powf(KAPPA)
}

/// Temperature (K) from potential temperature and pressure.
pub fn temperature_from_theta(theta_k: f64, pressure_pa: f64) -> f64 {
    theta_k * (pressure_pa / P0).powf(KAPPA)
}

/// Virtual potential temperature including condensate loading.
pub fn virtual_potential_temperature(theta_k: f64, q_v: f64, q_l: f64) -> f64 {
    theta_k * (1.0 + 0.608 * q_v - q_l)
}

/// Equivalent potential temperature (Bolton 1980, approximate).
pub fn equivalent_potential_temperature(temp_k: f64, pressure_pa: f64, q: f64) -> f64 {
    let theta = potential_temperature(temp_k, pressure_pa);
    let e = vapour_pressure(q, pressure_pa);
    let tl = if e > 1.0 {
        2840.0 / (3.5 * temp_k.ln() - (e / 100.0).ln() - 4.805) + 55.0
    } else {
        temp_k
    };
    let r_gkg = (q / (1.0 - q).max(1.0e-6)) * 1000.0;
    theta * ((3.376 / tl.max(1.0) - 0.00254) * r_gkg * (1.0 + 0.81 * r_gkg)).exp()
}

/// Moist adiabatic lapse rate (K/m) for saturated air.
pub fn moist_lapse_rate(temp_k: f64, pressure_pa: f64) -> f64 {
    let qs = saturation_mixing_ratio(temp_k, pressure_pa);
    G * (1.0 + LV * qs / (RD * temp_k)) / (CPD + LV * LV * qs * EPS / (RD * temp_k * temp_k))
}

/// Dry-lift lifting condensation level (m above the parcel origin).
///
/// This solves `q_s(T_dry(z), p_dry(z)) = q` along a dry adiabat; it is a
/// high-accuracy approximation to the exact Romps (2017) expression and is
/// deliberately cheaper than a Lambert-W evaluation at every column.
pub fn lcl_height(temp_k: f64, q: f64, pressure_pa: f64) -> f64 {
    if !(temp_k.is_finite() && pressure_pa.is_finite() && pressure_pa > 0.0)
        || !q.is_finite()
        || q <= 0.0
    {
        return 0.0;
    }
    let qs0 = saturation_specific_humidity(temp_k, pressure_pa);
    if q >= qs0 {
        return 0.0;
    }
    let mut lo = 0.0;
    let mut hi = 8_000.0;
    for _ in 0..20 {
        let mid = 0.5 * (lo + hi);
        let t = temp_k - G / CPD * mid;
        let p = pressure_pa * (t / temp_k).powf(CPD / RD);
        let qs = saturation_specific_humidity(t.max(150.0), p.max(1.0));
        if qs > q {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// LCL temperature (K) corresponding to `lcl_height`.
pub fn lcl_temperature(temp_k: f64, q: f64, pressure_pa: f64) -> f64 {
    let z = lcl_height(temp_k, q, pressure_pa);
    temp_k - G / CPD * z
}

/// One environmental level for CAPE/CIN.
#[derive(Clone, Copy, Debug)]
pub struct Level {
    pub height_m: f64,
    pub temp_k: f64,
    pub q: f64,
    pub pressure_pa: f64,
}

/// Positive and negative buoyancy areas (J/kg) for a parcel lifted from
/// `surface` through `levels`.
pub fn cape_cin(levels: &[Level], surface: Level) -> (f64, f64) {
    if levels.len() < 2 {
        return (0.0, 0.0);
    }
    let mut cape = 0.0;
    let mut cin = 0.0;
    let lcl_z = lcl_height(surface.temp_k, surface.q, surface.pressure_pa);
    let mut parcel_t = surface.temp_k;
    let mut parcel_q = surface.q;
    let mut parcel_p = surface.pressure_pa;
    let mut parcel_z = surface.height_m;
    for level in levels {
        let dz = (level.height_m - parcel_z).max(0.0);
        if dz == 0.0 {
            continue;
        }
        let steps = (dz / 100.0).ceil().max(1.0) as u32;
        let dz_step = dz / steps as f64;
        for _ in 0..steps {
            let saturated = parcel_z + dz_step > lcl_z;
            let t_mean = 0.5 * (parcel_t + level.temp_k);
            parcel_p = (parcel_p * (-G * dz_step / (RD * t_mean.max(150.0))).exp()).max(1.0);
            if saturated {
                let gamma = moist_lapse_rate(parcel_t, parcel_p);
                parcel_t -= gamma * dz_step;
                parcel_q = saturation_specific_humidity(parcel_t, parcel_p);
            } else {
                parcel_t -= G / CPD * dz_step;
            }
            parcel_z += dz_step;
            let theta_v_parcel = virtual_potential_temperature(
                potential_temperature(parcel_t, parcel_p),
                parcel_q,
                0.0,
            );
            let theta_v_env = virtual_potential_temperature(
                potential_temperature(level.temp_k, level.pressure_pa),
                level.q,
                0.0,
            );
            let buoyancy = G * (theta_v_parcel - theta_v_env) / theta_v_env.max(1.0);
            if buoyancy > 0.0 {
                cape += buoyancy * dz_step;
            } else {
                cin += buoyancy * dz_step;
            }
        }
    }
    (cape.max(0.0), cin.min(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buck_saturation_matches_known_values() {
        let es0 = saturation_vapour_pressure(273.15);
        assert!((es0 - 611.2).abs() < 1.0, "{es0}");
        let es20 = saturation_vapour_pressure(293.15);
        assert!((es20 - 2338.0).abs() < 25.0, "{es20}");
        let es30 = saturation_vapour_pressure(303.15);
        assert!((es30 - 4243.0).abs() < 60.0, "{es30}");
    }

    #[test]
    fn dew_point_inverts_vapour_pressure() {
        for t in [273.15, 283.15, 293.15, 303.15] {
            let e = saturation_vapour_pressure(t);
            let td = dew_point_from_vapour_pressure(e);
            assert!((td - t).abs() < 0.05, "{t} -> {td}");
        }
    }

    #[test]
    fn potential_temperature_round_trips() {
        let t = 288.0;
        let p = 70_000.0;
        let theta = potential_temperature(t, p);
        assert!((temperature_from_theta(theta, p) - t).abs() < 1.0e-9);
    }

    #[test]
    fn lcl_is_near_espy_rule() {
        // 300 K parcel with 10 K dew-point depression: Espy ~1250 m.
        let temp = 300.0;
        let q = saturation_specific_humidity(290.0, P0);
        let z = lcl_height(temp, q, P0);
        let espy = 125.0 * (temp - 290.0);
        assert!((z - espy).abs() < 0.25 * espy, "z={z} espy={espy}");
    }

    #[test]
    fn unstable_column_has_positive_cape() {
        let surface = Level {
            height_m: 0.0,
            temp_k: 300.0,
            q: 0.016,
            pressure_pa: P0,
        };
        let levels = [
            Level {
                height_m: 0.0,
                temp_k: 300.0,
                q: 0.016,
                pressure_pa: P0,
            },
            Level {
                height_m: 4_000.0,
                temp_k: 270.0,
                q: 0.004,
                pressure_pa: 62_000.0,
            },
            Level {
                height_m: 8_000.0,
                temp_k: 255.0,
                q: 0.001,
                pressure_pa: 36_000.0,
            },
        ];
        let (cape, cin) = cape_cin(&levels, surface);
        assert!(cape > 100.0, "cape={cape} cin={cin}");
        assert!(cin <= 0.0);
    }
}
