// This crate is a packaging-only meta-package.
// It ships no code and no files — its `.deb` exists solely so
// `sudo apt install ./bacakos_*.deb` pulls in every BacakOS component
// (compositor, display manager, shell apps, plugins, brand themes,
// desktop defaults) as a single dependency list. See Cargo.toml's
// `[package.metadata.deb] depends`.
