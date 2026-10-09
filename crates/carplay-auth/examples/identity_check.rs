// SPDX-License-Identifier: GPL-3.0-only
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("usage: identity_check <external-identity-directory>")?;
    let identity = carplay_auth::LocalIdentity::load(directory)?;
    println!(
        "identity key/certificate consistency: {}",
        identity.self_check().key_matches_certificate
    );
    println!("algorithm: {}", identity.self_check().algorithm);
    println!(
        "iPhone trust verified: {}",
        identity.self_check().iphone_trust_verified
    );
    Ok(())
}
