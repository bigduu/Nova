// screencapturekit references @rpath/libswift_Concurrency.dylib (and other
// Swift runtime libs). Bake an LC_RPATH pointing at the OS Swift runtime in the
// dyld shared cache so the binary — and test/bench harnesses — load without any
// DYLD_* environment shim. Present on every macOS that can run SCK (14.0+).
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
