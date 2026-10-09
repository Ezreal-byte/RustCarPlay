//! Read-only smoke test. Deliberately prints only counts, never picker names or addresses.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adapters = carplay_platform::bluetooth::local_adapters()?;
    let devices = carplay_platform::bluetooth::paired_devices()?;
    println!(
        "Classic Bluetooth adapters: {}; remembered/authenticated devices: {}",
        adapters.len(),
        devices.len()
    );
    Ok(())
}
