//! Explicit USB mode switch for hardware bring-up. No driver or registry edits.
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("--switch-carplay") && args.len() <= 2 {
        let phone = carplay_platform::usb::native::request_carplay_mode(
            args.get(1).map(String::as_str),
            &AtomicBool::new(false),
            Duration::from_secs(20),
        )?;
        println!("{}", serde_json::to_string_pretty(&phone)?);
    } else if args.is_empty() {
        println!(
            "{}",
            serde_json::to_string_pretty(&carplay_platform::usb::diagnose())?
        );
    } else {
        return Err("usage: usb_mode [--switch-carplay [usb-device-id]]".into());
    }
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("USB mode switching is implemented on Windows and Linux only".into())
}
