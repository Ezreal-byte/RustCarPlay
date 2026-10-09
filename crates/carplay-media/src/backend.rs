// SPDX-License-Identifier: GPL-3.0-only
use crate::{Error, PlaybackError, RgbaFrame, framing};
use carplay_core::media::{AudioCodec, AudioFormat, MAX_VIDEO_BODY, MediaEvent, VideoCodec};
use gst::prelude::*;
use gst_video::prelude::*;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const MAX_QUEUE_EVENTS: usize = 64;
const MAX_QUEUE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_STREAMS: usize = 8;

#[derive(Default)]
struct Shared {
    frames: RwLock<BTreeMap<u16, Arc<RgbaFrame>>>,
    frame_ready: RwLock<Option<Arc<dyn Fn() + Send + Sync>>>,
    errors: Mutex<VecDeque<PlaybackError>>,
    failure: Mutex<Option<String>>,
    queued_bytes: AtomicUsize,
    stop: AtomicBool,
}

impl Shared {
    fn fail(&self, stream: Option<u16>, message: String) {
        let mut failure = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        if failure.is_none() {
            *failure = Some(message.clone());
        }
        let mut errors = self.errors.lock().unwrap_or_else(|e| e.into_inner());
        if errors.len() == 32 {
            errors.pop_front();
        }
        errors.push_back(PlaybackError { stream, message });
    }
}

struct Queued {
    event: MediaEvent,
    bytes: usize,
    prepared_audio: Option<Pipeline>,
    completed: Option<mpsc::SyncSender<Result<(), String>>>,
}

pub(crate) struct Backend {
    shared: Arc<Shared>,
    sender: mpsc::SyncSender<Queued>,
    worker: Option<JoinHandle<()>>,
}

impl Backend {
    pub(crate) fn set_frame_ready_callback(&self, callback: Arc<dyn Fn() + Send + Sync>) {
        *self
            .shared
            .frame_ready
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(callback);
    }
    pub(crate) fn new() -> Result<Self, Error> {
        gst::init().map_err(|error| Error::Initialization(error.to_string()))?;
        // All requested codecs are checked when their stream starts; don't
        // reject PCM playback just because an optional H.265 decoder is absent.
        for factory in [
            "appsrc",
            "appsink",
            "videoconvert",
            "audioconvert",
            "audioresample",
            "decodebin",
        ] {
            if gst::ElementFactory::find(factory).is_none() {
                return Err(Error::Initialization(format!(
                    "missing GStreamer element {factory}"
                )));
            }
        }
        let shared = Arc::new(Shared::default());
        let (sender, receiver) = mpsc::sync_channel(MAX_QUEUE_EVENTS);
        let state = shared.clone();
        let worker = thread::Builder::new()
            .name("carplay-media".into())
            .spawn(move || run(receiver, state))
            .map_err(|e| Error::Initialization(e.to_string()))?;
        Ok(Self {
            shared,
            sender,
            worker: Some(worker),
        })
    }

