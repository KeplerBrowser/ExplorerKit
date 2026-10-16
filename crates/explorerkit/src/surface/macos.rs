#![allow(deprecated)]

use std::{ffi::c_void, ptr::NonNull};

use cocoa::{
    appkit::NSView,
    base::{id, nil, YES},
    foundation::{NSPoint, NSRect, NSSize},
};
use raw_window_handle::{
    AppKitWindowHandle, DisplayHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
};

use super::{NativeSurface, SurfaceError, SurfaceSize, SurfaceViewport};

mod exportable;

pub(crate) use exportable::ExportableRenderingContext;
pub use exportable::{CVPixelBuffer, GpuFrame, GpuFrameCompletion, GpuFrameInfo};

/// Logical AppKit bounds, in points, for an embedded child surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppKitChildViewBounds {
    /// Horizontal offset from the parent view's left edge, in logical points.
    pub x: f64,
    /// Vertical offset from the parent view's top edge, in logical points.
    pub y: f64,
    /// Child-view width in logical points.
    pub width: f64,
    /// Child-view height in logical points.
    pub height: f64,
}

impl AppKitChildViewBounds {
    /// Creates logical top-left AppKit child-view bounds.
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Borrowed-handle snapshot for an AppKit child surface refreshed by `AppKitChildSurface`.
///
/// This type is copyable so callers can move it out of their own state/borrows before converting
/// the raw handles into borrowed `DisplayHandle`, `WindowHandle`, or `NativeSurface` values.
/// The underlying app-owned display and AppKit views must remain valid for the duration of any
/// borrow created from this snapshot.
#[derive(Debug, Clone, Copy)]
pub struct AppKitChildSurfaceHandles {
    display_handle: RawDisplayHandle,
    window_handle: RawWindowHandle,
}

impl AppKitChildSurfaceHandles {
    /// Borrows the host display handle captured by the latest update.
    pub fn display_handle<'a>(self) -> DisplayHandle<'a> {
        // SAFETY: `AppKitChildSurface::update` stored this raw handle from a live host display.
        // Callers must keep the app-owned display/window alive while borrowing it.
        unsafe { DisplayHandle::borrow_raw(self.display_handle) }
    }

    /// Borrows the AppKit child-view handle captured by the latest update.
    pub fn window_handle<'a>(self) -> WindowHandle<'a> {
        // SAFETY: `AppKitChildSurface::update` stored this raw handle from the live child `NSView`
        // that ExplorerKit created or refreshed under the host-owned parent view.
        unsafe { WindowHandle::borrow_raw(self.window_handle) }
    }

    /// Converts the captured handles into an ExplorerKit native-child surface.
    pub fn native_surface<'a>(self) -> NativeSurface<'a> {
        NativeSurface::new(self.display_handle(), self.window_handle())
    }
}

/// macOS AppKit helper for CEF-like child-surface embedding.
///
/// The host app or framework keeps ownership of its window, parent `NSView`, layout bounds, and
/// event loop. ExplorerKit keeps track of the browser child `NSView` that it creates under that
/// parent, updates the child view's frame from logical AppKit bounds plus scale factor, and
/// exposes copyable raw-handle snapshots suitable for `NativeSurface`.
///
/// `update` must be called with live parent/display handles from the host before attaching or
/// refreshing the embedded surface. Handles returned by `handles` assume the underlying app-owned
/// display and AppKit views remain valid for the duration of any borrow created from that
/// snapshot.
#[derive(Debug, Default)]
pub struct AppKitChildSurface {
    child_view: Option<NonNull<c_void>>,
    display_handle: Option<RawDisplayHandle>,
    window_handle: Option<RawWindowHandle>,
    viewport: Option<SurfaceViewport>,
}

impl AppKitChildSurface {
    /// Creates an uninitialized AppKit child-surface helper.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the most recently computed physical viewport.
    pub fn viewport(&self) -> Option<SurfaceViewport> {
        self.viewport
    }

