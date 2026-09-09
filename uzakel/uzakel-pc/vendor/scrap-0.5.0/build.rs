fn main() {
    // Upstream bug fixed here: a build script runs *for the host* that's
    // doing the compiling, so `cfg!(windows)`/`cfg!(unix)` reflect the host,
    // not the `--target` being cross-compiled for — which made this always
    // pick X11 when cross-compiling to Windows from a Linux host (needed for
    // bacak-remote-server's Windows build; see uzakel-pc/README.md). Reading
    // Cargo's `TARGET` env var (always the real target triple, set for every
    // build script) fixes it for both native and cross builds.
    let target = std::env::var("TARGET").unwrap_or_default();

    if target.contains("windows") {
        // The first choice is Windows because DXGI is amazing.
        println!("cargo:rustc-cfg=dxgi");
    } else if target.contains("apple") {
        // Quartz is second because macOS is the (annoying) exception.
        println!("cargo:rustc-cfg=quartz");
    } else {
        // On UNIX we pray that X11 (with XCB) is available.
        println!("cargo:rustc-cfg=x11");
    }
}
