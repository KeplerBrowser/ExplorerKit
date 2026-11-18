//! Owned result and host capability for the macOS WebKit snapshot.

use futures_channel::oneshot;

use crate::{Host, HostError, WebViewHandle};

/// A snapshot of the native WebKit viewport in tightly packed, top-down pixels.
#[derive(Debug)]
pub struct WebKitSnapshot {
    /// Image width in physical pixels.
    pub width: u32,
    /// Image height in physical pixels.
    pub height: u32,
    /// Straight-alpha sRGB RGBA8 bytes in row-major order.
    pub rgba: Vec<u8>,
}

/// An in-flight WebKit snapshot. Await it or poll it without blocking AppKit.
pub type WebKitSnapshotRequest = oneshot::Receiver<Result<WebKitSnapshot, HostError>>;

/// The sender held by the native snapshot completion block.
pub type WebKitSnapshotReply = oneshot::Sender<Result<WebKitSnapshot, HostError>>;

/// Host support for the native system-WebView snapshot.
pub trait WebKitSnapshotHost: Host {
    /// Submits the request for the host's attached native WebKit view.
    fn start_webkit_snapshot(
        &mut self,
        webview: WebViewHandle,
        width: u32,
        reply: WebKitSnapshotReply,
    ) -> Result<(), HostError>;
}
