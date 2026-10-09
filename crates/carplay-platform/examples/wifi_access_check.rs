//! Explicit local capability check. Prints only booleans and security type.
use carplay_platform::{PlatformError, network, wifi};

fn main() {
    let result = wifi::current_wifi().and_then(|current| {
        let Some(current) = current else {
            return Ok(None);
        };
        let address = network::diagnose().addresses.into_iter().find(|entry| {
            Some(entry.interface_index) == current.interface_index
                && entry.is_up
                && entry.address.is_ipv4()
        });
        match address {
            Some(address) => wifi::credentials_for_address(address.address).map(Some),
            None => Ok(None),
        }
    });
    match result {
        Ok(Some(value)) => println!(
            "success=true security_type={} secret_available={}",
            value.security_type,
            !value.passphrase.is_empty()
        ),
        Ok(None) => println!("success=false current_wifi_available=false"),
        Err(error) => {
            let denied = matches!(error, PlatformError::Io(ref error) if error.kind() == std::io::ErrorKind::PermissionDenied);
            println!("success=false permission_denied={denied}");
            std::process::exit(1);
        }
    }
}
