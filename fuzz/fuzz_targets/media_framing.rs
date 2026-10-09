// SPDX-License-Identifier: GPL-3.0-only
// Exercises transport framing and native-decoder configuration without loading GStreamer.
#![no_main]
use carplay_core::media::{self, AudioCodec, AudioFormat, VideoCodec};
use carplay_media::framing;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let data = &data[..data.len().min(1_048_576)];
    let _ = media::screen_header(data);
    let _ = media::rtp_payload(data);
    let _ = media::annex_b(data);
    let _ = media::codec_config(data);
    let _ = framing::validate_annex_b(data);
    for codec in [VideoCodec::H264, VideoCodec::H265] {
        let _ = framing::video_parameters(codec, data);
    }
    for codec in [AudioCodec::Lpcm, AudioCodec::AacLc, AudioCodec::Opus] {
        let _ = framing::validate_audio(
            AudioFormat {
                codec,
                rate: 48000,
                channels: 2,
            },
            data,
        );
    }
    if data.len() >= 12 {
        let width = u32::from_le_bytes(data[..4].try_into().unwrap());
        let height = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let stride = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
        let _ = framing::packed_rgba(width, height, stride, &data[12..]);
    }
});
