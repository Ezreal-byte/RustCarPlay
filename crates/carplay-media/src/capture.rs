// SPDX-License-Identifier: GPL-3.0-only
//! Explicitly activated microphone capture; construction/probing opens no device.
use carplay_core::media::{AudioCodec, CaptureConfig, CaptureFactory, CaptureSession, CaptureSink};
use std::sync::Arc;

pub struct GStreamerCaptureFactory {
    #[cfg(feature = "gstreamer")]
    synthetic: bool,
}

impl GStreamerCaptureFactory {
    /// Initialize GStreamer without constructing or activating an audio source.
    pub fn new() -> Result<Self, crate::Error> {
        Self::create(false)
    }

    /// Test source with generated audio. Never accesses a microphone.
    pub fn synthetic() -> Result<Self, crate::Error> {
        Self::create(true)
    }

    fn create(synthetic: bool) -> Result<Self, crate::Error> {
        #[cfg(feature = "gstreamer")]
        {
            gst::init().map_err(|error| crate::Error::Initialization(error.to_string()))?;
            Ok(Self { synthetic })
        }
        #[cfg(not(feature = "gstreamer"))]
        {
            let _ = synthetic;
            Err(crate::Error::FeatureDisabled)
        }
    }
}

fn valid_configuration(config: &CaptureConfig) -> bool {
    if config.format.channels != 1
        || config.samples_per_packet == 0
        || config.samples_per_packet > 4096
    {
        return false;
    }
    match config.format.codec {
        AudioCodec::Lpcm => [8000, 16000, 24000, 32000, 44100, 48000].contains(&config.format.rate),
        AudioCodec::Opus => {
            config.format.rate == 48000
                && config.samples_per_packet == 960
                && config
                    .bitrate
                    .is_none_or(|bitrate| (6000..=128000).contains(&bitrate))
        }
        AudioCodec::AacLc => false,
    }
}

impl CaptureFactory for GStreamerCaptureFactory {
    fn supports(&self, config: &CaptureConfig) -> bool {
        if !valid_configuration(config) {
            return false;
        }
        #[cfg(feature = "gstreamer")]
        {
            let source = if self.synthetic {
                "audiotestsrc"
            } else {
                "autoaudiosrc"
            };
            [source, "audioconvert", "audioresample", "appsink"]
                .into_iter()
                .all(|name| gst::ElementFactory::find(name).is_some())
                && (config.format.codec != AudioCodec::Opus
                    || gst::ElementFactory::find("opusenc").is_some())
        }
        #[cfg(not(feature = "gstreamer"))]
        {
            false
        }
    }

    fn start(
        &self,
        config: CaptureConfig,
        output: Arc<dyn CaptureSink>,
    ) -> Result<Box<dyn CaptureSession>, String> {
        if !self.supports(&config) {
            return Err("capture format or native source/encoder is unavailable".into());
        }
        #[cfg(feature = "gstreamer")]
        {
            native::start(config, output, self.synthetic)
        }
        #[cfg(not(feature = "gstreamer"))]
        {
            let _ = output;
            Err(crate::Error::FeatureDisabled.to_string())
        }
    }
}

