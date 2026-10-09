// SPDX-License-Identifier: GPL-3.0-only
//! Shared application lifecycle for the CLI and desktop shell.
mod wired;
use anyhow::{Context, Result, ensure};
use carplay_auth::LocalIdentity;
use carplay_core::config::ReceiverConfig;
use carplay_media::{GStreamerCaptureFactory, GStreamerMediaSink, RgbaFrame};

use carplay_platform::bluetooth::{BluetoothAddress, ConnectOptions, RfcommStream};
use carplay_protocol::{
    metadata,
    tlv::{ControlMessage, Identification, IdentificationTransport},
};
use carplay_receiver::{ReceiverEvent, ReceiverHandle, ReceiverOptions};
use carplay_wireless::{Action, Coordinator, Endpoint, IAP2_IPHONE_SERVICE_UUID};
use std::{
    fmt,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

/// The portable launcher selects its bundled accessory identity before startup.
/// Source builds keep the existing local development directory.
pub fn default_auth_dir() -> PathBuf {
    std::env::var_os("RUSTCARPLAY_AUTH_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".local/auth"))
}

/// Connection choice is persisted; credentials are never part of the saved profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionMode {
    #[default]
    Lan,
    Hotspot,
    Usb,
}

#[derive(Clone, Debug)]
pub struct WirelessDevice {
    pub local_bluetooth: BluetoothAddress,
    pub iphone: BluetoothAddress,
    pub iphone_ip: Option<std::net::IpAddr>,
    pub rfcomm_channel: Option<u8>,
}

/// Each transport carries only the options that actually apply to it.
pub enum TransportOptions {
    Lan {
        bind: SocketAddr,
        device: WirelessDevice,
    },
    Hotspot {
        device: WirelessDevice,
        port: u16,
        /// None uses the operating system's current hotspot configuration.
        config: Option<carplay_platform::hotspot::HotspotConfig>,
    },
    Usb {
        device_id: Option<String>,
    },
}

pub struct ConnectionOptions {
    pub config: ReceiverConfig,
    pub transport: TransportOptions,
    pub auth_dir: PathBuf,
    pub state_dir: PathBuf,
}

struct PreparedWireless {
    bind: SocketAddr,
    device: WirelessDevice,
    ssid: String,
    passphrase: Zeroizing<String>,
    security_type: u8,
    hotspot: Option<carplay_platform::hotspot::HotspotSession>,
}

fn prepare_wireless(transport: TransportOptions, cancel: &AtomicBool) -> Result<PreparedWireless> {
    ensure!(!cancel.load(Ordering::Acquire), "连接已取消");
    ensure!(
        cfg!(any(target_os = "windows", target_os = "linux")),
        carplay_platform::PlatformError::Unsupported(
            "wireless CarPlay is currently implemented on Windows and Linux only"
        )
    );
    match transport {
        TransportOptions::Lan { bind, device } => {
            ensure!(
                !bind.ip().is_loopback() && !bind.ip().is_unspecified(),
                "请选择当前 Wi-Fi 的实际本机地址"
            );
            let credentials = carplay_platform::wifi::credentials_for_address(bind.ip()).context(
                "无法取得系统当前 Wi-Fi 配置；请先在系统中连接 Wi-Fi，软件无需手动输入密码",
            )?;
            ensure!(!cancel.load(Ordering::Acquire), "连接已取消");
            Ok(PreparedWireless {
                bind,
                device,
                ssid: credentials.network.ssid,
                passphrase: credentials.passphrase,
                security_type: credentials.security_type,
                hotspot: None,
            })
        }
        TransportOptions::Hotspot {
            mut device,
            port,
            config,
        } => {
            ensure!(port > 0, "热点接收端口不能为零");
            // An address from an earlier LAN session must never select a peer
            // on the newly created hotspot subnet, including through the CLI.
            device.iphone_ip = None;
            let hotspot = match config {
                Some(config) => carplay_platform::hotspot::HotspotSession::start(&config, cancel),
                None => carplay_platform::hotspot::HotspotSession::start_default(cancel),
            }
            .context("无法启动电脑热点")?;
            let endpoint = hotspot.endpoint();
            Ok(PreparedWireless {
                bind: SocketAddr::new(endpoint.address.into(), port),
                device,
                ssid: endpoint.ssid.clone(),
                passphrase: endpoint.passphrase.clone(),
                security_type: endpoint.security_type,
                hotspot: Some(hotspot),
            })
        }
        TransportOptions::Usb { device_id } => {
            let _ = device_id;
            anyhow::bail!("USB 必须使用独立有线通路，不可回退到无线连接")
        }
    }
}

#[derive(Clone)]
pub enum AppEvent {
    BootstrapStarted { attempt: u32 },
    PhoneSelectionRequired,
    Status(String),
    Receiver(ReceiverEvent),
    Metadata(metadata::Update),
    Error(String),
}
impl fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PhoneSelectionRequired => f.write_str("PhoneSelectionRequired"),
            Self::BootstrapStarted { attempt } => f
                .debug_struct("BootstrapStarted")
                .field("attempt", attempt)
                .finish(),
            Self::Status(s) => f.debug_tuple("Status").field(s).finish(),
            Self::Receiver(e) => e.fmt(f),
            Self::Error(e) => f.debug_tuple("Error").field(e).finish(),
            Self::Metadata(update) => f
                .debug_tuple("Metadata")
                .field(&match update {
                    metadata::Update::NowPlaying(_) => "now_playing",
                    metadata::Update::Route(_) => "route",
                    metadata::Update::Maneuver(_) => "maneuver",
                    metadata::Update::Call(_) => "call",
                })
                .finish(),
        }
    }
}

