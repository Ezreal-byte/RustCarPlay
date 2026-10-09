// SPDX-License-Identifier: GPL-3.0-only
//! Exercise the actual desktop event/frame handlers without devices or filesystem I/O.

use super::*;
use carplay_media::RgbaFrame;
use carplay_receiver::ReceiverEvent;

fn desktop(ctx: &egui::Context) -> Desktop {
    let logo = ctx.load_texture(
        "test-logo",
        egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 180, 90, 255]),
        egui::TextureOptions::LINEAR,
    );
    Desktop {
        config: ReceiverConfig::default(),
        bind: String::new(),
        local_bt: String::new(),
        iphone: String::new(),
        iphone_ip: String::new(),
        mode: ConnectionMode::Lan,
        hotspot_custom: false,
        hotspot_ssid: String::new(),
        hotspot_password: zeroize::Zeroizing::new(String::new()),
        hotspot_status: None,
        usb_device: String::new(),
        #[cfg(target_os = "windows")]
        usb_resume: None,
        startup_cancel: None,
        auth_dir: String::new(),
        status: String::new(),
        logs: VecDeque::new(),
        diagnostics: None,
        job: None,
        pending: None,
        connection: None,
        closing: None,
        texture: None,
        render_state: None,
        last_frame: None,
        navigation: Navigation::default(),
        logo,
        diagnostics_open: false,
        vehicle_open: false,
        night: false,
        fullscreen: false,
        suppress_touch: false,
        video_session_ready: false,
        screenshot: None,
        screenshot_requested: false,
        touch: touch::TouchInput::default(),
        created: Instant::now(),
        smoke: false,
        hosts: None,
        journal: None,
    }
}

fn frame(pts_ns: u64) -> Arc<RgbaFrame> {
    Arc::new(RgbaFrame {
        width: 2,
        height: 1,
        rgba: vec![0, 0, 0, 255, 20, 30, 40, 255],
        pts_ns: Some(pts_ns),
    })
}

fn receiver(app: &mut Desktop, event: ReceiverEvent) {
    app.handle_event(AppEvent::Receiver(event));
}

#[cfg(target_os = "windows")]
#[test]
fn preparation_progress_keeps_worker_channel_until_final_result() {
    let ctx = egui::Context::default();
    let mut app = desktop(&ctx);
    let (tx, rx) = mpsc::channel();
    app.job = Some(rx);
    tx.send(JobResult::UsbProgress("checking".into())).unwrap();
    tx.send(JobResult::UsbProgress("activating".into()))
        .unwrap();
    app.poll(&ctx);
    assert_eq!(app.status, "checking");
    assert!(app.job.is_some());
    app.poll(&ctx);
    assert_eq!(app.status, "activating");
    assert!(app.job.is_some());
    // Use an in-memory terminal result to avoid real device enumeration.
    tx.send(JobResult::Diagnostics("finished".into())).unwrap();
    app.poll(&ctx);
    assert!(app.job.is_none());
}

#[cfg(target_os = "windows")]
#[test]
fn usb_preparation_gates_connection_even_after_device_refresh() {
    use carplay_platform::usb::{Readiness, UsbPhone};
    let ctx = egui::Context::default();
    let mut app = desktop(&ctx);
    app.mode = ConnectionMode::Usb;
    app.usb_device = "port-a".into();
    app.hosts = Some(HostChoices {
        adapters: vec![],
        peers: vec![],
        addresses: vec![],
        wifi: None,
        wifi_address: None,
        usb: Ok(vec![UsbPhone {
            id: "port-a".into(),
            windows_device_token: None,
            name: "iPhone".into(),
            product_id: 0,
            active_configuration: None,
            configuration: None,
            readiness: Readiness::ModeSwitchRequired,
        }]),
    });
    for readiness in [
        Readiness::ModeSwitchRequired,
        Readiness::DriverSetupRequired("inactive".into()),
    ] {
        app.hosts.as_mut().unwrap().usb.as_mut().unwrap()[0].readiness = readiness;
        assert!(app.usb_connection_blocker().is_some());
        app.start();
        assert!(
            app.pending.is_none(),
            "must not switch phone mode while preparation is incomplete"
        );
    }
    app.hosts.as_mut().unwrap().usb.as_mut().unwrap()[0].readiness = Readiness::InterfacesAvailable;
    app.usb_resume = Some("port-a".into());
    assert!(app.usb_connection_blocker().is_some());
    app.usb_resume = None;
    assert!(app.usb_connection_blocker().is_none());
    app.usb_resume = Some("port-a".into());
    app.mode = ConnectionMode::Lan;
    assert!(
        app.usb_connection_blocker().is_none(),
        "USB preparation does not block LAN"
    );
}

