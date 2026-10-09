// SPDX-License-Identifier: GPL-3.0-only
//! Strict parsers are independent of native libraries and testable everywhere.
use carplay_core::media::{AudioCodec, AudioFormat, VideoCodec};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid media framing: {0}")]
pub struct FramingError(pub &'static str);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoParameters {
    pub nal_length_size: u8,
    pub annex_b: Vec<u8>,
}

fn read_nal(input: &[u8], position: &mut usize, out: &mut Vec<u8>) -> Result<(), FramingError> {
    let size = input
        .get(*position..*position + 2)
        .ok_or(FramingError("missing NAL length"))?;
    let size = u16::from_be_bytes([size[0], size[1]]) as usize;
    *position += 2;
    if size == 0 {
        return Err(FramingError("empty NAL"));
    }
    let nal = input
        .get(*position..*position + size)
        .ok_or(FramingError("truncated NAL"))?;
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
    *position += size;
    Ok(())
}

/// Extract parameter sets from AVCDecoderConfigurationRecord or
/// HEVCDecoderConfigurationRecord. No scan for magic values inside payloads.
pub fn video_parameters(codec: VideoCodec, input: &[u8]) -> Result<VideoParameters, FramingError> {
    if input.first() != Some(&1) {
        return Err(FramingError("unsupported configuration version"));
    }
    let mut out = Vec::new();
    let length_size = match codec {
        VideoCodec::H264 => {
            if input.len() < 7 {
                return Err(FramingError("short avcC"));
            }
            let length_size = (input[4] & 3) + 1;
            let sps_count = input[5] & 31;
            if sps_count == 0 {
                return Err(FramingError("avcC has no SPS"));
            }
            let mut p = 6;
            for _ in 0..sps_count {
                read_nal(input, &mut p, &mut out)?;
            }
            let pps_count = *input.get(p).ok_or(FramingError("avcC has no PPS count"))?;
            p += 1;
            if pps_count == 0 {
                return Err(FramingError("avcC has no PPS"));
            }
            for _ in 0..pps_count {
                read_nal(input, &mut p, &mut out)?;
            }
            // High-profile avcC may include chroma/bit-depth fields and SPS ext.
            if p < input.len() {
                if !matches!(
                    input[1],
                    100 | 110 | 122 | 144 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
                ) {
                    return Err(FramingError("unexpected avcC extension"));
                }
                let ext = input
                    .get(p..p + 4)
                    .ok_or(FramingError("truncated avcC extension"))?;
                p += 4;
                for _ in 0..ext[3] {
                    read_nal(input, &mut p, &mut out)?;
                }
                if p != input.len() {
                    return Err(FramingError("trailing avcC data"));
                }
            }
            length_size
        }
        VideoCodec::H265 => {
            if input.len() < 23 {
                return Err(FramingError("short hvcC"));
            }
            let length_size = (input[21] & 3) + 1;
            let mut p = 23;
            for _ in 0..input[22] {
                let header = input
                    .get(p..p + 3)
                    .ok_or(FramingError("missing hvcC array"))?;
                let count = u16::from_be_bytes([header[1], header[2]]);
                p += 3;
                for _ in 0..count {
                    read_nal(input, &mut p, &mut out)?;
                }
            }
            if out.is_empty() || p != input.len() {
                return Err(FramingError("empty or trailing hvcC data"));
            }
            length_size
        }
    };
    if length_size == 3 {
        return Err(FramingError("reserved NAL length size"));
    }
    Ok(VideoParameters {
        nal_length_size: length_size,
        annex_b: out,
    })
}

pub fn validate_annex_b(input: &[u8]) -> Result<(), FramingError> {
    let size = if input.starts_with(&[0, 0, 0, 1]) {
        4
    } else if input.starts_with(&[0, 0, 1]) {
        3
    } else {
        return Err(FramingError("expected Annex B video"));
    };
    if input.len() <= size {
        return Err(FramingError("empty Annex B video"));
    }
    Ok(())
}

/// MPEG-4 AudioSpecificConfig for raw AAC-LC access units.
pub fn aac_config(format: AudioFormat) -> Result<[u8; 2], FramingError> {
    if format.codec != AudioCodec::AacLc || !(1..=2).contains(&format.channels) {
        return Err(FramingError("unsupported AAC format"));
    }
    let rates = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    let index = rates
        .iter()
        .position(|rate| *rate == format.rate)
        .ok_or(FramingError("unsupported AAC sample rate"))? as u16;
    Ok(((2u16 << 11) | (index << 7) | ((format.channels as u16) << 3)).to_be_bytes())
}

pub fn validate_audio_format(format: AudioFormat) -> Result<(), FramingError> {
    if !(1..=2).contains(&format.channels) {
        return Err(FramingError("unsupported audio channels"));
    }
    match format.codec {
        AudioCodec::Lpcm => {
            if ![8000, 16000, 24000, 32000, 44100, 48000].contains(&format.rate) {
                return Err(FramingError("unsupported PCM rate"));
            }
        }
        AudioCodec::AacLc => {
            aac_config(format)?;
        }
        AudioCodec::Opus => {
            if format.rate != 48000 {
                return Err(FramingError("unsupported Opus rate"));
            }
        }
    }
    Ok(())
}

