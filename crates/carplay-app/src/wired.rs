// SPDX-License-Identifier: GPL-3.0-only
//! USB must never silently fall back to a Bluetooth or Wi-Fi session.
use super::*;

impl Connection {
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    pub(super) fn start_usb(_: ConnectionOptions, _: Arc<AtomicBool>) -> Result<Self> {
        Err(carplay_platform::PlatformError::Unsupported(
            "USB CarPlay is currently implemented on Windows and Linux only",
        )
        .into())
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub(super) fn start_usb(options: ConnectionOptions, cancel: Arc<AtomicBool>) -> Result<Self> {
        let TransportOptions::Usb { device_id } = options.transport else {
            anyhow::bail!("USB transport required")
        };
        ensure!(!cancel.load(Ordering::Acquire), "连接已取消");
        let auth =
            Arc::new(LocalIdentity::load(&options.auth_dir).context("load accessory identity")?);
        let (identity, _) = carplay_receiver::storage::FilePairingStore::open(&options.state_dir)?;
        let media = Arc::new(GStreamerMediaSink::new()?);
        let capture = if options.config.microphone {
            Some(Arc::new(GStreamerCaptureFactory::new()?)
                as Arc<dyn carplay_core::media::CaptureFactory>)
        } else {
            None
        };
        let prepared = carplay_platform::usb::system::prepare_system(
            device_id.as_deref(),
            &cancel,
            Duration::from_secs(30),
        )
        .context("准备 USBMUX / Lockdown / NCM 有线通路")?;
        let identifier = prepared
            .mac
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":");
        let identification = Identification {
            name: options.config.name.clone(),
            model: "RustCarPlay".into(),
            manufacturer: "RustCarPlay".into(),
            serial: identity.pairing_id.clone(),
            firmware_version: env!("CARGO_PKG_VERSION").into(),
            hardware_version: "1".into(),
            language: "en".into(),
            external_accessory_protocol: "org.rustcarplay.receiver".into(),
            transport: IdentificationTransport::Wired {
                usb_interface: prepared.usb_interface,
            },
            sent_messages: carplay_wireless::SENT_MESSAGES
                .iter()
                .copied()
                .filter(|id| *id != 0x5703)
                .chain(std::iter::once(0xae03))
                .collect(),
            received_messages: carplay_wireless::RECEIVED_MESSAGES
                .iter()
                .copied()
                .filter(|id| ![0x5702, 0x4e0d, 0x4e0e].contains(id))
                .collect(),
            extra_components: Vec::new(),
        };
        let mut endpoint = Endpoint {
            transport: carplay_wireless::EndpointTransport::Wired,
            ssid: String::new(),
            passphrase: Zeroizing::new(String::new()),
            channel: 0,
            security_type: 0,
            ip_addresses: vec![prepared.bind.ip().to_string()],
            airplay_port: prepared.bind.port(),
            device_identifier: identifier.clone(),
            public_key: identity
                .public_key()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            source_version: carplay_core::SOURCE_VERSION.into(),
            access_point_bssid: None,
        };
        let receiver = carplay_receiver::start_with_runtime(
            ReceiverOptions {
                bind: SocketAddr::V6(prepared.bind),
                config: options.config,
                bluetooth_address: identifier,
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
        let coordinator = Coordinator::new(endpoint, identification, auth, Default::default())?;
        let live = Arc::new(AtomicBool::new(false));
        let active = live.clone();
        let stopped = cancel.clone();
        let (tx, updates) = mpsc::sync_channel(128);
        let wireless = thread::Builder::new()
            .name("carplay-usb-iap2".into())
            .spawn(move || {
                let _ = tx.try_send(AppEvent::Status(
                    "USB CarKit 服务已建立，正在认证 iPhone".into(),
                ));
                let outcome = carplay_wireless::run(
                    prepared.stream,
                    coordinator,
                    &stopped,
                    || active.load(Ordering::Acquire),
                    |event| {
                        let update = match &event {
                            carplay_wireless::Event::Incoming(message) => metadata::decode(message)
                                .ok()
                                .flatten()
                                .map(AppEvent::Metadata),
                            _ => None,
                        };
                        let _ = tx.try_send(
                            update.unwrap_or_else(|| AppEvent::Status(format!("USB: {event:?}"))),
                        );
                        Action::Continue
                    },
                );
                match outcome {
                    Err(error) => {
                        let _ = tx.try_send(AppEvent::Error(format!("USB iAP2: {error}")));
                    }
                    Ok(outcome) if outcome.terminal != carplay_wireless::Terminal::Cancelled => {
                        // Some phones close CarKit after handing media to NCM. Give
                        // the authenticated AirPlay first frame time to reach poll().
                        let deadline = Instant::now() + Duration::from_secs(10);
                        if outcome.progress.authenticated
                            && outcome.progress.start_sessions_sent > 0
                        {
                            while !active.load(Ordering::Acquire)
                                && !stopped.load(Ordering::Acquire)
                                && Instant::now() < deadline
                            {
                                wait_for_stop(&stopped, Duration::from_millis(100));
                            }
                        }
                        while active.load(Ordering::Acquire) && !stopped.load(Ordering::Acquire) {
                            wait_for_stop(&stopped, Duration::from_millis(100));
                        }
                        if !stopped.load(Ordering::Acquire) {
                            let _ = tx.try_send(AppEvent::Error(format!(
                                "USB 会话已结束（{:?}），请检查数据线后断开并重新连接",
                                outcome.terminal
                            )));
                        }
                    }
                    _ => {}
                }
            })?;
        Ok(Self {
            media,
            receiver,
            discovery: None,
            wireless: Some(wireless),
            updates,
            cancel,
            live,
            proof: FrameProof::default(),
            handoff: BootstrapHandoff::default(),
            release_bootstrap: Arc::new(AtomicBool::new(false)),
            hotspot: None,
        })
    }
}
