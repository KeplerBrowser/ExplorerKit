use std::{cell::Cell, ptr};

use block2::RcBlock;
use explorerkit_embedder::webkit_snapshot::{
    WebKitSnapshot, WebKitSnapshotHost, WebKitSnapshotReply,
};
use explorerkit_embedder::{HostError, WebViewHandle};
use objc2::MainThreadMarker;
use objc2_app_kit::NSImage;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    kCGColorSpaceSRGB, CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo,
    CGImageByteOrderInfo,
};
use objc2_foundation::{NSError, NSNumber};
use objc2_web_kit::WKSnapshotConfiguration;

use wry::WebViewExtMacOS;

use super::{system_state_mut, AppKitViewDelegate, MacOsViewHost};
use crate::surface::{SurfaceDelegate, SurfaceFrame};

impl<S> WebKitSnapshotHost for MacOsViewHost<S>
where
    S: AppKitViewDelegate + for<'a> SurfaceDelegate<Frame<'a> = SurfaceFrame<'a>>,
{
    fn start_webkit_snapshot(
        &mut self,
        webview: WebViewHandle,
        width: u32,
        reply: WebKitSnapshotReply,
    ) -> Result<(), HostError> {
        let mtm = MainThreadMarker::new().ok_or_else(|| {
            HostError::new("WebKit snapshots must start on the AppKit main thread")
        })?;
        self.sync_views();
        let native = system_state_mut(&mut self.views, webview)?
            .webview
            .as_ref()
            .ok_or_else(|| HostError::new("system WebView has no native view yet"))?;
        let view = native.webview();
        let window = view
            .window()
            .ok_or_else(|| HostError::new("system WebView must be attached to a window"))?;
        let bounds = view.bounds().size;
        let scale = window.backingScaleFactor();
        if width == 0 || bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Err(HostError::new(
                "WebKit snapshot requires nonzero dimensions",
            ));
        }
        let height = (f64::from(width) * bounds.height / bounds.width)
            .round()
            .max(1.0);
        if !height.is_finite() || height > f64::from(u32::MAX) {
            return Err(HostError::new("WebKit snapshot height is out of range"));
        }
        let height = height as u32;
        let points = (f64::from(width) / scale)
            .ceil()
            .max((bounds.width / bounds.height).ceil());
        let native_width = points * scale;
        let native_height = native_width * bounds.height / bounds.width;
        // WebKit scales the snapshot request into a signed pixel extent.
        if native_width.ceil().max(native_height.ceil()) > f64::from(i32::MAX) {
            return Err(HostError::new("WebKit snapshot dimensions are too large"));
        }
        let row_bytes = width as usize * 4;
        let byte_count = row_bytes
            .checked_mul(height as usize)
            .filter(|&size| size <= isize::MAX as usize)
            .ok_or_else(|| HostError::new("WebKit snapshot dimensions are too large"))?;

        let reply = Cell::new(Some(reply));
        let block = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            let Some(reply) = reply.take() else { return };
            if reply.is_canceled() {
                return;
            }
            // SAFETY: WebKit supplies live nullable objects for the duration of its main-thread callback.
            let result = unsafe {
                if let Some(error) = error.as_ref() {
                    Err(HostError::new(error.localizedDescription().to_string()))
                } else if let Some(image) = image.as_ref() {
                    rasterize(image, width, height, row_bytes, byte_count)
                } else {
                    Err(HostError::new("WebKit returned no snapshot image"))
                }
            };
            let _ = reply.send(result);
        });

        // SAFETY: The view and configuration are used on their owning main thread.
        // WebKit copies the block, which owns just the reply and raster dimensions.
        unsafe {
            let configuration = WKSnapshotConfiguration::new(mtm);
            configuration.setSnapshotWidth(Some(&NSNumber::new_f64(points)));
            view.takeSnapshotWithConfiguration_completionHandler(Some(&configuration), &block);
        }
        Ok(())
    }
}

fn rasterize(
    image: &NSImage,
    width: u32,
    height: u32,
    row_bytes: usize,
    byte_count: usize,
) -> Result<WebKitSnapshot, HostError> {
    // SAFETY: The native image lives through the synchronous raster and the optional inputs are null.
    let image = unsafe { image.CGImageForProposedRect_context_hints(ptr::null_mut(), None, None) }
        .ok_or_else(|| HostError::new("WebKit snapshot has no bitmap representation"))?;
    // SAFETY: kCGColorSpaceSRGB is a process-wide Core Graphics constant.
    let color_space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or_else(|| HostError::new("could not create an sRGB color space"))?;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(byte_count)
        .map_err(|error| HostError::new(format!("could not allocate WebKit snapshot: {error}")))?;
    rgba.resize(byte_count, 0_u8);
    // SAFETY: The allocation provides exactly row_bytes * height writable, initialized bytes
    // and stays alive and unaliased until the context is released.
    let context = unsafe {
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            width as usize,
            height as usize,
            8,
            row_bytes,
            Some(&color_space),
            CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0,
        )
    }
    .ok_or_else(|| HostError::new("could not create the WebKit snapshot bitmap"))?;
    CGContext::draw_image(
        Some(&context),
        CGRect::new(
            CGPoint::ZERO,
            CGSize::new(f64::from(width), f64::from(height)),
        ),
        Some(&image),
    );
    drop(context);

    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha > 0 && alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    Ok(WebKitSnapshot {
        width,
        height,
        rgba,
    })
}
