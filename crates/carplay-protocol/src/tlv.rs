// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay 9e244d9:
// shared/src/main/java/com/shilapi/xcertplay/iap2/wire/Iap2CsmFramer.kt,
// iap2/message/Iap2Messages.kt, transport/Iap2IdentificationClient.kt.
//! CSM framing and ordered TLV parameters. Unknown and repeated IDs are preserved.
use crate::{Error, Result};
use std::collections::VecDeque;

pub const MAX_FRAME: usize = 65_535;
pub const START_IDENTIFICATION: u16 = 0x1d00;
pub const IDENTIFICATION_INFORMATION: u16 = 0x1d01;
pub const IDENTIFICATION_ACCEPTED: u16 = 0x1d02;
pub const IDENTIFICATION_REJECTED: u16 = 0x1d03;
pub const REQUEST_CERTIFICATE: u16 = 0xaa00;
pub const CERTIFICATE: u16 = 0xaa01;
pub const REQUEST_CHALLENGE_RESPONSE: u16 = 0xaa02;
pub const CHALLENGE_RESPONSE: u16 = 0xaa03;
pub const AUTHENTICATION_FAILED: u16 = 0xaa04;
pub const AUTHENTICATION_SUCCEEDED: u16 = 0xaa05;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub id: u16,
    pub value: Vec<u8>,
}
impl Parameter {
    pub fn new(id: u16, value: impl Into<Vec<u8>>) -> Self {
        Self {
            id,
            value: value.into(),
        }
    }
    pub fn empty(id: u16) -> Self {
        Self::new(id, Vec::new())
    }
    pub fn u8(id: u16, value: u8) -> Self {
        Self::new(id, vec![value])
    }
    pub fn u16(id: u16, value: u16) -> Self {
        Self::new(id, value.to_be_bytes().to_vec())
    }
    pub fn u32(id: u16, value: u32) -> Self {
        Self::new(id, value.to_be_bytes().to_vec())
    }
    pub fn string(id: u16, value: &str) -> Result<Self> {
        if value.contains('\0') {
            return Err(Error::Invalid("embedded NUL in string"));
        }
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        if bytes.len() > MAX_FRAME - 4 {
            return Err(Error::Limit);
        }
        Ok(Self::new(id, bytes))
    }
    pub fn group(id: u16, parameters: &[Self]) -> Result<Self> {
        Ok(Self::new(id, encode_parameters(parameters)?))
    }
    pub fn as_u8(&self) -> Result<u8> {
        match self.value.as_slice() {
            [b] => Ok(*b),
            _ => Err(Error::Invalid("u8 parameter length")),
        }
    }
    pub fn as_u16(&self) -> Result<u16> {
        match self.value.as_slice() {
            [a, b] => Ok(u16::from_be_bytes([*a, *b])),
            _ => Err(Error::Invalid("u16 parameter length")),
        }
    }
    pub fn as_u32(&self) -> Result<u32> {
        match self.value.as_slice() {
            [a, b, c, d] => Ok(u32::from_be_bytes([*a, *b, *c, *d])),
            _ => Err(Error::Invalid("u32 parameter length")),
        }
    }
    pub fn as_str(&self) -> Result<&str> {
        let Some((&0, bytes)) = self.value.split_last() else {
            return Err(Error::Invalid("unterminated string"));
        };
        if bytes.contains(&0) {
            return Err(Error::Invalid("embedded NUL in string"));
        }
        std::str::from_utf8(bytes).map_err(|_| Error::Invalid("invalid UTF-8 string"))
    }
    pub fn parameters(&self) -> Result<Vec<Self>> {
        parse_parameters(&self.value)
    }
}
pub fn encode_parameters(parameters: &[Parameter]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for parameter in parameters {
        let length = parameter.value.len().checked_add(4).ok_or(Error::Limit)?;
        if length > MAX_FRAME || out.len() + length > MAX_FRAME - 6 {
            return Err(Error::Limit);
        }
        out.extend((length as u16).to_be_bytes());
        out.extend(parameter.id.to_be_bytes());
        out.extend(&parameter.value);
    }
    Ok(out)
}
pub fn parse_parameters(mut bytes: &[u8]) -> Result<Vec<Parameter>> {
    if bytes.len() > MAX_FRAME - 6 {
        return Err(Error::Limit);
    }
    let mut out = Vec::new();
    while !bytes.is_empty() {
        if bytes.len() < 4 {
            return Err(Error::Invalid("truncated parameter header"));
        }
        let length = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
        if length < 4 || length > bytes.len() {
            return Err(Error::Invalid("parameter length"));
        }
        out.push(Parameter::new(
            u16::from_be_bytes([bytes[2], bytes[3]]),
            bytes[4..length].to_vec(),
        ));
        bytes = &bytes[length..];
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlMessage {
    pub message_id: u16,
    pub body: Vec<u8>,
}
impl ControlMessage {
    pub fn new(message_id: u16, parameters: &[Parameter]) -> Result<Self> {
        Ok(Self {
            message_id,
            body: encode_parameters(parameters)?,
        })
    }
    pub fn empty(message_id: u16) -> Self {
        Self {
            message_id,
            body: Vec::new(),
        }
    }
    pub fn parameters(&self) -> Result<Vec<Parameter>> {
        parse_parameters(&self.body)
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.body.len() > MAX_FRAME - 6 {
            return Err(Error::Limit);
        }
        let mut out = vec![0x40, 0x40];
        out.extend(((self.body.len() + 6) as u16).to_be_bytes());
        out.extend(self.message_id.to_be_bytes());
        out.extend(&self.body);
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 6 {
            return Err(Error::Incomplete);
        }
        if bytes[..2] != [0x40, 0x40]
            || u16::from_be_bytes([bytes[2], bytes[3]]) as usize != bytes.len()
        {
            return Err(Error::Invalid("CSM header"));
        }
        Ok(Self {
            message_id: u16::from_be_bytes([bytes[4], bytes[5]]),
            body: bytes[6..].to_vec(),
        })
    }
    pub fn link_chunks(&self, chunk_size: usize) -> Result<Vec<Vec<u8>>> {
        if !(1..=crate::iap2::MAX_PAYLOAD).contains(&chunk_size) {
            return Err(Error::Invalid("link chunk size"));
        }
        Ok(self
            .encode()?
            .chunks(chunk_size)
            .map(<[u8]>::to_vec)
            .collect())
    }
}

#[derive(Debug, Default)]
pub struct ControlDecoder {
    bytes: VecDeque<u8>,
}
impl ControlDecoder {
    pub fn buffered_bytes(&self) -> usize {
        self.bytes.len()
    }
    /// Accept arbitrary fragmentation and concatenation, retaining at most 65535 bytes.
    pub fn offer(&mut self, mut bytes: &[u8]) -> Vec<ControlMessage> {
        let mut messages = Vec::new();
        while !bytes.is_empty() {
            let count = (MAX_FRAME - self.bytes.len()).min(bytes.len());
            self.bytes.extend(&bytes[..count]);
            bytes = &bytes[count..];
            self.drain(&mut messages);
        }
        messages
    }
    fn drain(&mut self, out: &mut Vec<ControlMessage>) {
        loop {
            while self.bytes.len() >= 2 && (self.bytes[0] != 0x40 || self.bytes[1] != 0x40) {
                self.bytes.pop_front();
            }
            if self.bytes.len() < 6 {
                return;
            }
            let length = u16::from_be_bytes([self.bytes[2], self.bytes[3]]) as usize;
            if length < 6 {
                self.bytes.pop_front();
                continue;
            }
            if self.bytes.len() < length {
                return;
            }
            let bytes: Vec<_> = self.bytes.drain(..length).collect();
            // Magic and complete length have already been validated above.
            out.push(ControlMessage {
                message_id: u16::from_be_bytes([bytes[4], bytes[5]]),
                body: bytes[6..].to_vec(),
            });
        }
    }
}

pub fn accessory_certificate(certificate: &[u8]) -> Result<ControlMessage> {
    ControlMessage::new(CERTIFICATE, &[Parameter::new(0, certificate.to_vec())])
}
pub fn authentication_response(signature: &[u8]) -> Result<ControlMessage> {
    ControlMessage::new(CHALLENGE_RESPONSE, &[Parameter::new(0, signature.to_vec())])
}
pub fn authentication_challenge(message: &ControlMessage) -> Result<Vec<u8>> {
    if message.message_id != REQUEST_CHALLENGE_RESPONSE {
        return Err(Error::Invalid("expected authentication challenge"));
    }
    let params = message.parameters()?;
    let mut challenges = params.into_iter().filter(|p| p.id == 0);
    let challenge = challenges
        .next()
        .ok_or(Error::Invalid("missing challenge"))?
        .value;
    if challenges.next().is_some() || challenge.is_empty() || challenge.len() > 128 {
        return Err(Error::Invalid("challenge length or duplicate"));
    }
    Ok(challenge)
}

pub fn wifi_configuration(
    ssid: &str,
    passphrase: &str,
    channel: u8,
    security_type: u8,
    bssid: Option<[u8; 6]>,
) -> Result<ControlMessage> {
    if ssid.trim().is_empty() || (security_type != 0 && passphrase.is_empty()) {
        return Err(Error::Invalid("Wi-Fi credentials"));
    }
    let mut params = Vec::new();
    if let Some(bssid) = bssid {
        params.push(Parameter::new(0, bssid.to_vec()));
    }
    params.extend([
        Parameter::string(1, ssid)?,
        Parameter::string(2, passphrase)?,
        Parameter::u8(3, security_type),
        Parameter::u8(4, channel),
    ]);
    ControlMessage::new(0x5703, &params)
}

#[derive(Clone)]
pub struct WirelessSession {
    pub ssid: String,
    pub passphrase: String,
    pub channel: u8,
    pub ip_addresses: Vec<String>,
    pub security_type: u8,
}
impl std::fmt::Debug for WirelessSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WirelessSession")
            .field("network", &"[redacted]")
            .field("channel", &self.channel)
            .field("address_count", &self.ip_addresses.len())
            .field("security_type", &self.security_type)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone)]
