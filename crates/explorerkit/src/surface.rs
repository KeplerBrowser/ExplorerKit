//! Surface facade exports.

pub use explorerkit_host::{
    HostSurface, NativeSurface, OffscreenSurface, SurfaceDelegate, SurfaceError, SurfaceFrameInfo,
    SurfaceFrameLike, SurfaceMode, SurfacePoint, SurfaceSize, SurfaceTarget, SurfaceViewport,
};

#[cfg(all(any(feature = "servo", feature = "macos-webkit"), target_os = "macos"))]
/// AppKit child-view helpers for macOS hosts that own their windows and layout.
pub mod macos;

#[cfg(any(
    all(
        feature = "servo",
        any(
            target_os = "android",
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        )
    ),
    all(feature = "macos-webkit", target_os = "macos")
))]
mod frame;
#[cfg(any(
    all(
        feature = "servo",
        any(
            target_os = "android",
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        )
    ),
    all(feature = "macos-webkit", target_os = "macos")
))]
mod options;
#[cfg(any(
    all(
        feature = "servo",
        any(
            target_os = "android",
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        )
    ),
    all(feature = "macos-webkit", target_os = "macos")
))]
pub use frame::{SurfaceFrame, SurfaceFrameImage};
#[cfg(any(
    all(
        feature = "servo",
        any(
            target_os = "android",
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        )
    ),
    all(feature = "macos-webkit", target_os = "macos")
))]
pub use options::{MemoryClipboard, SurfaceClipboard, SurfaceEventLoopWaker, SurfaceHostOptions};

#[cfg(all(
    feature = "servo",
    any(
        target_os = "android",
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    )
))]
pub use crate::surface_host::SurfaceHost;