    /// Creates or repositions the child view and records fresh native handles.
    pub fn update(
        &mut self,
        display_handle: DisplayHandle<'_>,
        parent_window_handle: WindowHandle<'_>,
        bounds: AppKitChildViewBounds,
        scale_factor: f32,
    ) -> Result<SurfaceViewport, SurfaceError> {
        let parent = appkit_parent_view(parent_window_handle)?;
        let child = ensure_child_view(self.child_view, parent, bounds)?;
        let viewport = SurfaceViewport::new(physical_size(bounds, scale_factor), scale_factor);

        self.child_view = Some(child);
        self.display_handle = Some(display_handle.as_raw());
        self.window_handle = Some(RawWindowHandle::AppKit(AppKitWindowHandle::new(child)));
        self.viewport = Some(viewport);

        Ok(viewport)
    }

    /// Returns the latest native handles after a successful update.
    pub fn handles(&self) -> Result<AppKitChildSurfaceHandles, SurfaceError> {
        Ok(AppKitChildSurfaceHandles {
            display_handle: self
                .display_handle
                .ok_or_else(|| SurfaceError::new("AppKit display handle is not ready"))?,
            window_handle: self
                .window_handle
                .ok_or_else(|| SurfaceError::new("AppKit child NSView is not ready"))?,
        })
    }
}

fn appkit_parent_view(parent_window_handle: WindowHandle<'_>) -> Result<id, SurfaceError> {
    match parent_window_handle.as_raw() {
        RawWindowHandle::AppKit(handle) => Ok(handle.ns_view.as_ptr() as id),
        other => Err(SurfaceError::new(format!(
            "AppKit child surface requires an AppKit parent NSView handle, got {other:?}",
        ))),
    }
}

fn ensure_child_view(
    existing: Option<NonNull<c_void>>,
    parent: id,
    bounds: AppKitChildViewBounds,
) -> Result<NonNull<c_void>, SurfaceError> {
    if parent == nil {
        return Err(SurfaceError::new("AppKit parent NSView was null"));
    }

    unsafe {
        let frame = appkit_frame(parent, bounds);
        let child = match existing {
            Some(child_ptr) => {
                let child = child_ptr.as_ptr() as id;
                if child == nil {
                    return Err(SurfaceError::new("AppKit child NSView was null"));
                }
                if child.superview() != parent {
                    child.removeFromSuperview();
                    parent.addSubview_(child);
                }
                child
            }
            None => {
                let child = NSView::alloc(nil).initWithFrame_(frame);
                if child == nil {
                    return Err(SurfaceError::new("failed to allocate AppKit child NSView"));
                }
                child.setWantsLayer(YES);
                child.setWantsBestResolutionOpenGLSurface_(YES);
                parent.addSubview_(child);
                child
            }
        };

        child.setFrameOrigin(frame.origin);
        child.setFrameSize(frame.size);

        NonNull::new(child as *mut c_void)
            .ok_or_else(|| SurfaceError::new("AppKit child NSView was null"))
    }
}

/// Return the child frame in its AppKit parent's coordinate system.
///
/// # Safety
/// `parent` must be a live `NSView` accessed on the AppKit main thread.
unsafe fn appkit_frame(parent: id, bounds: AppKitChildViewBounds) -> NSRect {
    let parent_bounds = parent.bounds();
    let width = bounds.width.max(1.0);
    let height = bounds.height.max(1.0);

    NSRect::new(
        NSPoint::new(bounds.x, parent_bounds.size.height - bounds.y - height),
        NSSize::new(width, height),
    )
}

fn physical_size(bounds: AppKitChildViewBounds, scale_factor: f32) -> SurfaceSize {
    SurfaceSize::new(
        ((bounds.width as f32 * scale_factor).round() as u32).max(1),
        ((bounds.height as f32 * scale_factor).round() as u32).max(1),
    )
}