pub struct StartSession {
    pub airplay_port: u16,
    pub public_key: String,
    pub source_version: String,
    pub wired_ipv6_addresses: Vec<String>,
    pub wired_reserved: Option<u32>,
    pub wireless: Option<WirelessSession>,
    pub device_identifier: Option<String>,
    pub sdk_version: Option<String>,
    pub cluster_asset: Option<(String, u32)>,
    pub mutual_auth: Option<bool>,
}
impl StartSession {
    pub fn build(&self) -> Result<ControlMessage> {
        if self.airplay_port == 0 || self.public_key.is_empty() || self.source_version.is_empty() {
            return Err(Error::Invalid("start-session identity"));
        }
        let mut params = Vec::new();
        if !self.wired_ipv6_addresses.is_empty() || self.wired_reserved.is_some() {
            let mut wired = Vec::new();
            for address in &self.wired_ipv6_addresses {
                wired.push(Parameter::string(0, address)?);
            }
            if let Some(value) = self.wired_reserved {
                wired.push(Parameter::u32(1, value));
            }
            params.push(Parameter::group(0, &wired)?);
        }
        if let Some(w) = &self.wireless {
            if w.ssid.trim().is_empty()
                || w.ip_addresses.is_empty()
                || (w.security_type != 0 && w.passphrase.is_empty())
            {
                return Err(Error::Invalid("wireless start-session configuration"));
            }
            let mut group = vec![
                Parameter::string(0, &w.ssid)?,
                Parameter::string(1, &w.passphrase)?,
                Parameter::u8(2, w.channel),
            ];
            for address in &w.ip_addresses {
                group.push(Parameter::string(3, address)?);
            }
            group.push(Parameter::u8(4, w.security_type));
            params.push(Parameter::group(1, &group)?);
        }
        params.push(Parameter::u32(2, self.airplay_port as u32));
        if let Some(id) = &self.device_identifier {
            params.push(Parameter::string(3, id)?);
        }
        params.extend([
            Parameter::string(4, &self.public_key)?,
            Parameter::string(5, &self.source_version)?,
        ]);
        if let Some(version) = &self.sdk_version {
            params.push(Parameter::string(6, version)?);
        }
        if let Some((id, version)) = &self.cluster_asset {
            params.push(Parameter::group(
                7,
                &[Parameter::string(0, id)?, Parameter::u32(1, *version)],
            )?);
        }
        if let Some(value) = self.mutual_auth {
            params.push(Parameter::u8(8, u8::from(value)));
        }
        ControlMessage::new(0x4301, &params)
    }
}

