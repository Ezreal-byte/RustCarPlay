//! Read-only USB preflight: no mode switch, interface claim or driver changes.
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let phones = match args.as_slice() {
        [] => carplay_platform::usb::native::discover()?,
        [id] => vec![carplay_platform::usb::native::probe(Some(id))?],
        _ => return Err("usage: usb_probe [usb-device-id]".into()),
    };
    serde_json::to_writer_pretty(std::io::stdout().lock(), &phones)?;
    println!();
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn main() {
    eprintln!("USB discovery is currently supported on Windows and Linux");
}
