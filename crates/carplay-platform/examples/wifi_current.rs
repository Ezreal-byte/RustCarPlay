//! Prints capability data only. Never print the connected SSID.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    match carplay_platform::wifi::current_wifi()? {
        Some(wifi) => println!(
            "connected Wi-Fi: true; security: {:?}; interface_index: {:?}; SSID withheld",
            wifi.security, wifi.interface_index
        ),
        None => println!("connected Wi-Fi: false"),
    }
    Ok(())
}
