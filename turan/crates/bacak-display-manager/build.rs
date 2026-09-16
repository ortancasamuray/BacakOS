//! Build script: link libpam only when the `system-pam` feature is enabled.
//!
//! We avoid requiring the `libpam0g-dev` package (which provides the `libpam.so`
//! linker symlink and headers). Instead:
//!   * if a dev `libpam.so` is present, link it the normal way (`-lpam`);
//!   * otherwise link the runtime SONAME directly via `-l:libpam.so.0`, which
//!     GNU ld resolves by exact filename. This lets the daemon build on systems
//!     that ship only the runtime library.

use std::path::Path;

fn main() {
    if std::env::var_os("CARGO_FEATURE_SYSTEM_PAM").is_none() {
        return;
    }

    // Common library directories to search (multiarch + classic).
    let search_dirs = [
        "/usr/lib/x86_64-linux-gnu",
        "/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/lib/aarch64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
        "/lib",
    ];

    let mut have_dev_symlink = false;
    for dir in search_dirs {
        if Path::new(dir).exists() {
            println!("cargo:rustc-link-search=native={dir}");
            if Path::new(&format!("{dir}/libpam.so")).exists() {
                have_dev_symlink = true;
            }
        }
    }

    if have_dev_symlink {
        // libpam0g-dev is installed: standard linkage.
        println!("cargo:rustc-link-lib=dylib=pam");
    } else {
        // Runtime-only: link the SONAME by exact name.
        println!("cargo:rustc-link-arg=-l:libpam.so.0");
    }

    println!("cargo:rerun-if-changed=build.rs");
}
