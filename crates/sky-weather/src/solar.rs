//! Approximate solar position (Michalsky 1988 / NOAA solar calculator).
//!
//! The model only needs degree-level accuracy for diurnal cloud evolution;
//! the renderer's sun disk uses the same returned direction. Azimuth follows
//! the sky-tracer convention: 0 deg = +Z, 90 deg = +X.

use crate::SunPosition;

fn radians(deg: f64) -> f64 {
    deg.to_radians()
}

/// Solar position for a UTC hour and day of year.
pub fn position(
    latitude_deg: f64,
    longitude_deg: f64,
    day_of_year: f64,
    utc_hours: f64,
) -> SunPosition {
    let hour = utc_hours.rem_euclid(24.0);
    let gamma = 2.0 * std::f64::consts::PI / 365.0 * (day_of_year - 1.0 + (hour - 12.0) / 24.0);
    let eqtime = 229.18
        * (0.000_075 + 0.001_868 * gamma.cos()
            - 0.032_077 * gamma.sin()
            - 0.014_615 * (2.0 * gamma).cos()
            - 0.040_849 * (2.0 * gamma).sin());
    let decl = 0.006_918 - 0.399_912 * gamma.cos() + 0.070_257 * gamma.sin()
        - 0.006_758 * (2.0 * gamma).cos()
        + 0.000_907 * (2.0 * gamma).sin()
        - 0.002_697 * (3.0 * gamma).cos()
        + 0.001_48 * (3.0 * gamma).sin();
    let true_solar_minutes = hour * 60.0 + eqtime + 4.0 * longitude_deg;
    let hour_angle = radians(true_solar_minutes / 4.0 - 180.0);
    let lat = radians(latitude_deg.clamp(-89.9, 89.9));
    let cos_zenith =
        (lat.sin() * decl.sin() + lat.cos() * decl.cos() * hour_angle.cos()).clamp(-1.0, 1.0);
    let elevation = 90.0 - cos_zenith.acos().to_degrees();
    let east = -hour_angle.sin() * decl.cos();
    let north = lat.cos() * decl.sin() - lat.sin() * decl.cos() * hour_angle.cos();
    let azimuth = east.atan2(north).to_degrees().rem_euclid(360.0);
    SunPosition {
        elevation_deg: elevation.clamp(-90.0, 90.0),
        azimuth_deg: azimuth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equator_equinox_noon_is_overhead() {
        let sun = position(0.0, 0.0, 80.0, 12.0);
        assert!(sun.elevation_deg > 88.0, "{sun:?}");
    }

    #[test]
    fn midlatitude_summer_noon_is_high_but_not_overhead() {
        let sun = position(45.0, 0.0, 172.0, 12.0);
        assert!(
            (68.0..70.0).contains(&sun.elevation_deg),
            "elevation={} azimuth={}",
            sun.elevation_deg,
            sun.azimuth_deg
        );
        assert!(sun.azimuth_deg.is_finite() && (0.0..360.0).contains(&sun.azimuth_deg));
    }

    #[test]
    fn sun_is_below_horizon_at_midnight() {
        let sun = position(45.0, 0.0, 172.0, 0.0);
        assert!(sun.elevation_deg < 0.0, "{sun:?}");
    }

    #[test]
    fn azimuth_convention_points_south_at_northern_noon() {
        let sun = position(45.0, 0.0, 172.0, 12.0);
        // At northern-hemisphere solar noon the sun is south; the sky
        // convention maps south to 180 deg (toward -Z).
        assert!((sun.azimuth_deg - 180.0).abs() < 5.0, "{sun:?}");
    }
}
