fn main() {
    match serde_json::to_string_pretty(&carplay_platform::collect_diagnostics()) {
        Ok(report) => println!("{report}"),
        Err(error) => {
            eprintln!("diagnostic serialization failed: {error}");
            std::process::exit(1);
        }
    }
}