#[derive(Debug, Clone)]
pub enum IdentificationTransport {
    Wired {
        usb_interface: u8,
    },
    Wireless {
        bluetooth_mac: [u8; 6],
        ssid: String,
    },
}
#[derive(Debug, Clone)]
pub struct Identification {
    pub name: String,
    pub model: String,
    pub manufacturer: String,
    pub serial: String,
    pub firmware_version: String,
    pub hardware_version: String,
    pub language: String,
    pub external_accessory_protocol: String,
    pub transport: IdentificationTransport,
    /// Advertise only handlers implemented by the host. No automatic claims of vehicle/location capabilities.
    pub sent_messages: Vec<u16>,
    pub received_messages: Vec<u16>,
    /// Optional component TLVs (e.g. location or vehicle); host owns their associated runtime handlers.
    pub extra_components: Vec<Parameter>,
}
impl Identification {
    pub fn build(&self) -> Result<ControlMessage> {
        let strings = [
            &self.name,
            &self.model,
            &self.manufacturer,
            &self.serial,
            &self.firmware_version,
            &self.hardware_version,
        ];
        if strings.iter().any(|s| s.trim().is_empty())
            || self.language.trim().is_empty()
            || self.external_accessory_protocol.trim().is_empty()
        {
            return Err(Error::Invalid("empty identification identity"));
        }
        let mut params = Vec::new();
        for (id, value) in strings.into_iter().enumerate() {
            params.push(Parameter::string(id as u16, value)?);
        }
        params.push(Parameter::new(
            6,
            self.sent_messages
                .iter()
                .flat_map(|id| id.to_be_bytes())
                .collect::<Vec<_>>(),
        ));
        params.push(Parameter::new(
            7,
            self.received_messages
                .iter()
                .flat_map(|id| id.to_be_bytes())
                .collect::<Vec<_>>(),
        ));
        params.push(Parameter::u8(
            8,
            if matches!(self.transport, IdentificationTransport::Wired { .. }) {
                2
            } else {
                0
            },
        ));
        params.push(Parameter::u16(9, 20));
        params.push(Parameter::group(
            10,
            &[
                Parameter::u8(0, 1),
                Parameter::string(1, &self.external_accessory_protocol)?,
                Parameter::u8(2, 0),
            ],
        )?);
        params.extend([
            Parameter::string(12, &self.language)?,
            Parameter::string(13, &self.language)?,
        ]);
        match &self.transport {
            IdentificationTransport::Wired { usb_interface } => params.push(Parameter::group(
                16,
                &[
                    Parameter::u16(0, 0),
                    Parameter::string(1, "USBHostTransport")?,
                    Parameter::empty(2),
                    Parameter::u8(3, *usb_interface),
                    Parameter::empty(4),
                ],
            )?),
            IdentificationTransport::Wireless {
                bluetooth_mac,
                ssid,
            } => {
                if ssid.trim().is_empty() {
                    return Err(Error::Invalid("empty SSID"));
                }
                params.push(Parameter::group(
                    17,
                    &[
                        Parameter::u16(0, 0),
                        Parameter::string(1, "blue")?,
                        Parameter::empty(2),
                        Parameter::new(3, bluetooth_mac.to_vec()),
                        Parameter::string(4, "blue")?,
                        Parameter::empty(5),
                    ],
                )?);
                params.push(Parameter::group(
                    24,
                    &[
                        Parameter::u16(0, 1),
                        Parameter::string(1, ssid)?,
                        Parameter::empty(2),
                        Parameter::u16(3, 1),
                        Parameter::empty(4),
                        Parameter::empty(5),
                    ],
                )?);
            }
        }
        params.extend(self.extra_components.clone());
        // Only advertise route-guidance capacity when route updates are actually declared.
        if self.received_messages.contains(&0x5201) {
            params.push(Parameter::group(
                30,
                &[
                    Parameter::u16(0, 42),
                    Parameter::string(1, "RouteGuidance")?,
                    Parameter::u16(2, 64),
                    Parameter::u16(4, 64),
                    Parameter::u16(6, 8),
                ],
            )?);
        }
        ControlMessage::new(IDENTIFICATION_INFORMATION, &params)
    }
}

