// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay 9e244d9 shared/src/main/java/com/shilapi/xcertplay/hud/
// BydClusterSong.kt (ClusterSongState), BydHudRouteState.kt, BydCarPlayCall.kt (CarPlayCallState).
//! Portable decoding of the metadata subset used by DiPlay's displays.
//! These are incremental updates: None means omitted; Some("")/Some([]) means explicitly cleared.
//! No Android/BYD vendor service, vehicle-write plugin, routing cache or display policy is implemented here.
//! Unknown TLVs (including repeated IDs) remain ordered and byte-exact at their original group level.
use crate::{
    Error, Result,
    tlv::{self, ControlMessage, Parameter},
};

pub const NOW_PLAYING: u16 = 0x5001;
pub const ROUTE_GUIDANCE: u16 = 0x5201;
pub const ROUTE_MANEUVER: u16 = 0x5202;
pub const CALL_STATE: u16 = 0x4155;

/// Resource limits apply to the full message and to all decoded nested groups together.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_body_bytes: usize,
    pub max_parameters: usize,
    pub max_text_bytes: usize,
    pub max_maneuver_indices: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: tlv::MAX_FRAME - 6,
            max_parameters: 1024,
            max_text_bytes: 4096,
            max_maneuver_indices: 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    NowPlaying(NowPlaying),
    Route(RouteGuidance),
    Maneuver(RouteManeuver),
    Call(CallState),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NowPlaying {
    pub item: Option<MediaItem>,
    pub playback: Option<Playback>,
    pub unknown: Vec<Parameter>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaItem {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_ms: Option<u32>,
    pub unknown: Vec<Parameter>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Playback {
    pub status: Option<PlaybackStatus>,
    pub elapsed_ms: Option<u32>,
    pub unknown: Vec<Parameter>,
}
/// Raw values are retained for forward compatibility. DiPlay treats 1, 3, and 4 as playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackStatus(pub u8);
impl PlaybackStatus {
    pub fn is_playing(self) -> bool {
        matches!(self.0, 1 | 3 | 4)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteGuidance {
    pub state: Option<RouteState>,
    pub current_road: Option<String>,
    pub arrival_epoch_seconds: Option<u64>,
    pub remaining_seconds: Option<u64>,
    pub remaining_meters: Option<u32>,
    pub distance_to_maneuver_meters: Option<u32>,
    pub current_maneuver_indices: Option<Vec<u16>>,
    pub unknown: Vec<Parameter>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteState {
    NoRoute,
    Active,
    Arrived,
    Unknown(u8),
}
impl From<u8> for RouteState {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::NoRoute,
            1 => Self::Active,
            2 => Self::Arrived,
            value => Self::Unknown(value),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteManeuver {
    pub index: Option<u16>,
    pub maneuver_type: Option<u8>,
    pub after_road: Option<String>,
    pub driving_side: Option<u8>,
    pub unknown: Vec<Parameter>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallState {
    pub remote_identifier: Option<String>,
    pub display_name: Option<String>,
    pub status: Option<CallStatus>,
    pub call_uuid: Option<String>,
    pub unknown: Vec<Parameter>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallStatus {
    Disconnected,
    Sending,
    Ringing,
    Connecting,
    Active,
    Held,
    Disconnecting,
    Unknown(u8),
}
impl From<u8> for CallStatus {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Disconnected,
            1 => Self::Sending,
            2 => Self::Ringing,
            3 => Self::Connecting,
            4 => Self::Active,
            5 => Self::Held,
            6 => Self::Disconnecting,
            value => Self::Unknown(value),
        }
    }
}

/// Unsupported messages return None without being interpreted as metadata.
pub fn decode(message: &ControlMessage) -> Result<Option<Update>> {
    decode_with_limits(message, Limits::default())
}
pub fn decode_with_limits(message: &ControlMessage, limits: Limits) -> Result<Option<Update>> {
    if ![NOW_PLAYING, ROUTE_GUIDANCE, ROUTE_MANEUVER, CALL_STATE].contains(&message.message_id) {
        return Ok(None);
    }
    if message.body.len() > limits.max_body_bytes || message.body.len() > tlv::MAX_FRAME - 6 {
        return Err(Error::Limit);
    }
    let mut reader = Reader {
        limits,
        remaining: limits.max_parameters,
    };
    let params = reader.parameters(&message.body)?;
    Ok(Some(match message.message_id {
        NOW_PLAYING => Update::NowPlaying(reader.now_playing(params)?),
        ROUTE_GUIDANCE => Update::Route(reader.route(params)?),
        ROUTE_MANEUVER => Update::Maneuver(reader.maneuver(params)?),
        CALL_STATE => Update::Call(reader.call(params)?),
        _ => unreachable!("validated message ID"),
    }))
}
struct Reader {
    limits: Limits,
    remaining: usize,
}
fn assign<T>(slot: &mut Option<T>, value: T) -> Result<()> {
    if slot.is_some() {
        return Err(Error::Invalid("duplicate metadata field"));
    }
    *slot = Some(value);
    Ok(())
}
impl Reader {
    fn parameters(&mut self, mut bytes: &[u8]) -> Result<Vec<Parameter>> {
        let mut result = Vec::new();
        while !bytes.is_empty() {
            if bytes.len() < 4 {
                return Err(Error::Invalid("truncated metadata TLV"));
            }
            let length = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
            if length < 4 || length > bytes.len() {
                return Err(Error::Invalid("metadata TLV length"));
            }
            if self.remaining == 0 {
                return Err(Error::Limit);
            }
            self.remaining -= 1;
            result.push(Parameter::new(
                u16::from_be_bytes([bytes[2], bytes[3]]),
                bytes[4..length].to_vec(),
            ));
            bytes = &bytes[length..];
        }
        Ok(result)
    }
    fn text(&self, parameter: &Parameter) -> Result<String> {
        let text = parameter.as_str()?;
        if text.len() > self.limits.max_text_bytes {
            return Err(Error::Limit);
        }
        Ok(text.to_owned())
    }
    fn u64(&self, parameter: &Parameter) -> Result<u64> {
        let bytes: [u8; 8] = parameter
            .value
            .as_slice()
            .try_into()
            .map_err(|_| Error::Invalid("u64 metadata length"))?;
        Ok(u64::from_be_bytes(bytes))
    }
    fn now_playing(&mut self, params: Vec<Parameter>) -> Result<NowPlaying> {
        let mut update = NowPlaying::default();
        for p in params {
            match p.id {
                0 => {
                    let params = self.parameters(&p.value)?;
                    assign(&mut update.item, self.item(params)?)?;
                }
                1 => {
                    let params = self.parameters(&p.value)?;
                    assign(&mut update.playback, self.playback(params)?)?;
                }
                _ => update.unknown.push(p),
            }
        }
        Ok(update)
    }
    fn item(&self, params: Vec<Parameter>) -> Result<MediaItem> {
        let mut update = MediaItem::default();
        for p in params {
            match p.id {
                1 => assign(&mut update.title, self.text(&p)?)?,
                4 => assign(&mut update.duration_ms, p.as_u32()?)?,
                12 => assign(&mut update.artist, self.text(&p)?)?,
                _ => update.unknown.push(p),
            }
        }
        Ok(update)
    }
    fn playback(&self, params: Vec<Parameter>) -> Result<Playback> {
        let mut update = Playback::default();
        for p in params {
            match p.id {
                0 => assign(&mut update.status, PlaybackStatus(p.as_u8()?))?,
                1 => assign(&mut update.elapsed_ms, p.as_u32()?)?,
                _ => update.unknown.push(p),
            }
        }
        Ok(update)
    }
    fn route(&self, params: Vec<Parameter>) -> Result<RouteGuidance> {
        let mut update = RouteGuidance::default();
        for p in params {
            match p.id {
                1 => assign(&mut update.state, p.as_u8()?.into())?,
                3 => assign(&mut update.current_road, self.text(&p)?)?,
                5 => assign(&mut update.arrival_epoch_seconds, self.u64(&p)?)?,
                6 => assign(&mut update.remaining_seconds, self.u64(&p)?)?,
                7 => assign(&mut update.remaining_meters, p.as_u32()?)?,
                10 => assign(&mut update.distance_to_maneuver_meters, p.as_u32()?)?,
                13 => {
                    if p.value.len() % 2 != 0 {
                        return Err(Error::Invalid("odd maneuver index list"));
                    }
                    if p.value.len() / 2 > self.limits.max_maneuver_indices {
                        return Err(Error::Limit);
                    }
                    let indices = p
                        .value
                        .chunks_exact(2)
                        .map(|p| u16::from_be_bytes([p[0], p[1]]))
                        .collect();
                    assign(&mut update.current_maneuver_indices, indices)?;
                }
                _ => update.unknown.push(p),
            }
        }
        Ok(update)
    }
    fn maneuver(&self, params: Vec<Parameter>) -> Result<RouteManeuver> {
        let mut update = RouteManeuver::default();
        for p in params {
            match p.id {
                1 => assign(&mut update.index, p.as_u16()?)?,
                3 => assign(&mut update.maneuver_type, p.as_u8()?)?,
                4 => assign(&mut update.after_road, self.text(&p)?)?,
                8 => assign(&mut update.driving_side, p.as_u8()?)?,
                _ => update.unknown.push(p),
            }
        }
        Ok(update)
    }
    fn call(&self, params: Vec<Parameter>) -> Result<CallState> {
        let mut update = CallState::default();
        for p in params {
            match p.id {
                0 => assign(&mut update.remote_identifier, self.text(&p)?)?,
                1 => assign(&mut update.display_name, self.text(&p)?)?,
                2 => assign(&mut update.status, p.as_u8()?.into())?,
                4 => assign(&mut update.call_uuid, self.text(&p)?)?,
                _ => update.unknown.push(p),
            }
        }
        Ok(update)
    }
}