/// A cached decoded image is insufficient proof of a currently authenticated session.
#[derive(Default)]
struct FrameProof {
    verified: bool,
    wire_frame: bool,
    baseline: Option<Arc<RgbaFrame>>,
}
impl FrameProof {
    fn observe(&mut self, event: &ReceiverEvent, frame: Option<Arc<RgbaFrame>>) {
        match event {
            ReceiverEvent::TcpAccepted | ReceiverEvent::Disconnected | ReceiverEvent::Error(_) => {
                self.verified = false;
                self.wire_frame = false;
                self.baseline = frame;
            }
            ReceiverEvent::Verified => {
                self.verified = true;
                self.wire_frame = false;
                self.baseline = frame;
            }
            ReceiverEvent::FirstVideoFrame(110) if self.verified => {
                self.wire_frame = true;
            }
            _ => {}
        }
    }
    fn has_current_frame(&self, frame: Option<&Arc<RgbaFrame>>) -> bool {
        self.verified
            && self.wire_frame
            && frame.is_some_and(|frame| {
                self.baseline
                    .as_ref()
                    .is_none_or(|old| !Arc::ptr_eq(old, frame))
            })
    }
}

pub struct Connection {
    pub media: Arc<GStreamerMediaSink>,
    receiver: ReceiverHandle,
    discovery: Option<carplay_discovery::Handle>,
    wireless: Option<JoinHandle<()>>,
    updates: Receiver<AppEvent>,
    cancel: Arc<AtomicBool>,
    live: Arc<AtomicBool>,
    proof: FrameProof,
    hotspot: Option<carplay_platform::hotspot::HotspotSession>,
}

impl Connection {
    pub fn start(options: ConnectionOptions) -> Result<Self> {
        Self::start_with_cancel(options, Arc::new(AtomicBool::new(false)))
    }

