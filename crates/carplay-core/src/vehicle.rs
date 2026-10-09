// SPDX-License-Identifier: GPL-3.0-only
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Gear {
    Park,
    Reverse,
    Neutral,
    Drive,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VehicleStatus {
    pub gear: Option<Gear>,
    pub battery_percent: Option<f32>,
    pub speed_kmh: Option<f32>,
    /// Monotonic receiver timestamp; caller must never substitute a simulated gear for a real one.
    pub observed_at_ms: Option<u64>,
}

impl VehicleStatus {
    pub fn parked_video_allowed(&self, now_ms: u64) -> bool {
        self.gear == Some(Gear::Park)
            && self
                .observed_at_ms
                .is_some_and(|at| now_ms.checked_sub(at).is_some_and(|age| age <= 2000))
    }
}

pub trait VehicleProvider: Send {
    fn read_status(&mut self) -> Result<VehicleStatus, String>;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NavigationUpdate {
    pub street: Option<String>,
    pub distance_metres: Option<u32>,
    pub maneuver: Option<u16>,
    pub remaining_seconds: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_stale_future_or_nonpark_status_closes_video() {
        assert!(!VehicleStatus::default().parked_video_allowed(0));
        let mut s = VehicleStatus {
            gear: Some(Gear::Park),
            observed_at_ms: Some(100),
            ..Default::default()
        };
        assert!(s.parked_video_allowed(100));
        assert!(!s.parked_video_allowed(99));
        assert!(!s.parked_video_allowed(2101));
        s.gear = Some(Gear::Drive);
        assert!(!s.parked_video_allowed(100));
    }
}
