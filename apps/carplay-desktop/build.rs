// SPDX-License-Identifier: GPL-3.0-only
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Portable macOS packaging replaces SDK dylib names with longer paths
    // relative to this executable. Reserve load-command space when linking.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-headerpad_max_install_names");
    }
}
