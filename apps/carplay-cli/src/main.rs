// SPDX-License-Identifier: GPL-3.0-only
use anyhow::{Context, Result};
use carplay_platform::bluetooth::BluetoothAddress;
use clap::{Parser, Subcommand, ValueEnum};
use std::{
    io::Write,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Parser)]
#[command(
    version,
    about = "RustCarPlay receiver and read-only platform diagnostics"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum Mode {
    #[default]
    Lan,
    Hotspot,
    Usb,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect host capabilities without changing pairing, USB drivers or network routes.
    Doctor {
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Validate local MFi key/certificate consistency. Does not establish iPhone trust.
    AuthCheck {
        #[arg(long, default_value = ".local/auth")]
        assets: PathBuf,
    },
    /// Print the default configuration, or validate an existing JSON configuration.
    Config {
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Connect via existing LAN, a computer-hosted hotspot, or USB.
    Connect {
        #[arg(long, value_enum, default_value_t = Mode::Lan)]
        mode: Mode,
        /// Local Wi-Fi address. If omitted, infer it from the current system Wi-Fi.
        #[arg(long)]
        bind: Option<SocketAddr>,
        #[arg(long)]
        local_bluetooth: Option<BluetoothAddress>,
        #[arg(long)]
        iphone: Option<BluetoothAddress>,
        #[arg(long)]
        usb_device: Option<String>,
        /// Phone Wi-Fi IP, if its mDNS service omits a stable Bluetooth identifier.
        #[arg(long)]
        iphone_ip: Option<std::net::IpAddr>,

        #[arg(long)]
        rfcomm_channel: Option<u8>,
        #[arg(long, default_value = ".local/auth")]
        auth_dir: PathBuf,
        #[arg(long, default_value = ".local/state")]
        state_dir: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Host AirPlay on an explicit interface; the caller supplies iAP2 bootstrap separately.
    Serve {
        #[arg(long)]
        bind: SocketAddr,
        #[arg(long)]
        local_bluetooth: BluetoothAddress,
        #[arg(long, default_value = ".local/auth")]
        auth_dir: PathBuf,
        #[arg(long, default_value = ".local/state")]
        state_dir: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        no_discovery: bool,
    },
}
fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Doctor { output } => {
            let report = carplay_platform::collect_diagnostics();
            let json = serde_json::to_string_pretty(&report)?;
            if let Some(path) = output {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?;
                file.write_all(json.as_bytes())?;
            } else {
                println!("{json}");
            }
        }
        Command::AuthCheck { assets } => {
            let identity = carplay_auth::LocalIdentity::load(assets)?;
            println!("{:?}", identity.self_check());
            println!("Local consistency passed; iPhone acceptance requires a live connection.");
        }
        Command::Config { file } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&carplay_app::load_config(file.as_deref())?)?
            );
        }
        Command::Connect {
            mode,
            bind,
            local_bluetooth,
            iphone,
            iphone_ip,
            usb_device,
            rfcomm_channel,
            auth_dir,
            state_dir,
            config,
        } => {
            let wireless = || -> Result<carplay_app::WirelessDevice> {
                Ok(carplay_app::WirelessDevice {
                    local_bluetooth: local_bluetooth
                        .context("--local-bluetooth is required for LAN/hotspot")?,
                    iphone: iphone.context("--iphone is required for LAN/hotspot")?,
                    iphone_ip,
                    rfcomm_channel,
                })
            };
            let transport = match mode {
                Mode::Lan => carplay_app::TransportOptions::Lan {
                    bind: match bind {
                        Some(bind) => bind,
                        None => current_wifi_bind()?,
                    },
                    device: wireless()?,
                },
                Mode::Hotspot => carplay_app::TransportOptions::Hotspot {
                    port: bind.map(|b| b.port()).unwrap_or(7000),
                    device: wireless()?,
                    config: None,
                },
                Mode::Usb => carplay_app::TransportOptions::Usb {
                    device_id: usb_device,
                },
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let stopped = cancel.clone();
            ctrlc::set_handler(move || stopped.store(true, Ordering::Release))?;
            let mut connection = carplay_app::Connection::start_with_cancel(
                carplay_app::ConnectionOptions {
                    config: carplay_app::load_config(config.as_deref())?,
                    transport,
                    auth_dir,
                    state_dir,
                },
                cancel.clone(),
            )?;
            while !cancel.load(Ordering::Acquire) {
                for event in connection.poll() {
                    println!("{event:?}");
                }
                if connection.is_finished() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            connection.stop_checked()?;
        }
        Command::Serve {
            bind,
            local_bluetooth,
            auth_dir,
            state_dir,
            config,
            no_discovery,
        } => {
            let auth = Arc::new(carplay_auth::LocalIdentity::load(auth_dir)?);
            let media = Arc::new(carplay_media::GStreamerMediaSink::new()?);
            let mut server = carplay_receiver::start(
                carplay_receiver::ReceiverOptions {
                    bind,
                    config: carplay_app::load_config(config.as_deref())?,
                    bluetooth_address: local_bluetooth.to_colon_string(),
                    state_dir,
                    advertise: !no_discovery,
                },
                auth,
                media.clone(),
            )?;
            let cancel = Arc::new(AtomicBool::new(false));
            let stopped = cancel.clone();
            ctrlc::set_handler(move || stopped.store(true, Ordering::Release))?;
            while !cancel.load(Ordering::Acquire) {
                for event in server.events.try_iter() {
                    println!("{event:?}");
                }
                for error in media.take_errors() {
                    eprintln!("Media error: {}", error.message);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            server.stop();
        }
    }
    Ok(())
}

fn current_wifi_bind() -> Result<SocketAddr> {
    let wifi =
        carplay_platform::wifi::current_wifi()?.context("connect the computer to Wi-Fi first")?;
    let index = wifi
        .interface_index
        .context("system Wi-Fi interface has no index")?;
    carplay_platform::network::diagnose()
        .addresses
        .into_iter()
        .find(|a| a.is_up && a.interface_index == index && a.address.is_ipv4())
        .map(|a| SocketAddr::new(a.address, 7000))
        .context("current Wi-Fi interface has no IPv4 address")
}