    pub(crate) fn latest_frame(&self, stream: u16) -> Option<Arc<RgbaFrame>> {
        self.shared
            .frames
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&stream)
            .cloned()
    }

    pub(crate) fn latest_video_frame(&self) -> Option<Arc<RgbaFrame>> {
        self.shared
            .frames
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .next()
            .cloned()
    }

    pub(crate) fn take_errors(&self) -> Vec<PlaybackError> {
        self.shared
            .errors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }

    pub(crate) fn send(&self, event: MediaEvent) -> Result<(), String> {
        self.send_with_audio_builder(event, audio_pipeline)
    }

    fn send_with_audio_builder(
        &self,
        event: MediaEvent,
        build_audio: impl FnOnce(AudioFormat) -> Result<Pipeline, String>,
    ) -> Result<(), String> {
        if matches!(event, MediaEvent::Stop { .. }) {
            // Teardown cannot be dropped behind a full data queue. The receiver
            // sends Stop from its worker after producers have joined; wait for
            // native Null/slot clearing before permitting the next session.
            let (completed, done) = mpsc::sync_channel(1);
            self.sender
                .send(Queued {
                    event,
                    bytes: 0,
                    prepared_audio: None,
                    completed: Some(completed),
                })
                .map_err(|_| "media worker stopped".to_string())?;
            return done
                .recv()
                .map_err(|_| "media worker stopped during teardown".to_string())?;
        }
        if let Some(error) = self
            .shared
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return Err(error.clone());
        }
        if let MediaEvent::AudioConfig {
            audio_type, format, ..
        } = &event
        {
            if audio_type.len() > 128 {
                return Err("audio type is too long".into());
            }
            framing::validate_audio_format(*format).map_err(|e| e.to_string())?;
            // Device discovery/opening runs on the SETUP caller, never on the
            // media worker. Existing video and other audio streams keep draining
            // their bounded queues while this native operation completes.
            let pipeline = build_audio(*format)?;
            let (completed, done) = mpsc::sync_channel(1);
            self.sender
                .send(Queued {
                    event,
                    bytes: 0,
                    prepared_audio: Some(pipeline),
                    completed: Some(completed),
                })
                .map_err(|_| "media worker stopped during audio setup".to_string())?;
            return done
                .recv()
                .map_err(|_| "media worker stopped during audio setup".to_string())?;
        }
        let bytes = match &event {
            MediaEvent::VideoConfig { data, .. } | MediaEvent::VideoFrame { data, .. } => {
                if data.is_empty() || data.len() > MAX_VIDEO_BODY {
                    return Err("empty or oversized video event".into());
                }
                data.len()
            }
            MediaEvent::Audio {
                data,
                audio_type,
                format,
                ..
            } => {
                if audio_type.len() > 128 {
                    return Err("audio type is too long".into());
                }
                framing::validate_audio(*format, data).map_err(|e| e.to_string())?;
                data.len() + audio_type.len()
            }
            MediaEvent::AudioConfig { .. } | MediaEvent::Stop { .. } => unreachable!(),
        };
        reserve_bytes(&self.shared.queued_bytes, bytes)?;
        match self.sender.try_send(Queued {
            event,
            bytes,
            prepared_audio: None,
            completed: None,
        }) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.shared.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
                Err(match error {
                    mpsc::TrySendError::Full(_) => "media event queue is full",
                    mpsc::TrySendError::Disconnected(_) => "media worker stopped",
                }
                .into())
            }
        }
    }
}

// Rust 1.88 lacks the renamed AtomicUsize::try_update API. Keep the equivalent
// fetch_update call for the declared MSRV, despite its Rust 1.99 deprecation.
#[allow(deprecated)]
fn reserve_bytes(counter: &AtomicUsize, bytes: usize) -> Result<(), String> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
            old.checked_add(bytes).filter(|n| *n <= MAX_QUEUE_BYTES)
        })
        .map(|_| ())
        .map_err(|_| "media byte queue is full".into())
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Pipeline {
    pipeline: gst::Pipeline,
    source: gst_app::AppSrc,
    audio_format: Option<AudioFormat>,
}

