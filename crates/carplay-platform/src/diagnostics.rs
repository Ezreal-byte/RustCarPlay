//! Reports contain no device names, USB serials, Bluetooth addresses, SSIDs,
//! credentials, environment values, raw device paths or subprocess output.
use serde::Serialize;
use std::{
    io,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticIssue {
    pub operation: &'static str,
    pub category: String,
    pub os_code: Option<i32>,
}
impl DiagnosticIssue {
    pub fn io(operation: &'static str, error: &io::Error) -> Self {
        Self {
            operation,
            category: format!("{:?}", error.kind()),
            os_code: error.raw_os_error(),
        }
    }
    pub fn unsupported(operation: &'static str) -> Self {
        Self {
            operation,
            category: "unsupported".into(),
            os_code: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostics {
    pub schema_version: u32,
    pub operating_system: &'static str,
    pub architecture: &'static str,
    pub network: crate::network::NetworkDiagnostic,
    pub bluetooth: crate::bluetooth::BluetoothDiagnostic,
    pub usb: crate::usb::UsbDiagnostic,
    pub gstreamer: GStreamerDiagnostic,
}

/// Read-only host snapshot. This can run on a worker thread; GStreamer subprocess
/// probes are bounded, while OS/USB enumeration follows OS driver timeouts.
pub fn collect_diagnostics() -> Diagnostics {
    Diagnostics {
        schema_version: 1,
        operating_system: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        network: crate::network::diagnose(),
        bluetooth: crate::bluetooth::diagnose(),
        usb: crate::usb::diagnose(),
        gstreamer: diagnose_gstreamer(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GStreamerElement {
    pub name: &'static str,
    pub available: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct GStreamerDiagnostic {
    pub inspector_available: bool,
    /// Capability probe, not a tested live decoding pipeline.
    pub h264_pipeline_elements_present: bool,
    pub h265_pipeline_elements_present: bool,
    pub audio_pipeline_elements_present: bool,
    pub elements: Vec<GStreamerElement>,
    pub issue: Option<DiagnosticIssue>,
}

pub fn diagnose_gstreamer() -> GStreamerDiagnostic {
    let mut result = GStreamerDiagnostic {
        inspector_available: false,
        h264_pipeline_elements_present: false,
        h265_pipeline_elements_present: false,
        audio_pipeline_elements_present: false,
        elements: Vec::new(),
        issue: None,
    };
    match inspect("--version") {
        Ok(true) => result.inspector_available = true,
        Ok(false) => {
            result.issue = Some(DiagnosticIssue {
                operation: "gst_inspect",
                category: "nonzero_exit".into(),
                os_code: None,
            });
            return result;
        }
        Err(error) => {
            result.issue = Some(DiagnosticIssue::io("gst_inspect", &error));
            return result;
        }
    }
    #[cfg(target_os = "windows")]
    let decoder = "d3d11h264dec";
    #[cfg(not(target_os = "windows"))]
    let decoder = "vah264dec";
    for name in [
        "appsrc",
        "appsink",
        "h264parse",
        "h265parse",
        "videoconvert",
        decoder,
        "avdec_h264",
        "avdec_h265",
        "aacparse",
        "decodebin",
        "audioconvert",
        "audioresample",
        "autoaudiosink",
        "opusdec",
        "opusenc",
        "avdec_aac",
    ] {
        let available = match inspect(name) {
            Ok(available) => available,
            Err(error) => {
                result.issue = Some(DiagnosticIssue::io("gst_element_probe", &error));
                false
            }
        };
        result.elements.push(GStreamerElement { name, available });
    }
    let has = |name| {
        result
            .elements
            .iter()
            .any(|e| e.name == name && e.available)
    };
    let video_common = has("appsrc") && has("appsink") && has("videoconvert");
    // Mirror carplay-media's explicit software decoder choice. Hardware factory
    // availability alone says nothing about successful CPU RGBA negotiation.
    result.h264_pipeline_elements_present = video_common && has("h264parse") && has("avdec_h264");
    result.h265_pipeline_elements_present = video_common && has("h265parse") && has("avdec_h265");
    result.audio_pipeline_elements_present = [
        "appsrc",
        "aacparse",
        "decodebin",
        "avdec_aac",
        "opusdec",
        "audioconvert",
        "audioresample",
        "autoaudiosink",
    ]
    .into_iter()
    .all(has);
    result
}

fn inspect(argument: &str) -> io::Result<bool> {
    // Rust's Windows executable lookup treats the `.0` suffix as an extension;
    // name the actual .exe instead of relying on PowerShell's PATHEXT lookup.
    let executable = if cfg!(target_os = "windows") {
        "gst-inspect-1.0.exe"
    } else {
        "gst-inspect-1.0"
    };
    let mut command = Command::new(executable);
    command
        .arg(argument)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn errors_do_not_export_private_os_error_messages() {
        let error = io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Alice iPhone serial ABC password secret",
        );
        let exported = serde_json::to_string(&DiagnosticIssue::io("usb_open", &error)).unwrap();
        assert!(exported.contains("PermissionDenied"));
        for secret in ["Alice", "ABC", "password", "secret"] {
            assert!(!exported.contains(secret));
        }
    }
}
