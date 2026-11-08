//! Minimal AppKit consumer of the public macOS WebKit automation API.
//! The application owns presentation and helper launch/reaping. Run with a
//! compatible EXPLORERKIT_MINIDRIVER and send JSON commands on stdin; the native
//! acceptance client exercises this same consumer. No SPI is used by this host.
#![allow(deprecated)]
#[cfg(target_os = "macos")]
mod app;
fn main() {
    #[cfg(target_os = "macos")]
    app::run();
    #[cfg(not(target_os = "macos"))]
    panic!("this example requires macOS");
}