/// Exact subscription bodies from the reference host; caller advertises/uses only enabled ones.
pub fn subscriptions() -> Result<Vec<ControlMessage>> {
    let empties = |ids: &[u16]| {
        ids.iter()
            .copied()
            .map(Parameter::empty)
            .collect::<Vec<_>>()
    };
    Ok(vec![
        ControlMessage::new(
            0x5000,
            &[
                Parameter::group(0, &empties(&[1, 4, 6, 12, 26]))?,
                Parameter::group(1, &empties(&[0, 1, 7]))?,
            ],
        )?,
        ControlMessage::new(
            0x5200,
            &[
                Parameter::u16(0, 42),
                Parameter::empty(1),
                Parameter::empty(2),
            ],
        )?,
        ControlMessage::new(0xae00, &empties(&[4, 5, 6]))?,
        ControlMessage::new(0x4157, &empties(&[0, 4, 5]))?,
        ControlMessage::new(0x4154, &empties(&[0, 1, 2, 3, 4, 11]))?,
    ])
}

/// Empty Stop messages paired with the five update services in `subscriptions`.
/// The source identifies each Start/Stop pair together in MessagesSentByAccessory.
/// Creating or sending these requests does not acknowledge receipt by the phone.
pub fn stop_subscriptions() -> Vec<ControlMessage> {
    [0x5002, 0x5203, 0xae02, 0x4159, 0x4156]
        .into_iter()
        .map(ControlMessage::empty)
        .collect()
}