impl Pipeline {
    fn from_description(description: &str) -> Result<Self, String> {
        let pipeline = gst::parse::launch(description)
            .map_err(|e| e.to_string())?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "media graph is not a pipeline")?;
        let source = pipeline
            .by_name("source")
            .ok_or("pipeline appsrc missing")?
            .downcast::<gst_app::AppSrc>()
            .map_err(|_| "pipeline source is not appsrc")?;
        source.set_format(gst::Format::Time);
        source.set_is_live(true);
        source.set_do_timestamp(true);
        source.set_block(false);
        source.set_max_bytes(MAX_SOURCE_BYTES);
        Ok(Self {
            pipeline,
            source,
            audio_format: None,
        })
    }

    fn start(&self) -> Result<(), String> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn push(&self, data: Vec<u8>, header: bool) -> Result<(), String> {
        // appsrc max-bytes is a threshold, not a hard cap with block=false.
        // A single worker pushes, so the explicit admission check bounds it.
        if self
            .source
            .current_level_bytes()
            .saturating_add(data.len() as u64)
            > MAX_SOURCE_BYTES
        {
            return Err(
                "native decoder input queue is full; restart stream at a configuration/keyframe"
                    .into(),
            );
        }
        let mut buffer = gst::Buffer::from_mut_slice(data);
        if header {
            buffer
                .get_mut()
                .ok_or("GStreamer buffer is not writable")?
                .set_flags(gst::BufferFlags::HEADER);
        }
        self.source
            .push_buffer(buffer)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn error(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        while let Some(message) = bus.pop() {
            match message.view() {
                gst::MessageView::Error(error) => return Some(error.error().to_string()),
                gst::MessageView::Eos(_) => {
                    return Some("decoder unexpectedly reached end of stream".into());
                }
                _ => {}
            }
        }
        None
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn video_pipeline(
    stream: u16,
    codec: VideoCodec,
    config: &[u8],
    shared: Arc<Shared>,
) -> Result<Pipeline, String> {
    let parameters = framing::video_parameters(codec, config).map_err(|e| e.to_string())?;
    let (media, description) = match codec {
        VideoCodec::H264 => (
            "video/x-h264",
            "appsrc name=source ! h264parse ! avdec_h264 thread-type=slice max-threads=4 ! videoconvert ! video/x-raw,format=RGBA ! appsink name=frames",
        ),
        VideoCodec::H265 => (
            "video/x-h265",
            "appsrc name=source ! h265parse ! avdec_h265 thread-type=slice max-threads=4 ! videoconvert ! video/x-raw,format=RGBA ! appsink name=frames",
        ),
    };
    let pipeline = Pipeline::from_description(description)?;
    pipeline.source.set_caps(Some(
        &gst::Caps::builder(media)
            .field("stream-format", "byte-stream")
            .field("alignment", "au")
            .build(),
    ));
    let sink = pipeline
        .pipeline
        .by_name("frames")
        .ok_or("video appsink missing")?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| "video sink is not appsink")?;
    sink.set_max_buffers(1);
    sink.set_drop(true);
    sink.set_sync(false);
    sink.set_enable_last_sample(false);
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                match decode_rgba(&sample) {
                    Ok(frame) => {
                        shared
                            .frames
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(stream, Arc::new(frame));
                        let callback = shared
                            .frame_ready
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        if let Some(callback) = callback {
                            callback();
                        }
                        Ok(gst::FlowSuccess::Ok)
                    }
                    Err(error) => {
                        shared.fail(Some(stream), error);
                        Err(gst::FlowError::Error)
                    }
                }
            })
            .build(),
    );
    pipeline.start()?;
    pipeline.push(parameters.annex_b, true)?;
    Ok(pipeline)
}

fn decode_rgba(sample: &gst::Sample) -> Result<RgbaFrame, String> {
    let caps = sample.caps().ok_or("decoded sample has no caps")?;
    let info = gst_video::VideoInfo::from_caps(caps).map_err(|e| e.to_string())?;
    if info.format() != gst_video::VideoFormat::Rgba {
        return Err("decoded sample is not RGBA".into());
    }
    let buffer = sample.buffer().ok_or("decoded sample has no buffer")?;
    let mapped = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
        .map_err(|e| e.to_string())?;
    let stride =
        usize::try_from(mapped.plane_stride()[0]).map_err(|_| "negative RGBA row stride")?;
    let data = mapped.plane_data(0).map_err(|e| e.to_string())?;
    let rgba = framing::packed_rgba(info.width(), info.height(), stride, data)
        .map_err(|e| e.to_string())?;
    Ok(RgbaFrame {
        width: info.width(),
        height: info.height(),
        rgba,
        pts_ns: buffer.pts().map(|t| t.nseconds()),
    })
}

