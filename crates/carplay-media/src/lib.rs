// SPDX-License-Identifier: GPL-3.0-only
//! Decoded RGBA frames and native audio playback. Input is already decrypted.
//! Video configuration is avcC/hvcC; video frames are Annex B. Audio is raw AAC
//! access units, raw Opus packets, or interleaved signed 16-bit big-endian PCM.
use carplay_core::media::{MediaEvent, MediaSink};
use std::sync::Arc;

#[cfg(feature = "gstreamer")]
mod backend;
mod capture;
pub use capture::GStreamerCaptureFactory;
pub mod framing;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("GStreamer support is disabled; rebuild carplay-media with feature `gstreamer`")]
    FeatureDisabled,
    #[error("media initialization failed: {0}")]
    Initialization(String),
}

/// Packed RGBA8 rows, with no padding. Frames are shared with the UI via Arc;
/// the sink retains only the most recent frame per active video stream.
#[derive(Clone, Debug)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub pts_ns: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct PlaybackError {
    pub stream: Option<u16>,
    pub message: String,
}

/// A bounded worker owns active native pipelines. AudioConfig opens the output
/// device on its caller before transferring ownership to the worker. Drop stops
/// pipelines and joins the worker. Asynchronous failures are returned by later
/// send() calls and through take_errors(). Recreate after failure.
pub struct GStreamerMediaSink {
    #[cfg(feature = "gstreamer")]
    backend: backend::Backend,
}

impl GStreamerMediaSink {
    /// Called after publishing a decoded frame. Must be quick and non-blocking;
    /// use it to schedule a UI repaint, not to draw or access native devices.
    pub fn set_frame_ready_callback(&self, callback: Arc<dyn Fn() + Send + Sync>) {
        #[cfg(feature = "gstreamer")]
        self.backend.set_frame_ready_callback(callback);
        #[cfg(not(feature = "gstreamer"))]
        let _ = callback;
    }
    pub fn new() -> Result<Self, Error> {
        #[cfg(feature = "gstreamer")]
        {
            Ok(Self {
                backend: backend::Backend::new()?,
            })
        }
        #[cfg(not(feature = "gstreamer"))]
        {
            Err(Error::FeatureDisabled)
        }
    }

    pub fn latest_frame(&self, stream: u16) -> Option<Arc<RgbaFrame>> {
        #[cfg(feature = "gstreamer")]
        {
            self.backend.latest_frame(stream)
        }
        #[cfg(not(feature = "gstreamer"))]
        {
            let _ = stream;
            None
        }
    }

    pub fn latest_video_frame(&self) -> Option<Arc<RgbaFrame>> {
        #[cfg(feature = "gstreamer")]
        {
            self.backend.latest_video_frame()
        }
        #[cfg(not(feature = "gstreamer"))]
        {
            None
        }
    }

    pub fn take_errors(&self) -> Vec<PlaybackError> {
        #[cfg(feature = "gstreamer")]
        {
            self.backend.take_errors()
        }
        #[cfg(not(feature = "gstreamer"))]
        {
            Vec::new()
        }
    }
}

impl MediaSink for GStreamerMediaSink {
    fn send(&self, event: MediaEvent) -> Result<(), String> {
        #[cfg(feature = "gstreamer")]
        {
            self.backend.send(event)
        }
        #[cfg(not(feature = "gstreamer"))]
        {
            let _ = event;
            Err(Error::FeatureDisabled.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(not(feature = "gstreamer"))]
    fn disabled_backend_fails_explicitly() {
        assert!(matches!(
            super::GStreamerMediaSink::new(),
            Err(super::Error::FeatureDisabled)
        ));
    }
}