#[test]
fn oem_tile_opens_placeholder_without_losing_video_or_reopening_player() {
    let ctx = egui::Context::default();
    let mut app = desktop(&ctx);
    receiver(&mut app, ReceiverEvent::TcpAccepted);
    receiver(&mut app, ReceiverEvent::Verified);
    app.display_frame(&ctx, frame(1));
    receiver(&mut app, ReceiverEvent::UiRequested("vehicle:".into()));
    assert!(app.vehicle_open);
    assert_eq!(app.navigation.page(), Page::Home);
    assert!(app.texture.is_some());
    app.display_frame(&ctx, frame(2));
    assert_eq!(app.navigation.page(), Page::Home);
    receiver(&mut app, ReceiverEvent::Disconnected);
    assert!(!app.vehicle_open);
}

#[test]
fn malformed_decoded_frame_does_not_replace_last_valid_texture() {
    let ctx = egui::Context::default();
    let mut app = desktop(&ctx);
    receiver(&mut app, ReceiverEvent::Verified);
    app.display_frame(&ctx, frame(1));
    let previous = app.last_frame.clone().unwrap();
    app.display_frame(
        &ctx,
        Arc::new(RgbaFrame {
            width: u32::MAX,
            height: u32::MAX,
            rgba: vec![],
            pts_ns: None,
        }),
    );
    assert!(Arc::ptr_eq(app.last_frame.as_ref().unwrap(), &previous));
    assert!(app.texture.is_some());
}

fn select_wireless_devices(app: &mut Desktop) {
    app.local_bt = "02:00:00:00:00:01".into();
    app.iphone = "02:00:00:00:00:02".into();
}

#[test]
fn lan_options_need_no_ssid_or_password_fields() {
    let ctx = egui::Context::default();
    let mut app = desktop(&ctx);
    select_wireless_devices(&mut app);
    app.bind = "192.0.2.10:7000".into();
    // Even stale invalid hotspot fields must have no effect on LAN connection.
    app.hotspot_custom = true;
    app.hotspot_ssid.clear();
    app.hotspot_password = zeroize::Zeroizing::new("x".into());
    let options = app.connection_options().unwrap();
    let TransportOptions::Lan { bind, device } = options.transport else {
        panic!("LAN options must retain their explicit transport");
    };
    assert_eq!(bind, "192.0.2.10:7000".parse().unwrap());
    assert!(device.iphone_ip.is_none());
}

#[test]
fn usb_options_never_validate_or_require_wireless_fields() {
    let ctx = egui::Context::default();
    let mut app = desktop(&ctx);
    app.mode = ConnectionMode::Usb;
    app.bind = "not a socket".into();
    app.local_bt = "not a Bluetooth address".into();
    app.iphone_ip = "not an IP address".into();
    app.hotspot_custom = true;
    let options = app.connection_options().unwrap();
    assert!(matches!(
        options.transport,
        TransportOptions::Usb { device_id: None }
    ));
    app.usb_device = "selected-usb-device".into();
    let TransportOptions::Usb { device_id } = app.connection_options().unwrap().transport else {
        panic!("USB cannot fall back to wireless");
    };
    assert_eq!(device_id.as_deref(), Some("selected-usb-device"));
}

#[test]
fn default_hotspot_options_ignore_lan_address_and_old_phone_ip() {
    let ctx = egui::Context::default();
    let mut app = desktop(&ctx);
    select_wireless_devices(&mut app);
    app.mode = ConnectionMode::Hotspot;
    app.bind = "invalid old LAN socket".into();
    app.iphone_ip = "invalid old LAN phone IP".into();
    let TransportOptions::Hotspot {
        device,
        port,
        config,
    } = app.connection_options().unwrap().transport
    else {
        panic!("hotspot must prepare its own endpoint");
    };
    assert_eq!(port, 7000);
    assert!(device.iphone_ip.is_none());
    assert!(config.is_none());
}

#[test]
fn custom_hotspot_validates_its_own_credentials_without_lan_fields() {
    let ctx = egui::Context::default();
    let mut app = desktop(&ctx);
    select_wireless_devices(&mut app);
    app.mode = ConnectionMode::Hotspot;
    app.hotspot_custom = true;
    app.iphone_ip = "invalid old LAN phone IP".into();
    assert!(app.connection_options().is_err());
    app.hotspot_ssid = "TestCarPlay".into();
    app.hotspot_password = zeroize::Zeroizing::new("test-password".into());
    let TransportOptions::Hotspot {
        config: Some(config),
        device,
        ..
    } = app.connection_options().unwrap().transport
    else {
        panic!("custom hotspot must retain its own configuration");
    };
    assert_eq!(config.ssid, "TestCarPlay");
    assert_eq!(config.passphrase.as_str(), "test-password");
    assert!(device.iphone_ip.is_none());
}