pub fn validate_audio(format: AudioFormat, payload: &[u8]) -> Result<(), FramingError> {
    validate_audio_format(format)?;
    if payload.is_empty() || payload.len() > 1024 * 1024 {
        return Err(FramingError("empty or oversized audio"));
    }
    if format.codec == AudioCodec::Lpcm
        && !payload.len().is_multiple_of(format.channels as usize * 2)
    {
        return Err(FramingError("misaligned PCM samples"));
    }
    if format.codec == AudioCodec::Opus && payload.len() > 1275 * 48 {
        return Err(FramingError("unsupported Opus packet"));
    }
    Ok(())
}

/// Copy possibly padded native rows into a packed RGBA frame. Reject excessive
/// dimensions before allocating, and never expose uninitialized padding bytes.
pub fn packed_rgba(
    width: u32,
    height: u32,
    stride: usize,
    data: &[u8],
) -> Result<Vec<u8>, FramingError> {
    let row = (width as usize)
        .checked_mul(4)
        .ok_or(FramingError("RGBA width overflow"))?;
    let total = row
        .checked_mul(height as usize)
        .ok_or(FramingError("RGBA dimensions overflow"))?;
    if width == 0 || height == 0 || total > 64 * 1024 * 1024 || stride < row {
        return Err(FramingError("invalid RGBA dimensions or stride"));
    }
    let needed = stride
        .checked_mul(height as usize - 1)
        .and_then(|offset| offset.checked_add(row))
        .ok_or(FramingError("RGBA stride overflow"))?;
    if data.len() < needed {
        return Err(FramingError("truncated RGBA plane"));
    }
    let mut packed = Vec::with_capacity(total);
    for y in 0..height as usize {
        packed.extend_from_slice(&data[y * stride..y * stride + row]);
    }
    Ok(packed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn avcc_extracts_sps_pps_and_rejects_all_truncations() {
        let config = [
            1, 66, 0, 30, 0xff, 0xe1, 0, 4, 0x67, 66, 0, 30, 1, 0, 2, 0x68, 0xce,
        ];
        let p = video_parameters(VideoCodec::H264, &config).unwrap();
        assert_eq!(p.nal_length_size, 4);
        assert_eq!(
            p.annex_b,
            [0, 0, 0, 1, 0x67, 66, 0, 30, 0, 0, 0, 1, 0x68, 0xce]
        );
        for end in 0..config.len() {
            assert!(video_parameters(VideoCodec::H264, &config[..end]).is_err());
        }
        let mut bad = config;
        bad[4] = 0xfe;
        assert!(video_parameters(VideoCodec::H264, &bad).is_err());
    }
    #[test]
    fn hvcc_extracts_arrays_and_rejects_trailing_bytes() {
        let mut config = vec![0; 23];
        config[0] = 1;
        config[21] = 3;
        config[22] = 2;
        config.extend([0xa0, 0, 1, 0, 2, 0x40, 1, 0xa1, 0, 1, 0, 2, 0x42, 1]);
        assert_eq!(
            video_parameters(VideoCodec::H265, &config).unwrap().annex_b,
            [0, 0, 0, 1, 0x40, 1, 0, 0, 0, 1, 0x42, 1]
        );
        for end in 0..config.len() {
            assert!(video_parameters(VideoCodec::H265, &config[..end]).is_err());
        }
        config.push(0);
        assert!(video_parameters(VideoCodec::H265, &config).is_err());
    }
    #[test]
    fn aac_config_has_known_lc_values() {
        assert_eq!(
            aac_config(AudioFormat {
                codec: AudioCodec::AacLc,
                rate: 44100,
                channels: 2
            })
            .unwrap(),
            [0x12, 0x10]
        );
        assert_eq!(
            aac_config(AudioFormat {
                codec: AudioCodec::AacLc,
                rate: 48000,
                channels: 2
            })
            .unwrap(),
            [0x11, 0x90]
        );
        assert!(
            aac_config(AudioFormat {
                codec: AudioCodec::AacLc,
                rate: 123,
                channels: 2
            })
            .is_err()
        );
    }
    #[test]
    fn pcm_requires_complete_interleaved_frames() {
        let f = AudioFormat {
            codec: AudioCodec::Lpcm,
            rate: 48000,
            channels: 2,
        };
        assert!(validate_audio(f, &[0, 1, 255, 254]).is_ok());
        assert!(validate_audio(f, &[0, 1, 255]).is_err());
    }
    #[test]
    fn rgba_removes_padding_and_checks_bounds_before_copying() {
        assert_eq!(
            packed_rgba(1, 2, 8, &[1, 2, 3, 4, 99, 99, 99, 99, 5, 6, 7, 8]).unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert!(packed_rgba(1, 2, 8, &[0; 11]).is_err());
        assert!(packed_rgba(u32::MAX, u32::MAX, usize::MAX, &[]).is_err());
        assert!(packed_rgba(1, 1, 3, &[0; 4]).is_err());
    }
}