fn audio_pipeline(format: AudioFormat) -> Result<Pipeline, String> {
    audio_pipeline_with_capture(format, false)
}

// Capture is used by native smoke tests to verify decoded samples without
// opening a speaker. Production audio_pipeline always selects autoaudiosink.
fn audio_pipeline_with_capture(format: AudioFormat, capture: bool) -> Result<Pipeline, String> {
    let (description, caps) = match format.codec {
        AudioCodec::Lpcm => (
            "appsrc name=source ! audioconvert ! audioresample ! autoaudiosink",
            gst::Caps::builder("audio/x-raw")
                .field("format", "S16BE")
                .field("layout", "interleaved")
                .field("rate", format.rate as i32)
                .field("channels", format.channels as i32)
                .build(),
        ),
        AudioCodec::AacLc => {
            let config = framing::aac_config(format).map_err(|e| e.to_string())?;
            (
                "appsrc name=source ! aacparse ! decodebin ! audioconvert ! audioresample ! autoaudiosink",
                gst::Caps::builder("audio/mpeg")
                    .field("mpegversion", 4i32)
                    .field("stream-format", "raw")
                    .field("rate", format.rate as i32)
                    .field("channels", format.channels as i32)
                    .field("codec_data", gst::Buffer::from_slice(config.to_vec()))
                    .build(),
            )
        }
        AudioCodec::Opus => (
            "appsrc name=source ! opusdec ! audioconvert ! audioresample ! autoaudiosink",
            gst::Caps::builder("audio/x-opus")
                .field("rate", format.rate as i32)
                .field("channels", format.channels as i32)
                .field("channel-mapping-family", 0i32)
                .field("stream-count", 1i32)
                .field("coupled-count", i32::from(format.channels == 2))
                .build(),
        ),
    };
    let description = if capture {
        description.replace(
            "autoaudiosink",
            "audio/x-raw,format=S16LE,layout=interleaved ! appsink name=decoded sync=false",
        )
    } else {
        description.to_owned()
    };
    let mut pipeline = Pipeline::from_description(&description)?;
    pipeline.source.set_caps(Some(&caps));
    pipeline.audio_format = Some(format);
    pipeline.start()?;
    Ok(pipeline)
}

