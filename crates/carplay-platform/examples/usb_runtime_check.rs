//! Read-only Windows USB runtime smoke check. This does not pair, start a
//! service on the phone, request USB mode changes, or change drivers.

#[cfg(target_os = "windows")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};
    use std::time::Duration;

    carplay_platform::usb::system::runtime_available()?;
    let local_usbmux_available = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, 27015)),
        Duration::from_millis(500),
    )
    .is_ok();
    println!(
        "{}",
        serde_json::json!({
            "runtime_abi_available": true,
            "local_usbmux_available": local_usbmux_available,
        })
    );
    if !local_usbmux_available {
        return Err("Apple Mobile Device Service is not available on localhost:27015".into());
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("--devices") && args.len() <= 2 {
        println!(
            "{}",
            serde_json::to_string(&carplay_platform::usb::system::usbmux_status(
                args.get(1).map(String::as_str)
            )?)?
        );
    } else if !args.is_empty() {
        return Err("usage: usb_runtime_check [--devices [usb-device-id]]".into());
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("This example checks the Windows USB runtime only.");
}
