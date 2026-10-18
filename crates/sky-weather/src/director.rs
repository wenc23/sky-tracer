//! Weather director: a small state machine plus moving weather systems.
//!
//! The director does not solve atmospheric equations. It produces plausible
//! target fields and switches between them with dwell times, so the demo gets
//! weather behaviour (fronts, cells, fog banks, snow bands) rather than a
//! random preset every frame.

use serde::{Deserialize, Serialize};

use crate::{
    kind::{Climate, Target, WeatherKeyframe, WeatherKind, WeatherMode},
    rng::Rng,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SystemKind {
    Front,
    ConvectiveCell,
    RainBand,
    SnowBand,
    FogBank,
    DustPlume,
}

impl SystemKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Front => "front",
            Self::ConvectiveCell => "convective cell",
            Self::RainBand => "rain band",
            Self::SnowBand => "snow band",
            Self::FogBank => "fog bank",
            Self::DustPlume => "dust plume",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WeatherSystem {
    pub kind: SystemKind,
    pub weather: WeatherKind,
    pub center_x_m: f32,
    pub center_z_m: f32,
    pub radius_m: f32,
    pub velocity_x_m_s: f32,
    pub velocity_z_m_s: f32,
    pub age_s: f32,
    pub life_s: f32,
    pub intensity: f32,
}

impl WeatherSystem {
    fn update(&mut self, dt_s: f32, domain_m: f32) {
        self.age_s += dt_s;
        self.center_x_m += self.velocity_x_m_s * dt_s;
        self.center_z_m += self.velocity_z_m_s * dt_s;
        let half = domain_m * 0.5;
        if self.center_x_m > half {
            self.center_x_m -= domain_m;
        } else if self.center_x_m < -half {
            self.center_x_m += domain_m;
        }
        if self.center_z_m > half {
            self.center_z_m -= domain_m;
        } else if self.center_z_m < -half {
            self.center_z_m += domain_m;
        }
    }

    fn alive(&self) -> bool {
        self.age_s < self.life_s
    }

    fn influence(&self, x_m: f32, z_m: f32) -> f32 {
        let dx = x_m - self.center_x_m;
        let dz = z_m - self.center_z_m;
        let distance = (dx * dx + dz * dz).sqrt();
        let radius = self.radius_m.max(250.0);
        if distance >= radius * 2.5 {
            return 0.0;
        }
        let core = (-(distance * distance) / (2.0 * radius * radius)).exp();
        let fade = if self.age_s < 30.0 {
            self.age_s / 30.0
        } else if self.age_s > self.life_s - 60.0 {
            ((self.life_s - self.age_s) / 60.0).clamp(0.0, 1.0)
        } else {
            1.0
        };
        (self.intensity * core * fade).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WeatherDirector {
    pub mode: WeatherMode,
    pub kind: WeatherKind,
    pub target_kind: WeatherKind,
    pub intensity: f32,
    pub systems: Vec<WeatherSystem>,
    blend: f32,
    dwell_s: f32,
    duration_s: f32,
    transition_s: f32,
    tour_index: usize,
    timeline_index: usize,
    spawn_timer_s: f32,
    rng: Rng,
}

impl WeatherDirector {
    pub fn new(mode: WeatherMode, kind: WeatherKind, intensity: f32, seed: u64) -> Self {
        let target_kind = kind;
        Self {
            mode,
            kind,
            target_kind,
            intensity: intensity.clamp(0.25, 2.5),
            systems: Vec::new(),
            blend: 1.0,
            dwell_s: 0.0,
            duration_s: 240.0,
            transition_s: 45.0,
            tour_index: 0,
            timeline_index: 0,
            spawn_timer_s: 35.0,
            rng: Rng::new(seed),
        }
    }

    pub fn target(&self) -> Target {
        let mut target = self
            .kind
            .target()
            .blended(self.target_kind.target(), self.blend);
        target.precipitation_mm_h *= self.intensity;
        target.lightning_rate_per_min *= self.intensity;
        target.wind_m_s *= (0.75 + 0.35 * self.intensity).clamp(0.75, 1.6);
        target.cloud_water =
            (target.cloud_water * (0.75 + 0.35 * self.intensity)).clamp(0.0, 8.0e-3);
        target.cloud_cover = (target.cloud_cover * (0.8 + 0.25 * self.intensity)).clamp(0.0, 1.0);
        target
    }

    pub fn set_mode(&mut self, mode: WeatherMode) -> Result<(), String> {
        if let WeatherMode::Timeline(ref frames) = mode {
            if frames
                .windows(2)
                .any(|pair| !pair[0].time_s.is_finite() || pair[1].time_s < pair[0].time_s)
            {
                return Err("weather timeline must be finite and sorted".into());
            }
        }
        self.mode = mode;
        self.blend = 1.0;
        self.dwell_s = 0.0;
        self.timeline_index = 0;
        Ok(())
    }

    pub fn set_weather(&mut self, kind: WeatherKind) {
        self.kind = kind;
        self.target_kind = kind;
        self.blend = 1.0;
        self.dwell_s = 0.0;
    }

    pub fn set_intensity(&mut self, intensity: f32) {
        self.intensity = intensity.clamp(0.25, 2.5);
    }

    pub fn step(&mut self, dt_s: f32, time_s: f64, climate: Climate, season: f32, domain_m: f32) {
        if dt_s <= 0.0 || !dt_s.is_finite() {
            return;
        }
        self.update_systems(dt_s, domain_m);
        let mode = self.mode.clone();
        match mode {
            WeatherMode::Fixed(kind) => {
                self.kind = kind;
                self.target_kind = kind;
                self.blend = 1.0;
            }
            WeatherMode::Tour => self.step_tour(dt_s),
            WeatherMode::Timeline(frames) => self.step_timeline(dt_s, time_s, &frames),
            WeatherMode::Auto => self.step_auto(dt_s, climate, season),
        }

        if self.blend < 1.0 {
            self.blend = (self.blend + dt_s / self.transition_s.max(1.0)).min(1.0);
            if self.blend >= 1.0 {
                self.kind = self.target_kind;
            }
        }
    }

    fn step_tour(&mut self, dt_s: f32) {
        self.dwell_s += dt_s;
        if self.dwell_s >= 18.0 {
            self.dwell_s = 0.0;
            self.tour_index = (self.tour_index + 1) % WeatherKind::ALL.len();
            self.target_kind = WeatherKind::ALL[self.tour_index];
            self.blend = 0.0;
            self.transition_s = 5.0;
        } else if self.blend >= 1.0 {
            self.kind = self.target_kind;
        }
    }

    fn step_timeline(&mut self, dt_s: f32, time_s: f64, frames: &[WeatherKeyframe]) {
        if frames.is_empty() {
            return;
        }
        while self.timeline_index + 1 < frames.len()
            && time_s >= frames[self.timeline_index + 1].time_s
        {
            self.timeline_index += 1;
            self.target_kind = frames[self.timeline_index].kind;
            self.blend = 0.0;
            self.transition_s = 12.0;
        }
        if self.timeline_index == 0 && self.blend >= 1.0 {
            self.kind = frames[0].kind;
            self.target_kind = frames[0].kind;
        }
        if self.blend < 1.0 {
            self.dwell_s += dt_s;
        }
    }

    fn step_auto(&mut self, dt_s: f32, climate: Climate, season: f32) {
        if self.blend < 1.0 {
            return;
        }
        self.dwell_s += dt_s;
        if self.dwell_s < self.duration_s {
            return;
        }
        self.dwell_s = 0.0;
        let next = self.next_kind(climate, season);
        if next != self.kind {
            self.target_kind = next;
            self.blend = 0.0;
            self.transition_s = self.range(25.0, 75.0);
            self.duration_s = self.range(150.0, 420.0);
            self.spawn_timer_s = self.spawn_timer_s.min(20.0);
        }
    }

    fn next_kind(&mut self, climate: Climate, season: f32) -> WeatherKind {
        let winter = (1.0 - season.clamp(0.0, 1.0)) * 0.7;
        let mut pool = Vec::with_capacity(WeatherKind::ALL.len());
        for kind in WeatherKind::ALL {
            let mut weight = 1.0f32;
            if matches!(
                kind,
                WeatherKind::LightSnow
                    | WeatherKind::HeavySnow
                    | WeatherKind::Blizzard
                    | WeatherKind::Sleet
                    | WeatherKind::FreezingRain
            ) {
                weight *= if climate.supports_snow() {
                    0.2 + winter
                } else {
                    0.0
                };
            }
            if matches!(kind, WeatherKind::Dust | WeatherKind::Haze) {
                weight *= if climate.is_dry() { 3.0 } else { 0.25 };
            }
            if matches!(kind, WeatherKind::Thunderstorm | WeatherKind::Hailstorm) {
                weight *= if climate.is_tropical() { 2.0 } else { 0.7 };
            }
            if matches!(kind, WeatherKind::Supercell) {
                weight *= 0.12;
            }
            if matches!(kind, WeatherKind::Fog) {
                weight *= 1.1;
            }
            if kind == self.kind || kind == self.target_kind {
                weight *= 0.25;
            }
            let slots = (weight * 20.0).round().max(0.0) as u32;
            for _ in 0..slots {
                pool.push(kind);
            }
        }
        if pool.is_empty() {
            return WeatherKind::PartlyCloudy;
        }
        pool[self.rng.range_u32(0, pool.len() as u32) as usize]
    }

    fn update_systems(&mut self, dt_s: f32, domain_m: f32) {
        for system in &mut self.systems {
            system.update(dt_s, domain_m);
        }
        self.systems.retain(WeatherSystem::alive);
        self.spawn_timer_s -= dt_s;
        if self.spawn_timer_s <= 0.0 {
            self.spawn_timer_s = self.range(70.0, 220.0);
            if matches!(self.mode, WeatherMode::Auto | WeatherMode::Fixed(_)) {
                self.spawn_system(domain_m);
            }
        }
    }

    fn spawn_system(&mut self, domain_m: f32) {
        let kind = if self.target_kind.is_precipitating() {
            self.target_kind
        } else {
            self.kind
        };
        let system_kind = system_kind_for(kind);
        let Some(system_kind) = system_kind else {
            return;
        };
        if self.systems.iter().any(|s| s.kind == system_kind) {
            return;
        }
        let half = domain_m * 0.5;
        let center_x = self.range(-half * 0.75, half * 0.75);
        let center_z = self.range(-half * 0.75, half * 0.75);
        let (vx, vz) = match system_kind {
            SystemKind::Front => (self.range(3.0, 8.0), self.range(-1.0, 1.0)),
            SystemKind::FogBank => (self.range(0.2, 1.2), self.range(-0.5, 0.5)),
            SystemKind::DustPlume => (self.range(4.0, 9.0), self.range(-2.0, 2.0)),
            _ => (self.range(2.0, 7.0), self.range(-3.0, 3.0)),
        };
        let radius_m = self.range(2_500.0, 8_000.0);
        let life_s = self.range(180.0, 720.0);
        let system_intensity = self.range(0.45, 1.0) * self.intensity.clamp(0.5, 1.6);
        self.systems.push(WeatherSystem {
            kind: system_kind,
            weather: kind,
            center_x_m: center_x,
            center_z_m: center_z,
            radius_m,
            velocity_x_m_s: vx,
            velocity_z_m_s: vz,
            age_s: 0.0,
            life_s,
            intensity: system_intensity,
        });
    }

    pub fn system_influence(&self, x_m: f32, z_m: f32) -> f32 {
        self.systems
            .iter()
            .map(|system| system.influence(x_m, z_m))
            .fold(0.0, f32::max)
            .clamp(0.0, 1.0)
    }

    fn range(&mut self, low: f32, high: f32) -> f32 {
        self.rng.range_f32(low, high)
    }
}

fn system_kind_for(kind: WeatherKind) -> Option<SystemKind> {
    match kind {
        WeatherKind::Clear
        | WeatherKind::Fair
        | WeatherKind::PartlyCloudy
        | WeatherKind::Overcast
        | WeatherKind::Haze
        | WeatherKind::Gale => None,
        WeatherKind::Fog => Some(SystemKind::FogBank),
        WeatherKind::Dust => Some(SystemKind::DustPlume),
        WeatherKind::LightSnow | WeatherKind::HeavySnow | WeatherKind::Blizzard => {
            Some(SystemKind::SnowBand)
        }
        WeatherKind::Sleet | WeatherKind::FreezingRain => Some(SystemKind::Front),
        WeatherKind::Drizzle | WeatherKind::Rain | WeatherKind::HeavyRain => {
            Some(SystemKind::RainBand)
        }
        WeatherKind::TropicalShower
        | WeatherKind::Thunderstorm
        | WeatherKind::Hailstorm
        | WeatherKind::Supercell => Some(SystemKind::ConvectiveCell),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn director(mode: WeatherMode) -> WeatherDirector {
        WeatherDirector::new(mode, WeatherKind::Clear, 1.0, 7)
    }

    #[test]
    fn fixed_mode_keeps_target() {
        let mut d = director(WeatherMode::Fixed(WeatherKind::Thunderstorm));
        for _ in 0..100 {
            d.step(1.0, 0.0, Climate::Temperate, 0.5, 32_000.0);
        }
        assert_eq!(d.kind, WeatherKind::Thunderstorm);
        assert!(d.target().precipitation_mm_h > 0.0);
    }

    #[test]
    fn tour_reaches_every_catalogue_entry() {
        let mut d = director(WeatherMode::Tour);
        let mut seen = std::collections::BTreeSet::new();
        for step in 0..400 {
            d.step(1.0, f64::from(step), Climate::Temperate, 0.5, 32_000.0);
            seen.insert(d.target_kind);
        }
        assert_eq!(seen.len(), WeatherKind::ALL.len());
    }

    #[test]
    fn auto_stays_finite_and_changes() {
        let mut d = director(WeatherMode::Auto);
        let start = d.kind;
        let mut changed = false;
        for step in 0..4000 {
            d.step(1.0, f64::from(step), Climate::Temperate, 0.5, 32_000.0);
            let t = d.target();
            assert!(t.cloud_cover.is_finite());
            assert!(t.wind_m_s.is_finite());
            if d.kind != start {
                changed = true;
            }
        }
        assert!(changed);
    }

    #[test]
    fn fixed_system_influence_is_bounded() {
        let mut d = director(WeatherMode::Fixed(WeatherKind::Rain));
        for _ in 0..100 {
            d.step(1.0, 0.0, Climate::Temperate, 0.5, 32_000.0);
        }
        assert!((0.0..=1.0).contains(&d.system_influence(0.0, 0.0)));
    }
}