fn run(receiver: mpsc::Receiver<Queued>, shared: Arc<Shared>) {
    let mut videos = BTreeMap::<u16, Pipeline>::new();
    let mut audio = BTreeMap::<(u16, String), Pipeline>::new();
    while !shared.stop.load(Ordering::Acquire) {
        if shared
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            break;
        }
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(Queued {
                event,
                bytes,
                prepared_audio,
                completed,
            }) => {
                shared.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
                let stream = match &event {
                    MediaEvent::VideoConfig { stream, .. }
                    | MediaEvent::VideoFrame { stream, .. }
                    | MediaEvent::AudioConfig { stream, .. }
                    | MediaEvent::Audio { stream, .. }
                    | MediaEvent::Stop { stream } => *stream,
                };
                let result = handle(event, prepared_audio, &mut videos, &mut audio, &shared);
                if let Some(completed) = completed {
                    let _ = completed.send(result.clone());
                }
                if let Err(error) = result {
                    shared.fail(Some(stream), error);
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        for (stream, pipeline) in videos
            .iter()
            .map(|(id, p)| (*id, p))
            .chain(audio.iter().map(|((id, _), p)| (*id, p)))
        {
            if let Some(error) = pipeline.error() {
                shared.fail(Some(stream), error);
                break;
            }
        }
    }
    // Drop pipelines before clearing slots: callbacks finish when state is Null.
    drop(videos);
    drop(audio);
    shared
        .frames
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

fn handle(
    event: MediaEvent,
    prepared_audio: Option<Pipeline>,
    videos: &mut BTreeMap<u16, Pipeline>,
    audio: &mut BTreeMap<(u16, String), Pipeline>,
    shared: &Arc<Shared>,
) -> Result<(), String> {
    match event {
        MediaEvent::VideoConfig {
            stream,
            codec,
            data,
        } => {
            videos.remove(&stream);
            shared
                .frames
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&stream);
            if videos.len() + audio.len() >= MAX_STREAMS {
                return Err("too many media streams".into());
            }
            videos.insert(
                stream,
                video_pipeline(stream, codec, &data, shared.clone())?,
            );
        }
        MediaEvent::VideoFrame {
            stream,
            data,
            sender_ns: _,
        } => {
            framing::validate_annex_b(&data).map_err(|e| e.to_string())?;
            videos
                .get(&stream)
                .ok_or("video frame received before codec configuration")?
                .push(data, false)?;
        }
        MediaEvent::AudioConfig {
            stream,
            audio_type,
            format,
        } => {
            let key = (stream, audio_type);
            if !audio.contains_key(&key) && videos.len() + audio.len() >= MAX_STREAMS {
                return Err("too many media streams".into());
            }
            let pipeline = prepared_audio.ok_or("audio configuration was not prepared")?;
            if pipeline.audio_format != Some(format) {
                return Err("prepared audio format mismatch".into());
            }
            if let Some(error) = pipeline.error() {
                return Err(error);
            }
            audio.insert(key, pipeline);
        }
        MediaEvent::Audio {
            stream,
            audio_type,
            format,
            timestamp: _,
            data,
        } => {
            let pipeline = audio
                .get(&(stream, audio_type))
                .ok_or("audio received before stream preparation")?;
            if pipeline.audio_format != Some(format) {
                return Err("audio format changed without stream preparation".into());
            }
            pipeline.push(data, false)?;
        }
        MediaEvent::Stop { stream } => {
            videos.remove(&stream);
            audio.retain(|(id, _), _| *id != stream);
            shared
                .frames
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&stream);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queue_reservation_never_exceeds_limit_or_wraps() {
        let count = AtomicUsize::new(0);
        reserve_bytes(&count, MAX_QUEUE_BYTES).unwrap();
        assert!(reserve_bytes(&count, 1).is_err());
        assert_eq!(count.load(Ordering::Relaxed), MAX_QUEUE_BYTES);
        assert!(reserve_bytes(&count, usize::MAX).is_err());
        count.fetch_sub(1024, Ordering::Relaxed);
        reserve_bytes(&count, 1024).unwrap();
    }

    #[test]
    fn stop_survives_full_queue_and_waits_for_teardown() {
        let (sender, receiver) = mpsc::sync_channel(MAX_QUEUE_EVENTS);
        for _ in 0..MAX_QUEUE_EVENTS {
            sender
                .try_send(Queued {
                    event: MediaEvent::Stop { stream: 9 },
                    bytes: 0,
                    prepared_audio: None,
                    completed: None,
                })
                .unwrap();
        }
        let (drain, ready) = mpsc::sync_channel(0);
        let worker = thread::spawn(move || {
            ready.recv().unwrap();
            for queued in receiver {
                if let Some(completed) = queued.completed {
                    completed.send(Ok(())).unwrap();
                    break;
                }
            }
        });
        let backend = Backend {
            shared: Arc::new(Shared::default()),
            sender,
            worker: Some(worker),
        };
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let stopper = thread::spawn(move || {
            result_tx
                .send(backend.send(MediaEvent::Stop { stream: 9 }))
                .unwrap();
        });
        assert!(matches!(
            result_rx.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drain.send(()).unwrap();
        result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        stopper.join().unwrap();
    }

    #[test]
    fn stop_returns_error_when_worker_exits_before_acknowledging() {
        let (sender, receiver) = mpsc::sync_channel::<Queued>(MAX_QUEUE_EVENTS);
        let worker = thread::spawn(move || {
            drop(receiver.recv().unwrap());
        });
        let backend = Backend {
            shared: Arc::new(Shared::default()),
            sender,
            worker: Some(worker),
        };
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let stopper = thread::spawn(move || {
            result_tx
                .send(backend.send(MediaEvent::Stop { stream: 9 }))
                .unwrap();
        });
        assert!(
            result_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_err()
        );
        stopper.join().unwrap();
    }

    #[test]
    #[ignore = "requires native GStreamer audio conversion; does not open speakers"]
    fn slow_audio_device_setup_does_not_block_playback_or_complete_early() {
        let backend = Arc::new(Backend::new().unwrap());
        let format = AudioFormat {
            codec: AudioCodec::Lpcm,
            rate: 48000,
            channels: 2,
        };
        let config = |stream| MediaEvent::AudioConfig {
            stream,
            audio_type: "media".into(),
            format,
        };
        backend
            .send_with_audio_builder(config(100), |format| {
                audio_pipeline_with_capture(format, true)
            })
            .unwrap();
        let (opening, opened) = mpsc::sync_channel(1);
        let (release, proceed) = mpsc::sync_channel(1);
        let (result, done) = mpsc::sync_channel(1);
        let preparing = backend.clone();
        let preparation = thread::spawn(move || {
            let outcome = preparing.send_with_audio_builder(
                MediaEvent::AudioConfig {
                    stream: 101,
                    audio_type: "alert".into(),
                    format,
                },
                |format| {
                    opening.send(()).unwrap();
                    proceed.recv_timeout(Duration::from_secs(5)).unwrap();
                    audio_pipeline_with_capture(format, true)
                },
            );
            result.send(outcome).unwrap();
        });
        opened.recv_timeout(Duration::from_secs(2)).unwrap();
        // More than a queue's worth of 5 ms wired PCM arrives while another
        // device/stream is opening. A shared-worker initializer would fill 64
        // events here and tear down the whole session before setup completes.
        for packet in 0..140 {
            backend
                .send(MediaEvent::Audio {
                    stream: 100,
                    audio_type: "media".into(),
                    format,
                    timestamp: packet * 240,
                    data: vec![0; 960],
                })
                .unwrap();
            thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert!(backend.take_errors().is_empty());
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        preparation.join().unwrap();
        backend.send(MediaEvent::Stop { stream: 101 }).unwrap();
        backend.send(MediaEvent::Stop { stream: 100 }).unwrap();
        assert_eq!(backend.shared.queued_bytes.load(Ordering::Acquire), 0);
        // A new session must prepare again and can use the same stream ID.
        backend
            .send_with_audio_builder(config(100), |format| {
                audio_pipeline_with_capture(format, true)
            })
            .unwrap();
        backend.send(MediaEvent::Stop { stream: 100 }).unwrap();
        assert!(backend.take_errors().is_empty());
    }

    struct TestEncoder(gst::Pipeline);
    impl Drop for TestEncoder {
        fn drop(&mut self) {
            let _ = self.0.set_state(gst::State::Null);
        }
    }

    fn encoded_samples(description: &str) -> Vec<gst::Sample> {
        gst::init().unwrap();
        let encoder = TestEncoder(
            gst::parse::launch(description)
                .unwrap()
                .downcast::<gst::Pipeline>()
                .unwrap(),
        );
        let sink = encoder
            .0
            .by_name("encoded")
            .unwrap()
            .downcast::<gst_app::AppSink>()
            .unwrap();
        encoder.0.set_state(gst::State::Playing).unwrap();
        let mut samples = Vec::new();
        for _ in 0..4 {
            if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(5)) {
                samples.push(sample);
            } else {
                break;
            }
        }
        if samples.is_empty() {
            let error = encoder
                .0
                .bus()
                .unwrap()
                .iter()
                .find_map(|m| match m.view() {
                    gst::MessageView::Error(e) => Some(e.error().to_string()),
                    _ => None,
                });
            panic!("encoder produced no samples: {error:?}");
        }
        samples
    }

    fn video_roundtrip(codec: VideoCodec, encoder: &str) {
        use carplay_core::media::MediaSink;
        let description = format!(
            "videotestsrc pattern=red num-buffers=4 ! video/x-raw,format=I420,width=64,height=48,framerate=30/1 ! {encoder} ! appsink name=encoded sync=false"
        );
        let samples = encoded_samples(&description);
        let config = samples[0]
            .caps()
            .unwrap()
            .structure(0)
            .unwrap()
            .get::<gst::Buffer>("codec_data")
            .unwrap();
        let config = config.map_readable().unwrap().as_slice().to_vec();
        // Exercise the actual screen-description boundary as well as decode.
        // A following metadata box must never become part of codec_data.
        let mut description = ((config.len() + 8) as u32).to_be_bytes().to_vec();
        description.extend_from_slice(match codec {
            VideoCodec::H264 => b"avcC",
            VideoCodec::H265 => b"hvcC",
        });
        description.extend_from_slice(&config);
        description.extend_from_slice(&8u32.to_be_bytes());
        description.extend_from_slice(b"free");
        let (detected, extracted) = carplay_core::media::codec_config(&description).unwrap();
        assert_eq!(detected, codec);
        assert_eq!(extracted, config);
        let config = extracted;
        let mut packets = Vec::new();
        for sample in samples {
            let raw = sample.buffer().unwrap().map_readable().unwrap();
            packets.push(carplay_core::media::annex_b(raw.as_slice()).unwrap());
        }
        let sink = crate::GStreamerMediaSink::new().unwrap();
        let wait_frame = |previous: Option<&Arc<RgbaFrame>>| {
            let until = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(frame) = sink.latest_frame(7)
                    && previous.is_none_or(|old| !Arc::ptr_eq(old, &frame))
                {
                    assert_eq!(
                        (frame.width, frame.height, frame.rgba.len()),
                        (64, 48, 64 * 48 * 4)
                    );
                    let pixel = &frame.rgba[(24 * 64 + 32) * 4..][..4];
                    assert!(
                        pixel[0] > 200 && pixel[1] < 40 && pixel[2] < 40 && pixel[3] == 255,
                        "unexpected red frame pixel: {pixel:?}"
                    );
                    return frame;
                }
                let errors = sink.take_errors();
                assert!(errors.is_empty(), "decoder errors: {errors:?}");
                assert!(
                    std::time::Instant::now() < until,
                    "decoder produced no fresh RGBA frame"
                );
                thread::sleep(Duration::from_millis(10));
            }
        };
        for _ in 0..2 {
            sink.send(MediaEvent::VideoConfig {
                stream: 7,
                codec,
                data: config.clone(),
            })
            .unwrap();
            for packet in &packets {
                sink.send(MediaEvent::VideoFrame {
                    stream: 7,
                    data: packet.clone(),
                    sender_ns: 0,
                })
                .unwrap();
            }
            let first = wait_frame(None);
            sink.send(MediaEvent::VideoFrame {
                stream: 7,
                data: packets[0].clone(),
                sender_ns: 0,
            })
            .unwrap();
            let _next = wait_frame(Some(&first));
            sink.send(MediaEvent::Stop { stream: 7 }).unwrap();
            assert!(
                sink.latest_frame(7).is_none(),
                "Stop must finish clearing native/decoded buffers before returning"
            );
        }
    }

    #[test]
    #[ignore = "requires native GStreamer, x264 encoder and H.264 decoder"]
    fn native_h264_roundtrip_to_rgba() {
        video_roundtrip(
            VideoCodec::H264,
            "x264enc tune=zerolatency speed-preset=ultrafast ! video/x-h264,stream-format=avc,alignment=au",
        );
    }

    #[test]
    #[ignore = "requires native GStreamer, x265 encoder and H.265 decoder"]
    fn native_h265_roundtrip_to_rgba() {
        video_roundtrip(
            VideoCodec::H265,
            "x265enc tune=zerolatency speed-preset=ultrafast option-string=log-level=error:pools=1:frame-threads=1 ! h265parse ! video/x-h265,stream-format=hvc1,alignment=au",
        );
    }

    fn captured_audio(format: AudioFormat, payloads: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
        gst::init().unwrap();
        let pipeline = audio_pipeline_with_capture(format, true).unwrap();
        let sink = pipeline
            .pipeline
            .by_name("decoded")
            .unwrap()
            .downcast::<gst_app::AppSink>()
            .unwrap();
        for bytes in payloads {
            pipeline.push(bytes, false).unwrap();
        }
        let sample = sink.try_pull_sample(gst::ClockTime::from_seconds(5));
        let sample = sample
            .unwrap_or_else(|| panic!("audio decoder produced no samples: {:?}", pipeline.error()));
        let structure = sample.caps().unwrap().structure(0).unwrap();
        assert_eq!(structure.get::<&str>("format").unwrap(), "S16LE");
        assert_eq!(structure.get::<i32>("rate").unwrap(), format.rate as i32);
        assert_eq!(
            structure.get::<i32>("channels").unwrap(),
            format.channels as i32
        );
        sample
            .buffer()
            .unwrap()
            .map_readable()
            .unwrap()
            .as_slice()
            .to_vec()
    }

    #[test]
    #[ignore = "requires native GStreamer audio conversion"]
    fn native_pcm_converts_network_endian_without_sound() {
        let pcm = [0x01, 0x02, 0xfe, 0xff].repeat(256);
        let output = captured_audio(
            AudioFormat {
                codec: AudioCodec::Lpcm,
                rate: 48000,
                channels: 2,
            },
            [pcm],
        );
        assert_eq!(output, [0x02, 0x01, 0xff, 0xfe].repeat(256));
    }

    #[test]
    #[ignore = "requires native GStreamer AAC encoder and decoder"]
    fn native_aac_decodes_without_sound() {
        let samples = encoded_samples(
            "audiotestsrc wave=silence num-buffers=8 samplesperbuffer=1024 ! audio/x-raw,rate=48000,channels=2 ! audioconvert ! avenc_aac ! aacparse ! audio/mpeg,mpegversion=4,stream-format=raw ! appsink name=encoded sync=false",
        );
        let output = captured_audio(
            AudioFormat {
                codec: AudioCodec::AacLc,
                rate: 48000,
                channels: 2,
            },
            samples.into_iter().map(|s| {
                s.buffer()
                    .unwrap()
                    .map_readable()
                    .unwrap()
                    .as_slice()
                    .to_vec()
            }),
        );
        assert!(!output.is_empty());
        assert_eq!(output.len() % 4, 0);
    }

    #[test]
    #[ignore = "requires native GStreamer Opus encoder and decoder"]
    fn native_opus_decodes_without_sound() {
        let samples = encoded_samples(
            "audiotestsrc wave=silence num-buffers=8 samplesperbuffer=960 ! audio/x-raw,rate=48000,channels=1 ! audioconvert ! opusenc ! appsink name=encoded sync=false",
        );
        let output = captured_audio(
            AudioFormat {
                codec: AudioCodec::Opus,
                rate: 48000,
                channels: 1,
            },
            samples.into_iter().map(|s| {
                s.buffer()
                    .unwrap()
                    .map_readable()
                    .unwrap()
                    .as_slice()
                    .to_vec()
            }),
        );
        assert!(!output.is_empty());
        assert_eq!(output.len() % 2, 0);
    }
}