    /// Network preparation and receiver startup run on the caller's worker thread.
    pub fn start_with_cancel(options: ConnectionOptions, cancel: Arc<AtomicBool>) -> Result<Self> {
        options.config.validate().map_err(anyhow::Error::msg)?;
        if matches!(&options.transport, TransportOptions::Usb { .. }) {
            return Self::start_usb(options, cancel);
        }
        let PreparedWireless {
            bind,
            device,
            ssid,
            passphrase,
            security_type,
            hotspot,
        } = prepare_wireless(options.transport, &cancel)?;
        let auth =
            Arc::new(LocalIdentity::load(&options.auth_dir).context("load accessory identity")?);
        let (identity, _) = carplay_receiver::storage::FilePairingStore::open(&options.state_dir)?;
        let media = Arc::new(GStreamerMediaSink::new()?);
        let identification = Identification {
            name: options.config.name.clone(),
            model: "RustCarPlay".into(),
            manufacturer: "RustCarPlay".into(),
            serial: identity.pairing_id.clone(),
            firmware_version: env!("CARGO_PKG_VERSION").into(),
            hardware_version: "1".into(),
            language: "en".into(),
            external_accessory_protocol: "org.rustcarplay.receiver".into(),
            transport: IdentificationTransport::Wireless {
                bluetooth_mac: device.local_bluetooth.octets(),
                ssid: ssid.clone(),
            },
            sent_messages: carplay_wireless::SENT_MESSAGES.to_vec(),
            received_messages: carplay_wireless::RECEIVED_MESSAGES.to_vec(),
            extra_components: Vec::new(),
        };
        let mut endpoint = Endpoint {
            transport: carplay_wireless::EndpointTransport::Wireless,
            ssid,
            security_type,
            passphrase,
            channel: 0,
            ip_addresses: vec![bind.ip().to_string()],
            airplay_port: bind.port().max(1),
            device_identifier: device.local_bluetooth.to_colon_string(),
            public_key: identity
                .public_key()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            source_version: carplay_core::SOURCE_VERSION.into(),
            access_point_bssid: None,
        };
        let _ = Coordinator::new(
            endpoint.clone(),
            identification.clone(),
            auth.clone(),
            Default::default(),
        )?;
        let capture = if options.config.microphone {
            Some(Arc::new(GStreamerCaptureFactory::new()?)
                as Arc<dyn carplay_core::media::CaptureFactory>)
        } else {
            None
        };
        let receiver = carplay_receiver::start_with_runtime(
            ReceiverOptions {
                bind,
                config: options.config.clone(),
                bluetooth_address: device.local_bluetooth.to_colon_string(),
                state_dir: options.state_dir,
                advertise: true,
            },
            auth.clone(),
            media.clone(),
            carplay_receiver::RuntimeOptions {
                identification: Some(identification.clone()),
                endpoint: Some(endpoint.clone()),
                capture,
            },
        )?;
        endpoint.airplay_port = receiver.address.port();
        let mut discovery_options = carplay_discovery::Options::new(
            receiver.address,
            device.local_bluetooth.octets(),
            carplay_core::SOURCE_VERSION,
            device.iphone.octets(),
        );
        discovery_options.target_ip = device.iphone_ip;
        let discovery = carplay_discovery::start(discovery_options)?;
        let stopped = cancel.clone();
        let live = Arc::new(AtomicBool::new(false));
        let active = live.clone();
        let (tx, updates) = mpsc::sync_channel(128);
        let wireless = thread::Builder::new()
            .name("carplay-bluetooth".into())
            .spawn(move || {
                let mut attempt = 0u32;
                while !stopped.load(Ordering::Acquire) {
                    let _ = tx.try_send(AppEvent::BootstrapStarted { attempt });
                    let run = || -> Result<()> {
                        let coordinator = Coordinator::new(
                            endpoint.clone(),
                            identification.clone(),
                            auth.clone(),
                            Default::default(),
                        )?;
                        let stream = RfcommStream::connect(ConnectOptions {
                            peer: device.iphone,
                            service: IAP2_IPHONE_SERVICE_UUID.parse()?,
                            channel: device.rfcomm_channel,
                            timeout: Duration::from_secs(15),
                        })?;
                        let result = carplay_wireless::run(
                            stream,
                            coordinator,
                            &stopped,
                            || active.load(Ordering::Acquire),
                            |event| {
                                let update = match &event {
                                    carplay_wireless::Event::Incoming(message) => {
                                        metadata::decode(message)
                                            .ok()
                                            .flatten()
                                            .map(AppEvent::Metadata)
                                    }
                                    _ => None,
                                };
                                let _ = tx.try_send(
                                    update
                                        .unwrap_or_else(|| AppEvent::Status(format!("{event:?}"))),
                                );
                                Action::Continue
                            },
                        )?;
                        if result.terminal != carplay_wireless::Terminal::Cancelled {
                            let _ = tx.try_send(AppEvent::Status(format!(
                                "Bluetooth bootstrap ended: {:?}",
                                result.terminal
                            )));
                        }
                        Ok(())
                    };
                    if let Err(e) = run() {
                        let permanent =
                            e.downcast_ref::<carplay_wireless::Error>()
                                .is_some_and(|e| {
                                    matches!(
                                        e,
                                        carplay_wireless::Error::AuthenticationRejected
                                            | carplay_wireless::Error::IdentificationRejected { .. }
                                            | carplay_wireless::Error::Configuration(_)
                                    )
                                });
                        let _ = tx.try_send(AppEvent::Error(e.to_string()));
                        if permanent {
                            break;
                        }
                    }
                    // The phone may close bootstrap RFCOMM after handing off to its live LAN session.
                    while active.load(Ordering::Acquire) && !stopped.load(Ordering::Acquire) {
                        attempt = 0;
                        wait_for_stop(&stopped, Duration::from_millis(100));
                    }
                    if !options.config.reconnect || stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let delay = Duration::from_millis((500u64 << attempt.min(6)).min(30_000));
                    attempt = attempt.saturating_add(1);
                    let _ = tx.try_send(AppEvent::Status(format!(
                        "Retrying Bluetooth in {} ms",
                        delay.as_millis()
                    )));
                    wait_for_stop(&stopped, delay);
                }
            })?;
        Ok(Self {
            media,
            receiver,
            discovery: Some(discovery),
            wireless: Some(wireless),
            updates,
            cancel,
            live,
            proof: FrameProof::default(),
            hotspot,
        })
    }
    pub fn poll(&mut self) -> Vec<AppEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.receiver.events.try_recv() {
            self.proof.observe(&event, self.media.latest_frame(110));
            if let ReceiverEvent::IapMessage { message_id, body } = &event {
                match metadata::decode(&ControlMessage {
                    message_id: *message_id,
                    body: body.clone(),
                }) {
                    Ok(Some(update)) => events.push(AppEvent::Metadata(update)),
                    Err(e) => events.push(AppEvent::Error(format!("Invalid iAP2 metadata: {e}"))),
                    _ => {}
                }
            } else {
                events.push(AppEvent::Receiver(event));
            }
        }
        self.live.store(
            self.proof
                .has_current_frame(self.media.latest_frame(110).as_ref()),
            Ordering::Release,
        );
        for event in self.updates.try_iter() {
            if matches!(event,AppEvent::BootstrapStarted{attempt} if attempt > 0)
                && let Some(discovery) = &self.discovery
            {
                discovery.retry();
            }
            events.push(event);
        }
        for event in self.discovery.iter().flat_map(|d| d.events.try_iter()) {
            if !self.proof.verified
                && matches!(
                    event,
                    carplay_discovery::Event::PhoneSelectionRequired { .. }
                )
            {
                events.push(AppEvent::PhoneSelectionRequired);
            }
            events.push(AppEvent::Status(format!("Discovery: {event:?}")));
        }
        for e in self.media.take_errors() {
            self.live.store(false, Ordering::Release);
            // A fatal decoder error terminates its worker; do not retry with that dead sink.
            self.cancel.store(true, Ordering::Release);
            events.push(AppEvent::Error(e.message));
        }
        events
    }
    pub fn hid(&self, uid: u32, data: Vec<u8>) -> bool {
        self.receiver.hid(uid, data)
    }
    pub fn night(&self, value: bool) -> bool {
        self.receiver.night(value)
    }
    pub fn is_finished(&self) -> bool {
        !self.live.load(Ordering::Acquire)
            && self.wireless.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub fn stop(&mut self) {
        let _ = self.stop_checked();
    }
    pub fn stop_checked(&mut self) -> Result<()> {
        self.cancel.store(true, Ordering::Release);
        self.live.store(false, Ordering::Release);
        if let Some(discovery) = &mut self.discovery {
            discovery.stop();
        }
        self.receiver.stop();
        let worker_ok = self.wireless.take().is_none_or(|t| t.join().is_ok());
        if let Some(mut hotspot) = self.hotspot.take() {
            hotspot
                .stop()
                .context("接收端已停止，但系统热点清理失败，请检查系统移动热点设置")?;
        }
        ensure!(worker_ok, "连接后台线程异常退出");
        Ok(())
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn load_config(path: Option<&std::path::Path>) -> Result<ReceiverConfig> {
    let config = match path {
        Some(p) => {
            let bytes = std::fs::read(p)?;
            ensure!(bytes.len() <= 65536, "configuration is too large");
            serde_json::from_slice(&bytes)?
        }
        None => ReceiverConfig::default(),
    };
    config.validate().map_err(anyhow::Error::msg)?;
    Ok(config)
}

pub fn wait_for_stop(cancel: &AtomicBool, duration: Duration) {
    let until = Instant::now() + duration;
    while !cancel.load(Ordering::Acquire) && Instant::now() < until {
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame() -> Arc<RgbaFrame> {
        Arc::new(RgbaFrame {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
            pts_ns: None,
        })
    }
    #[test]
    fn stale_frame_does_not_keep_disconnected_or_new_session_alive() {
        let old = frame();
        let new = frame();
        let mut proof = FrameProof::default();
        proof.observe(&ReceiverEvent::Verified, Some(old.clone()));
        assert!(!proof.has_current_frame(Some(&new)));
        proof.observe(&ReceiverEvent::FirstVideoFrame(110), Some(old.clone()));
        assert!(!proof.has_current_frame(Some(&old)));
        assert!(proof.has_current_frame(Some(&new)));
        proof.observe(&ReceiverEvent::Disconnected, Some(new.clone()));
        assert!(!proof.has_current_frame(Some(&new)));
        proof.observe(&ReceiverEvent::Verified, Some(new.clone()));
        assert!(!proof.has_current_frame(Some(&new)));
    }
    #[test]
    fn metadata_diagnostics_do_not_include_phone_content() {
        let update = metadata::Update::NowPlaying(metadata::NowPlaying {
            item: Some(metadata::MediaItem {
                title: Some("private song".into()),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(
            format!("{:?}", AppEvent::Metadata(update)),
            "Metadata(\"now_playing\")"
        );
    }
}
