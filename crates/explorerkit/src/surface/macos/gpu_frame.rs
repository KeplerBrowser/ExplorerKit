use super::super::SurfaceSize;
#[cfg(feature = "servo")]
use core_foundation::base::CFRetain;
use core_foundation::base::{CFRelease, CFTypeRef};
pub use core_video::pixel_buffer::CVPixelBuffer;
use explorerkit_embedder::WebViewHandle;
#[cfg(feature = "servo")]
use objc2_core_foundation::CFRetained;
use std::fmt;
use std::mem;
use std::sync::{mpsc, Arc};
#[cfg(feature = "servo")]
use surfman::default::surface::NativeSurface as SurfmanNativeSurface;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FrameIdentity {
    pub(super) webview: WebViewHandle,
    pub(super) generation: u64,
    pub(super) slot: u32,
    pub(super) frame_serial: u64,
}

/// Stable identity and physical extent for one exported macOS GPU frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuFrameInfo {
    pub(super) identity: FrameIdentity,
    pub(super) physical_size: SurfaceSize,
}

impl GpuFrameInfo {
    /// Returns the webview that produced this frame.
    pub fn webview(&self) -> WebViewHandle {
        self.identity.webview
    }

    /// Returns the physical-size generation that owns this frame.
    pub fn generation(&self) -> u64 {
        self.identity.generation
    }

    /// Returns the producer pool slot identity within its generation.
    pub fn slot(&self) -> u32 {
        self.identity.slot
    }

    /// Returns the monotonically increasing frame identity for this webview.
    pub fn frame_serial(&self) -> u64 {
        self.identity.frame_serial
    }

    /// Returns the exact physical pixel extent of the exported storage.
    pub fn physical_size(&self) -> SurfaceSize {
        self.physical_size
    }
}

/// A retained opaque BGRA macOS frame exported from ExplorerKit-owned GPU storage.
#[must_use = "the frame must be completed before ExplorerKit can reuse its storage"]
pub struct GpuFrame {
    pub(super) info: GpuFrameInfo,
    pub(super) pixel_buffer: CVPixelBuffer,
    pub(super) completion: GpuFrameCompletion,
}

impl GpuFrame {
    /// Returns this frame's stable producer identity and physical extent.
    pub fn info(&self) -> GpuFrameInfo {
        self.info
    }

    /// Borrows the retained single-plane opaque 32BGRA CoreVideo pixel buffer.
    pub fn pixel_buffer(&self) -> &CVPixelBuffer {
        &self.pixel_buffer
    }

    /// Splits the frame into its retained pixel buffer and one-shot completion token.
    pub fn into_parts(self) -> (CVPixelBuffer, GpuFrameCompletion) {
        (self.pixel_buffer, self.completion)
    }
}

impl fmt::Debug for GpuFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuFrame")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

pub(super) struct PendingCompletion {
    pub(super) identity: FrameIdentity,
    pub(super) sender: mpsc::Sender<FrameIdentity>,
    pub(super) wake: Arc<dyn Fn() + Send + Sync + 'static>,
    pub(super) quarantine: Option<IOSurfaceLease>,
}

pub(super) struct IOSurfaceLease(usize);

impl IOSurfaceLease {
    #[cfg(feature = "servo")]
    pub(super) fn new(surface: &SurfmanNativeSurface) -> Self {
        let reference = CFRetained::as_ptr(&surface.0).as_ptr() as CFTypeRef;
        // SAFETY: the surfman native surface is live and owns this IOSurface reference.
        unsafe { CFRetain(reference) };
        Self(reference as usize)
    }
}

impl Drop for IOSurfaceLease {
    fn drop(&mut self) {
        // IOSurface is a process-shareable Core Foundation object whose retain/release operations
        // are thread-safe. Storing the retained address as `usize` keeps the completion token Send
        // without claiming that surfman's thread-local wrapper itself is Send.
        unsafe { CFRelease(self.0 as CFTypeRef) };
    }
}

/// One-shot acknowledgement that the consumer has finished sampling a GPU frame.
#[must_use = "dropping an incomplete token permanently quarantines the frame storage"]
pub struct GpuFrameCompletion {
    pub(super) pending: Option<PendingCompletion>,
}

impl GpuFrameCompletion {
    /// Acknowledges that all consumer GPU work which can sample this frame has completed.
    pub fn complete(mut self) {
        let Some(mut pending) = self.pending.take() else {
            return;
        };
        if pending.sender.send(pending.identity).is_ok() {
            (pending.wake)();
        }
        pending.quarantine.take();
    }
}

impl fmt::Debug for GpuFrameCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuFrameCompletion")
            .field("pending", &self.pending.is_some())
            .finish()
    }
}

impl Drop for GpuFrameCompletion {
    fn drop(&mut self) {
        if let Some(mut pending) = self.pending.take() {
            // A missing consumer completion must never make the producer allocation reusable.
            // Leaking one retained IOSurface is the bounded fail-safe; process exit reclaims it.
            if let Some(quarantine) = pending.quarantine.take() {
                mem::forget(quarantine);
            }
        }
    }
}