#[cfg(feature = "gstreamer")]
mod native {
    use super::*;
    use gst::prelude::*;
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        thread::{self, JoinHandle},
    };

    struct Pipeline(gst::Pipeline);
    impl Drop for Pipeline {
        fn drop(&mut self) {
            let _ = self.0.set_state(gst::State::Null);
        }
    }

    struct Session {
        stop: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }
    impl CaptureSession for Session {
        fn stop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
    impl Drop for Session {
        fn drop(&mut self) {
            self.stop();
        }
    }

    pub(super) fn start(
        config: CaptureConfig,
        output: Arc<dyn CaptureSink>,
        synthetic: bool,
    ) -> Result<Box<dyn CaptureSession>, String> {
        let source = if synthetic {
            "audiotestsrc is-live=true wave=sine freq=440 volume=0.25 samplesperbuffer=320"
        } else {
            "autoaudiosrc"
        };
        let encoding = match config.format.codec {
            AudioCodec::Lpcm => format!(
                "audio/x-raw,format=S16BE,layout=interleaved,rate={},channels=1",
                config.format.rate
            ),
            AudioCodec::Opus => format!(
                "audio/x-raw,format=S16LE,layout=interleaved,rate=48000,channels=1 ! opusenc frame-size=20 audio-type=voice bitrate={}",
                config.bitrate.unwrap_or(24000)
            ),
            AudioCodec::AacLc => return Err("AAC capture is not supported".into()),
        };
        let description = format!(
            "{source} ! audioconvert ! audioresample ! {encoding} ! appsink name=captured sync=false"
        );
        let pipeline = Pipeline(
            gst::parse::launch(&description)
                .map_err(|e| e.to_string())?
                .downcast::<gst::Pipeline>()
                .map_err(|_| "capture graph is not a pipeline")?,
        );
        let sink = pipeline
            .0
            .by_name("captured")
            .ok_or("capture appsink missing")?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| "capture sink is not appsink")?;
        sink.set_max_buffers(8);
        sink.set_drop(false);
        sink.set_enable_last_sample(false);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        // Only start() enters Playing; factory probing never opens an input.
        pipeline
            .0
            .set_state(gst::State::Playing)
            .map_err(|e| e.to_string())?;
        let worker = thread::Builder::new()
            .name("carplay-microphone".into())
            .spawn(move || {
                let result = run(&pipeline, &sink, config, &output, &stopped);
                // Release native capture before reporting a failure to the receiver.
                drop(pipeline);
                if let Err(reason) = result {
                    output.failed(reason);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Box::new(Session {
            stop,
            worker: Some(worker),
        }))
    }

    fn run(
        pipeline: &Pipeline,
        sink: &gst_app::AppSink,
        config: CaptureConfig,
        output: &Arc<dyn CaptureSink>,
        stop: &AtomicBool,
    ) -> Result<(), String> {
        let bus = pipeline.0.bus().ok_or("capture pipeline has no bus")?;
        let bytes_per_packet = config.samples_per_packet as usize * 2;
        let mut pending = Vec::with_capacity(bytes_per_packet * 2);
        while !stop.load(Ordering::Acquire) {
            while let Some(message) = bus.pop() {
                match message.view() {
                    gst::MessageView::Error(error) => {
                        return Err(format!("microphone capture: {}", error.error()));
                    }
                    gst::MessageView::Eos(_) => {
                        return Err("microphone source ended unexpectedly".into());
                    }
                    _ => {}
                }
            }
            let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(20)) else {
                continue;
            };
            if stop.load(Ordering::Acquire) {
                break;
            }
            let buffer = sample.buffer().ok_or("capture sample has no buffer")?;
            if buffer.flags().contains(gst::BufferFlags::HEADER) {
                continue;
            }
            let mapped = buffer.map_readable().map_err(|e| e.to_string())?;
            let bytes = mapped.as_slice();
            if bytes.is_empty() {
                continue;
            }
            if bytes.len() > 1024 * 1024 {
                return Err("native capture produced an oversized buffer".into());
            }
            match config.format.codec {
                AudioCodec::Lpcm => {
                    if !bytes.len().is_multiple_of(2) {
                        return Err("native PCM capture sample is misaligned".into());
                    }
                    pending.extend_from_slice(bytes);
                    let mut consumed = 0;
                    while pending.len() - consumed >= bytes_per_packet {
                        if stop.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        output.packet(pending[consumed..consumed + bytes_per_packet].to_vec())?;
                        consumed += bytes_per_packet;
                    }
                    pending.drain(..consumed);
                }
                AudioCodec::Opus => {
                    if bytes.len() > 1275 {
                        return Err("native Opus capture packet exceeds 20 ms packet limit".into());
                    }
                    output.packet(bytes.to_vec())?;
                }
                AudioCodec::AacLc => return Err("AAC capture is not supported".into()),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use carplay_core::media::AudioFormat;

    fn config(codec: AudioCodec) -> CaptureConfig {
        CaptureConfig {
            format: AudioFormat {
                codec,
                rate: 48000,
                channels: 1,
            },
            samples_per_packet: 960,
            bitrate: None,
        }
    }

    #[test]
    fn capture_rejects_unsupported_or_unbounded_packet_shapes() {
        assert!(valid_configuration(&config(AudioCodec::Lpcm)));
        assert!(valid_configuration(&config(AudioCodec::Opus)));
        assert!(!valid_configuration(&config(AudioCodec::AacLc)));
        let mut c = config(AudioCodec::Opus);
        c.samples_per_packet = 480;
        assert!(!valid_configuration(&c));
        let mut c = config(AudioCodec::Lpcm);
        c.samples_per_packet = 0;
        assert!(!valid_configuration(&c));
        c.samples_per_packet = u32::MAX;
        assert!(!valid_configuration(&c));
        c.samples_per_packet = 960;
        c.format.channels = 2;
        assert!(!valid_configuration(&c));
    }

    #[test]
    #[cfg(not(feature = "gstreamer"))]
    fn disabled_capture_fails_explicitly() {
        assert!(matches!(
            GStreamerCaptureFactory::new(),
            Err(crate::Error::FeatureDisabled)
        ));
    }

    #[cfg(feature = "gstreamer")]
    struct Output {
        packets: std::sync::mpsc::SyncSender<Vec<u8>>,
        errors: std::sync::Mutex<Vec<String>>,
    }
    #[cfg(feature = "gstreamer")]
    impl CaptureSink for Output {
        fn packet(&self, bytes: Vec<u8>) -> Result<(), String> {
            self.packets
                .try_send(bytes)
                .map_err(|_| "test capture queue is full".into())
        }
        fn failed(&self, reason: String) {
            self.errors.lock().unwrap().push(reason);
        }
    }

    #[test]
    #[cfg(feature = "gstreamer")]
    #[ignore = "requires native GStreamer audiotestsrc and audio conversion; never opens a microphone"]
    fn native_synthetic_pcm_has_fixed_big_endian_packets_and_stops() {
        let factory = GStreamerCaptureFactory::synthetic().unwrap();
        let mut config = config(AudioCodec::Lpcm);
        config.format.rate = 16000;
        config.samples_per_packet = 320;
        assert!(factory.supports(&config));
        let (packets, receiver) = std::sync::mpsc::sync_channel(16);
        let output = Arc::new(Output {
            packets,
            errors: Default::default(),
        });
        let mut session = factory.start(config, output.clone()).unwrap();
        for _ in 0..3 {
            let packet = receiver
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap();
            assert_eq!(packet.len(), 640);
            let samples = packet
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_be_bytes([b[0], b[1]]))
                .collect::<Vec<_>>();
            assert!(samples.iter().any(|&s| s != 0));
            assert!(
                samples.iter().all(|&s| i32::from(s).abs() < 9000),
                "unexpected PCM scale/endian"
            );
        }
        session.stop();
        while receiver.try_recv().is_ok() {}
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(80))
                .is_err()
        );
        assert!(output.errors.lock().unwrap().is_empty());
    }

    #[test]
    #[cfg(feature = "gstreamer")]
    #[ignore = "requires native GStreamer audiotestsrc and opusenc; never opens a microphone"]
    fn native_synthetic_opus_is_raw_twenty_ms_and_drop_stops() {
        let factory = GStreamerCaptureFactory::synthetic().unwrap();
        let (packets, receiver) = std::sync::mpsc::sync_channel(16);
        let output = Arc::new(Output {
            packets,
            errors: Default::default(),
        });
        let session = factory
            .start(config(AudioCodec::Opus), output.clone())
            .unwrap();
        for _ in 0..3 {
            let packet = receiver
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap();
            assert!(!packet.is_empty() && packet.len() <= 1275);
            assert!(!packet.starts_with(b"OpusHead") && !packet.starts_with(b"OpusTags"));
            // RFC 6716 section 3.1: derive packet sample count from its TOC.
            let mode = packet[0] >> 3;
            let frame_samples = if mode >= 16 {
                120usize << (mode & 3)
            } else if mode >= 12 {
                480usize << (mode & 1)
            } else {
                [480, 960, 1920, 2880][usize::from(mode & 3)]
            };
            let frame_count = match packet[0] & 3 {
                0 => 1,
                1 | 2 => 2,
                _ => usize::from(packet[1] & 0x3f),
            };
            assert_eq!(
                frame_samples * frame_count,
                960,
                "Opus packet must cover exactly 20 ms at 48 kHz"
            );
        }
        drop(session);
        while receiver.try_recv().is_ok() {}
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(80))
                .is_err()
        );
        assert!(output.errors.lock().unwrap().is_empty());
    }
}
