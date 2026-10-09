// SPDX-License-Identifier: GPL-3.0-only
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Keep space for the private GStreamer paths written by macOS packaging.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-headerpad_max_install_names");
    }
}
