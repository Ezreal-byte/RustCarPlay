// SPDX-License-Identifier: GPL-3.0-only
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ReceiverConfig {
    pub name: String,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
    pub hevc: bool,
    pub audio_output: bool,
    pub microphone: bool,
    pub right_hand_drive: bool,
    pub second_screen: bool,
    pub reconnect: bool,
    pub audio_buffer_ms: u16,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            name: "RustCarPlay".into(),
            width: 1280,
            height: 720,
            fps: 30,
            hevc: false,
            audio_output: true,
            microphone: false,
            right_hand_drive: false,
            second_screen: false,
            reconnect: true,
            audio_buffer_ms: 80,
        }
    }
}

impl ReceiverConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.name.trim().is_empty()
            || self.name.len() > 63
            || self.name.chars().any(char::is_control)
        {
            return Err("name must contain 1..63 UTF-8 bytes without control characters");
        }
        if !(320..=3840).contains(&self.width) || !(240..=2160).contains(&self.height) {
            return Err("display dimensions must be within 320x240..3840x2160");
        }
        if ![24, 25, 30, 50, 60].contains(&self.fps) {
            return Err("unsupported frame rate");
        }
        if !(20..=500).contains(&self.audio_buffer_ms) {
            return Err("audio buffer must be 20..500 ms");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unavailable,
    Unverified,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capability {
    pub id: String,
    pub availability: Availability,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_saved_settings_are_not_accepted() {
        let mut config = ReceiverConfig::default();
        assert!(config.validate().is_ok());
        config.width = 0;
        assert!(config.validate().is_err());
        assert!(serde_json::from_str::<ReceiverConfig>(r#"{"unexpected":true}"#).is_err());
    }
}
