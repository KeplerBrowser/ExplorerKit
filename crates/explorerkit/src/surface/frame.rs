#[cfg(all(feature = "servo", target_os = "macos"))]
use super::macos::ExportableRenderingContext;
#[cfg(target_os = "macos")]
use super::macos::GpuFrame;
#[cfg(feature = "servo")]
use explorerkit_embedder::WebViewHandle;
#[cfg(target_os = "macos")]
use explorerkit_host::SurfaceError;
use explorerkit_host::{SurfaceFrameInfo, SurfaceFrameLike, SurfaceMode, SurfaceViewport};
#[cfg(feature = "servo")]
use servo::RenderingContext;
use std::fmt;
#[cfg(feature = "servo")]
use webrender_api::units::{DeviceIntPoint, DeviceIntRect, DeviceIntSize};

/// RGBA pixels read from one CPU-offscreen Servo frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceFrameImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl SurfaceFrameImage {
    /// Returns the image width in physical pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Returns the image height in physical pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Borrows the tightly packed RGBA8 pixel bytes.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Consumes the image and returns its tightly packed RGBA8 pixel bytes.
    pub fn into_rgba(self) -> Vec<u8> {
        self.rgba
    }
}

/// A painted Servo frame handed to host present/composite code.
///
/// Hosts can call [`SurfaceFrame::present`] for native-surface swap/present behavior, use
/// [`SurfaceFrame::read_rgba`] when an offscreen target supports CPU access, or take an exported
/// macOS GPU frame from an exportable offscreen target.
pub struct SurfaceFrame<'a> {
    #[cfg(feature = "servo")]
    rendering_context: Option<&'a dyn RenderingContext>,
    #[cfg(not(feature = "servo"))]
    _lifetime: std::marker::PhantomData<&'a ()>,
    info: SurfaceFrameInfo,
    did_present: bool,
    #[cfg(all(feature = "servo", target_os = "macos"))]
    webview: Option<WebViewHandle>,
    #[cfg(all(feature = "servo", target_os = "macos"))]
    exportable_context: Option<&'a ExportableRenderingContext>,
    #[cfg(all(feature = "servo", target_os = "macos"))]
    gpu_frame_taken: bool,
}

impl<'a> SurfaceFrame<'a> {
    #[cfg(feature = "servo")]
    pub(crate) fn new(
        webview: WebViewHandle,
        rendering_context: &'a dyn RenderingContext,
        viewport: SurfaceViewport,
        mode: SurfaceMode,
        #[cfg(all(feature = "servo", target_os = "macos"))] exportable_context: Option<
            &'a ExportableRenderingContext,
        >,
    ) -> Self {
        Self {
            rendering_context: Some(rendering_context),
            info: SurfaceFrameInfo::new(viewport, mode),
            did_present: false,
            #[cfg(all(feature = "servo", target_os = "macos"))]
            webview: Some(webview),
            #[cfg(all(feature = "servo", target_os = "macos"))]
            exportable_context,
            #[cfg(all(feature = "servo", target_os = "macos"))]
            gpu_frame_taken: false,
        }
    }

    #[cfg(all(test, feature = "servo"))]
    pub(crate) fn for_test(viewport: SurfaceViewport, mode: SurfaceMode) -> Self {
        Self {
            rendering_context: None,
            info: SurfaceFrameInfo::new(viewport, mode),
            did_present: false,
            #[cfg(all(feature = "servo", target_os = "macos"))]
            webview: None,
            #[cfg(all(feature = "servo", target_os = "macos"))]
            exportable_context: None,
            #[cfg(all(feature = "servo", target_os = "macos"))]
            gpu_frame_taken: false,
        }
    }

    /// Returns the viewport and surface-mode metadata for this frame.
    pub fn info(&self) -> SurfaceFrameInfo {
        self.info
    }

    /// Returns the viewport painted into this frame.
    pub fn viewport(&self) -> SurfaceViewport {
        self.info.viewport()
    }

    /// Returns the surface mode used to paint this frame.
    pub fn mode(&self) -> SurfaceMode {
        self.info.mode()
    }

    /// Reports whether [`SurfaceFrame::present`] has been called.
    pub fn did_present(&self) -> bool {
        self.did_present
    }

    /// Run ExplorerKit's default present operation for this target.
    ///
    /// For native targets this swaps/presents the native rendering surface. Servo's local
    /// offscreen context has no default swap operation; hosts normally composite it via
    /// [`SurfaceFrame::read_rgba`] instead.
    pub fn present(&mut self) {
        #[cfg(feature = "servo")]
        if let Some(rendering_context) = self.rendering_context {
            rendering_context.present();
        }
        self.did_present = true;
    }

    /// Read the painted frame as RGBA pixels for host-side compositing.
    pub fn read_rgba(&self) -> Option<SurfaceFrameImage> {
        #[cfg(not(feature = "servo"))]
        {
            None
        }
        #[cfg(feature = "servo")]
        {
            let rendering_context = self.rendering_context?;
            let size = rendering_context.size();
            let width = i32::try_from(size.width).ok()?;
            let height = i32::try_from(size.height).ok()?;
            let rectangle = DeviceIntRect::from_origin_and_size(
                DeviceIntPoint::new(0, 0),
                DeviceIntSize::new(width, height),
            );
            let image = rendering_context.read_to_image(rectangle)?;
            Some(SurfaceFrameImage {
                width: image.width(),
                height: image.height(),
                rgba: image.into_raw(),
            })
        }
    }

    /// Takes the macOS GPU resource painted for this frame, when the attached offscreen target is
    /// exportable. The returned completion token must be completed after the consumer's final GPU
    /// sample. Calling this more than once for one frame is an error.
    #[cfg(target_os = "macos")]
    pub fn take_gpu_frame(&mut self) -> Result<Option<GpuFrame>, SurfaceError> {
        #[cfg(not(feature = "servo"))]
        {
            Ok(None)
        }
        #[cfg(feature = "servo")]
        {
            let Some(context) = self.exportable_context else {
                return Ok(None);
            };
            if self.gpu_frame_taken {
                return Err(SurfaceError::new(
                    "the exportable GPU frame has already been taken",
                ));
            }
            let webview = self
                .webview
                .expect("real exportable frames always carry webview identity");
            let frame = context.take_gpu_frame(webview)?;
            self.gpu_frame_taken = true;
            Ok(Some(frame))
        }
    }
}

impl Drop for SurfaceFrame<'_> {
    fn drop(&mut self) {
        #[cfg(all(feature = "servo", target_os = "macos"))]
        if !self.gpu_frame_taken {
            if let Some(context) = self.exportable_context {
                let _ = context.recycle_unpublished();
            }
        }
    }
}

impl SurfaceFrameLike for SurfaceFrame<'_> {
    fn present(&mut self) {
        Self::present(self);
    }
}

impl fmt::Debug for SurfaceFrame<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SurfaceFrame")
            .field("viewport", &self.viewport())
            .field("mode", &self.mode())
            .field("did_present", &self.did_present)
            .finish_non_exhaustive()
    }
}
