//! Webview handle and command facade exports.

pub use explorerkit_embedder::{NavigationError, NavigationRequest, WebViewCommand, WebViewHandle};

#[cfg(all(any(feature = "servo", feature = "macos-webkit"), target_os = "macos"))]
/// macOS host adapter for create-time Servo or system-WebView selection.
pub mod macos;
