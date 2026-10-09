# carplay-media

`GStreamerMediaSink` implements the core `MediaSink` contract. Enable Cargo
feature `gstreamer` and provide a GStreamer 1.20+ SDK/runtime with app, playback,
video conversion, audio conversion/resampling, H.264/H.265 parsers and the
gst-libav video/AAC decoder plugins and Opus.

The native backend uses appsrc, parsers and gst-libav software video decoders
with slice threading. Selecting hardware decoders is a later, explicit option:
the initial smoke tests found a local D3D12 H.265 driver failure under decodebin's
automatic selection. Video leaves an
appsink as packed RGBA8; `latest_frame(stream)` / `latest_video_frame()` returns
an `Arc<RgbaFrame>` suitable for an egui texture. Audio reaches autoaudiosink.
The default build explicitly rejects construction because native playback has
not been enabled; it never reports simulated playback success.

Input is decrypted and depacketized by the receiver: raw avcC/hvcC configuration,
Annex B video access units, raw AAC-LC access units, raw Opus packets, or signed
16-bit big-endian interleaved PCM (as in DiPlay's AndroidMediaSink). AAC transport
headers are not guessed or silently removed. PCM endian conversion is performed
by GStreamer's audioconvert. Unsupported framing is an observable error.

The ingress queue has 64 events and a 16 MiB byte cap; each native appsrc has an
8 MiB admission cap. There are at most eight pipelines, one latest decoded frame
per video stream, and 32 retained error messages. Input pressure returns an error
instead of silently dropping reference frames. Native errors are returned by
subsequent `send` calls and available through `take_errors`; recreate the sink
after a failure. Drop sets all pipelines to Null and joins the worker.
`Stop` waits for queue admission and completed teardown even when the data queue
is full; call it from the receiver/lifecycle worker, as the application does.
It returns after native buffers and the latest decoded frame are cleared.

`AudioConfig` is also a completion barrier. The receiver sends it before replying
to audio SETUP. Native output-device initialization runs on the SETUP caller,
then transfers the prepared pipeline to the media worker for installation and
acknowledgement. It never stalls existing streams on that worker. Audio packets
require an already prepared matching format; opening the device on the first
PCM packet would overflow the shared ingress queue during Windows device startup.

This first backend timestamps at arrival. It does not yet discipline playback
against the negotiated AirPlay timing clock, align separately started audio and
video pipelines or mix/duck streams according to
CarPlay audio priority. Sender timestamps remain in the core event contract for
that next stage. Runtime codecs/devices and actual iPhone playback need hardware
validation; a successful default build alone is not evidence of native playback.

Validation: `cargo test -p carplay-media`; native compile and tests:
`cargo test -p carplay-media --features gstreamer`. On Windows, configure the
GStreamer SDK's `bin` in PATH and its `lib/pkgconfig` in PKG_CONFIG_PATH. On Linux,
install the equivalent GStreamer development packages and decoder plugins.

Five opt-in native smoke tests generate actual H.264/H.265 video and AAC/Opus
audio, decode them through the backend and inspect RGBA/PCM output. They also
check big-endian LPCM conversion. Audio is captured in a test-only appsink and
does not open a speaker. Run with `-- --include-ignored`; x264enc, x265enc,
avenc_aac, opusenc, and the normal decoder plugins are required. These synthetic
round trips validate local codecs, not iPhone interoperability or lip sync.
An additional native regression delays a second audio device initialization
while sending 140 five-millisecond PCM packets through an existing stream. It
checks that setup waits, playback keeps draining, and teardown permits a fresh
stream with the same ID. It uses appsink and does not open a speaker.

`GStreamerCaptureFactory::new()` initializes/probes GStreamer without opening a
microphone. Only its `CaptureFactory::start` call constructs and starts
autoaudiosrc; the receiver must gate that call on explicit microphone enablement
and an authenticated session. Capture supplies fixed-size mono S16BE packets or
48 kHz mono Opus packets of 20 ms to the receiver's bounded `CaptureSink`, which
owns RTP/encryption/network I/O. Source buffering is finite; source/encoder/sink
errors are reported, and stop/Drop releases the source and joins the worker.
Two additional native smoke tests use `GStreamerCaptureFactory::synthetic()`
with audiotestsrc, validate multiple PCM/Opus packets and stop/Drop behavior, and
never access a microphone. Actual input-device permissions and quality still
require the user's enabled real microphone session.
