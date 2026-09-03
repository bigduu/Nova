fn main() {
    // stdout is exclusively the Chrome Native Messaging frame stream. All
    // diagnostics, including this terminal error, must remain on stderr.
    if let Err(error) = nova_chrome_bridge::run_native_host() {
        eprintln!("nova-chrome-host: {error:#}");
        std::process::exit(1);
    }
}