#[test]
fn the_only_decoded_frame_opens_player_after_queued_connection_events() {
    let ctx = egui::Context::default();
    let mut app = desktop(&ctx);
    // Decoding may have finished before the UI consumes the connection events.
    // This first frame must not become a rejected baseline requiring a second frame.
    let first = frame(0);
    receiver(&mut app, ReceiverEvent::TcpAccepted);
    receiver(&mut app, ReceiverEvent::Verified);
    assert_eq!(app.navigation.page(), Page::Home);
    assert!(app.texture.is_none());
    app.display_frame(&ctx, first.clone());
    assert_eq!(app.navigation.page(), Page::Player);
    assert!(app.texture.is_some());
    assert!(Arc::ptr_eq(app.last_frame.as_ref().unwrap(), &first));
    assert!(
        app.suppress_touch,
        "the previous page's input must not leak"
    );

    app.suppress_touch = false;
    app.display_frame(&ctx, first);
    assert!(
        !app.suppress_touch,
        "a cached frame is not another transition"
    );
}

#[test]
fn returning_home_keeps_receiving_frames_without_reopening_player() {
    let ctx = egui::Context::default();
    let mut app = desktop(&ctx);
    receiver(&mut app, ReceiverEvent::TcpAccepted);
    receiver(&mut app, ReceiverEvent::Verified);
    app.display_frame(&ctx, frame(0));
    app.go_home(&ctx);
    app.suppress_touch = false;
    for index in 1..=3 {
        let next = frame(index);
        app.display_frame(&ctx, next.clone());
        assert_eq!(app.navigation.page(), Page::Home);
        assert!(app.texture.is_some());
        assert!(Arc::ptr_eq(app.last_frame.as_ref().unwrap(), &next));
        assert!(!app.suppress_touch);
    }
    app.navigation.show_player();
    assert_eq!(app.navigation.page(), Page::Player);
}

#[test]
fn disconnect_and_reconnect_reject_cached_and_unverified_frames() {
    let ctx = egui::Context::default();
    let mut app = desktop(&ctx);
    receiver(&mut app, ReceiverEvent::TcpAccepted);
    receiver(&mut app, ReceiverEvent::Verified);
    let previous = frame(0);
    app.display_frame(&ctx, previous.clone());
    receiver(&mut app, ReceiverEvent::Disconnected);
    app.suppress_touch = false;
    assert_eq!(app.navigation.page(), Page::Home);
    assert!(app.texture.is_none());

    app.display_frame(&ctx, previous.clone());
    app.display_frame(&ctx, frame(1));
    assert_eq!(app.navigation.page(), Page::Home);
    assert!(app.texture.is_none());
    assert!(Arc::ptr_eq(app.last_frame.as_ref().unwrap(), &previous));

    receiver(&mut app, ReceiverEvent::TcpAccepted);
    let next = frame(2);
    app.display_frame(&ctx, next.clone());
    assert!(app.texture.is_none());
    receiver(&mut app, ReceiverEvent::Verified);
    app.display_frame(&ctx, previous);
    assert!(
        app.texture.is_none(),
        "verification cannot resurrect the old frame"
    );
    app.display_frame(&ctx, next);
    assert_eq!(app.navigation.page(), Page::Player);
    assert!(app.texture.is_some());
    assert!(app.suppress_touch);
}

#[test]
fn returning_home_exits_fullscreen_and_releases_an_active_contact() {
    let ctx = egui::Context::default();
    let mut app = desktop(&ctx);
    receiver(&mut app, ReceiverEvent::TcpAccepted);
    receiver(&mut app, ReceiverEvent::Verified);
    app.display_frame(&ctx, frame(0));
    app.set_fullscreen(&ctx, true);
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640., 360.));
    let contacts = app.touch.process(
        rect,
        &[egui::Event::PointerButton {
            pos: egui::pos2(100., 120.),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        }],
        true,
        true,
    );
    assert_eq!(contacts.len(), 1);
    assert!(contacts[0].down);
    app.go_home(&ctx);
    assert_eq!(app.navigation.page(), Page::Home);
    assert!(!app.fullscreen);
    assert!(app.suppress_touch);
    assert!(
        app.touch.release().is_none(),
        "go_home must already release contact"
    );
}
