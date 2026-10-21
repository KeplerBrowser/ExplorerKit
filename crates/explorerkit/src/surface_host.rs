use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::mem;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use dpi::PhysicalSize;
use explorerkit_embedder::{
    ContextMenuAction, Host, HostCall, HostError, HostEvent, HostInputEvent, NewWebViewDecision,
    NewWebViewRequest, PopupCreated, PopupRequestPolicy, ServoRuntime, ServoWebView,
    ServoWebViewInit, SessionHandle, WebViewCommand, WebViewHandle, WebViewRegistrar,
};
use explorerkit_host::{
    HostSurface, NativeSurface, OffscreenSurface, SurfaceDelegate, SurfaceError, SurfaceFrameInfo,
    SurfaceFrameLike, SurfaceMode, SurfaceSize, SurfaceTarget, SurfaceViewport,
};
use servo::{
    ClipboardDelegate, EventLoopWaker, OffscreenRenderingContext, RefreshDriver, RenderingContext,
    StringRequest, WebView, WindowRenderingContext,
};
use webrender_api::units::{DeviceIntPoint, DeviceIntRect, DeviceIntSize};

#[cfg(target_os = "macos")]
use crate::surface::macos::{ExportableRenderingContext, GpuFrame};

#[derive(Debug, Clone, Copy)]
enum RenderTarget<'a> {
    Native(NativeSurface<'a>),
    Offscreen(OffscreenSurface<'a>),
    #[cfg(test)]
    Test(SurfaceMode),
}

impl<'a> From<SurfaceTarget<'a>> for RenderTarget<'a> {
    fn from(target: SurfaceTarget<'a>) -> Self {
        match target {
            SurfaceTarget::Native(native_surface) => Self::Native(native_surface),
            SurfaceTarget::Offscreen(offscreen_target) => Self::Offscreen(offscreen_target),
        }
    }
}

impl RenderTarget<'_> {
    fn mode(&self) -> SurfaceMode {
        match self {
            Self::Native(_) => SurfaceMode::Native,
            Self::Offscreen(_) => SurfaceMode::Offscreen,
            #[cfg(test)]
            Self::Test(mode) => *mode,
        }
    }

    fn kind(&self) -> SurfaceMode {
        self.mode()
    }
}

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
    rendering_context: Option<&'a dyn RenderingContext>,
    info: SurfaceFrameInfo,
    did_present: bool,
    #[cfg(target_os = "macos")]
    webview: Option<WebViewHandle>,
    #[cfg(target_os = "macos")]
    exportable_context: Option<&'a ExportableRenderingContext>,
    #[cfg(target_os = "macos")]
    gpu_frame_taken: bool,
}

impl<'a> SurfaceFrame<'a> {
    fn new(
        webview: WebViewHandle,
        rendering_context: &'a dyn RenderingContext,
        viewport: SurfaceViewport,
        mode: SurfaceMode,
        #[cfg(target_os = "macos")] exportable_context: Option<&'a ExportableRenderingContext>,
    ) -> Self {
        Self {
            rendering_context: Some(rendering_context),
            info: SurfaceFrameInfo::new(viewport, mode),
            did_present: false,
            #[cfg(target_os = "macos")]
            webview: Some(webview),
            #[cfg(target_os = "macos")]
            exportable_context,
            #[cfg(target_os = "macos")]
            gpu_frame_taken: false,
        }
    }

    #[cfg(test)]
    fn for_test(viewport: SurfaceViewport, mode: SurfaceMode) -> Self {
        Self {
            rendering_context: None,
            info: SurfaceFrameInfo::new(viewport, mode),
            did_present: false,
            #[cfg(target_os = "macos")]
            webview: None,
            #[cfg(target_os = "macos")]
            exportable_context: None,
            #[cfg(target_os = "macos")]
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
        if let Some(rendering_context) = self.rendering_context {
            rendering_context.present();
        }
        self.did_present = true;
    }

    /// Read the painted frame as RGBA pixels for host-side compositing.
    pub fn read_rgba(&self) -> Option<SurfaceFrameImage> {
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

    /// Takes the macOS GPU resource painted for this frame, when the attached offscreen target is
    /// exportable. The returned completion token must be completed after the consumer's final GPU
    /// sample. Calling this more than once for one frame is an error.
    #[cfg(target_os = "macos")]
    pub fn take_gpu_frame(&mut self) -> Result<Option<GpuFrame>, SurfaceError> {
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

impl Drop for SurfaceFrame<'_> {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
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

/// Internal bridge for the Servo surface host.
///
/// Public callers implement [`SurfaceDelegate`]. The surface host keeps `WebViewHandle`-aware hooks
/// private to `explorerkit` so `explorerkit-host` stays host-neutral.
trait SurfaceHostServices {
    fn render_target(
        &mut self,
        surface: &HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<RenderTarget<'_>, SurfaceError>;

    fn update_render_target(
        &mut self,
        surface: &HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<RenderTarget<'_>, SurfaceError> {
        self.render_target(surface, viewport)
    }

    fn before_update(
        &mut self,
        _webview: WebViewHandle,
        _surface: Option<&HostSurface>,
        _viewport: Option<SurfaceViewport>,
    ) -> Result<(), SurfaceError> {
        Ok(())
    }

    fn present_frame(
        &mut self,
        _webview: WebViewHandle,
        _surface: &HostSurface,
        frame: &mut SurfaceFrame<'_>,
    ) -> Result<(), SurfaceError> {
        frame.present();
        Ok(())
    }
}

impl<T> SurfaceHostServices for T
where
    T: for<'a> SurfaceDelegate<Frame<'a> = SurfaceFrame<'a>>,
{
    fn render_target(
        &mut self,
        surface: &HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<RenderTarget<'_>, SurfaceError> {
        <T as SurfaceDelegate>::render_target(self, surface, viewport).map(RenderTarget::from)
    }

    fn update_render_target(
        &mut self,
        surface: &HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<RenderTarget<'_>, SurfaceError> {
        <T as SurfaceDelegate>::update_render_target(self, surface, viewport)
            .map(RenderTarget::from)
    }

    fn before_update(
        &mut self,
        _webview: WebViewHandle,
        surface: Option<&HostSurface>,
        viewport: Option<SurfaceViewport>,
    ) -> Result<(), SurfaceError> {
        <T as SurfaceDelegate>::before_update(self, surface, viewport)
    }

    fn present_frame(
        &mut self,
        _webview: WebViewHandle,
        surface: &HostSurface,
        frame: &mut SurfaceFrame<'_>,
    ) -> Result<(), SurfaceError> {
        <T as SurfaceDelegate>::present_frame(self, surface, frame)
    }
}

/// Clipboard services exposed through the ExplorerKit facade and adapted to Servo internally.
pub trait SurfaceClipboard {
    /// Clears clipboard text, defaulting to a successful no-op.
    fn clear_text(&self) -> Result<(), SurfaceError> {
        Ok(())
    }

    /// Returns clipboard text, defaulting to an empty string.
    fn get_text(&self) -> Result<String, SurfaceError> {
        Ok(String::new())
    }

    /// Replaces clipboard text, defaulting to a successful no-op.
    fn set_text(&self, _text: String) -> Result<(), SurfaceError> {
        Ok(())
    }
}

/// In-memory clipboard suitable for simple hosts and tests.
#[derive(Debug, Default)]
pub struct MemoryClipboard {
    text: RefCell<String>,
}

impl SurfaceClipboard for MemoryClipboard {
    fn clear_text(&self) -> Result<(), SurfaceError> {
        self.text.borrow_mut().clear();
        Ok(())
    }

    fn get_text(&self) -> Result<String, SurfaceError> {
        Ok(self.text.borrow().clone())
    }

    fn set_text(&self, text: String) -> Result<(), SurfaceError> {
        self.text.replace(text);
        Ok(())
    }
}

/// Thread-safe callback used to wake the host event loop for Servo work or queued native results.
///
/// Schedule a later update on the owning thread; do not synchronously reenter the Runtime.
/// Wake hints may be coalesced or spurious. They do not replace animation pacing or native
/// URL/history sampling.
pub type SurfaceEventLoopWaker = Arc<dyn Fn() + Send + Sync + 'static>;

/// Event-loop, clipboard, and process-engine options supplied to [`SurfaceHost`].
#[derive(Clone)]
pub struct SurfaceHostOptions {
    event_loop_waker: SurfaceEventLoopWaker,
    clipboard: Rc<dyn SurfaceClipboard>,
    config_directory: Option<PathBuf>,
}

impl SurfaceHostOptions {
    /// Creates host options from an event-loop waker and clipboard service.
    pub fn new(
        event_loop_waker: SurfaceEventLoopWaker,
        clipboard: Rc<dyn SurfaceClipboard>,
    ) -> Self {
        Self {
            event_loop_waker,
            clipboard,
            config_directory: None,
        }
    }

    /// Sets Servo's process-wide config directory for persistent website data.
    ///
    /// The first initialized Servo engine fixes this directory for the process.
    /// Later hosts must request the same value.
    pub fn with_config_directory(mut self, config_directory: impl Into<PathBuf>) -> Self {
        self.config_directory = Some(config_directory.into());
        self
    }

    /// Borrows the event-loop wake callback for Servo work and macOS system-view results.
    pub fn event_loop_waker(&self) -> &SurfaceEventLoopWaker {
        &self.event_loop_waker
    }

    /// Clones the reference-counted clipboard service.
    pub fn clipboard(&self) -> Rc<dyn SurfaceClipboard> {
        self.clipboard.clone()
    }

    /// Returns Servo's configured process-wide config directory, if any.
    pub fn config_directory(&self) -> Option<&Path> {
        self.config_directory.as_deref()
    }
}

impl Default for SurfaceHostOptions {
    fn default() -> Self {
        Self::new(Arc::new(|| {}), Rc::new(MemoryClipboard::default()))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct AttachedSurface {
    surface: HostSurface,
    viewport: SurfaceViewport,
}

#[derive(Clone)]
struct SurfaceRuntimeBinding {
    webview: WebViewHandle,
    session: SessionHandle,
    registrar: WebViewRegistrar,
    adoptions: std::rc::Weak<RefCell<Vec<AdoptedSurfaceWebView>>>,
    navigation_state_events: bool,
}

struct AdoptedSurfaceWebView {
    webview: WebViewHandle,
    session: SessionHandle,
    driver: Box<dyn ServoWebViewDriver>,
}

type NewWebViewHandler = Rc<dyn Fn(&NewWebViewRequest) -> NewWebViewDecision>;

trait ServoWebViewFactory {
    fn shutdown(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn perform_updates(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn create(
        &self,
        target: RenderTarget<'_>,
        surface: HostSurface,
        viewport: SurfaceViewport,
        options: &SurfaceHostOptions,
        initial_url: Option<&str>,
    ) -> Result<Box<dyn ServoWebViewDriver>, HostError>;
}

trait ServoWebViewDriver {
    fn bind_runtime(
        &mut self,
        _binding: SurfaceRuntimeBinding,
        _options: &SurfaceHostOptions,
    ) -> Result<(), HostError> {
        Ok(())
    }

    fn set_new_webview_handler(
        &mut self,
        _handler: Option<NewWebViewHandler>,
    ) -> Result<(), HostError> {
        Err(HostError::new(
            "native child creation is unsupported by this driver",
        ))
    }

    fn is_live(&self) -> bool {
        true
    }

    fn can_dispatch_unattached(&self) -> bool {
        false
    }

    fn before_update(
        &mut self,
        _services: &mut dyn SurfaceHostServices,
        _webview: WebViewHandle,
    ) -> Result<(), HostError> {
        Ok(())
    }

    fn present_pending(
        &mut self,
        services: &mut dyn SurfaceHostServices,
        webview: WebViewHandle,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.perform_updates(services, webview)
    }

    fn attach(
        &mut self,
        target: RenderTarget<'_>,
        surface: HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<(), HostError>;

    fn update_viewport(
        &mut self,
        target: RenderTarget<'_>,
        viewport: SurfaceViewport,
    ) -> Result<(), HostError>;

    fn detach(&mut self) -> Result<Vec<HostEvent>, HostError>;

    fn dispatch_command(&mut self, command: WebViewCommand) -> Result<(), HostError>;

    fn resolve_navigation_request(
        &mut self,
        navigation_id: &str,
        allow: bool,
    ) -> Result<(), HostError>;

    fn resolve_simple_dialog(
        &mut self,
        dialog_id: &str,
        confirmed: bool,
        prompt_value: Option<&str>,
    ) -> Result<(), HostError>;

    fn resolve_context_menu(
        &mut self,
        context_menu_id: &str,
        action: ContextMenuAction,
    ) -> Result<(), HostError>;

    fn dismiss_context_menu(&mut self, context_menu_id: &str) -> Result<(), HostError>;

    fn evaluate_javascript(&mut self, evaluation_id: &str, script: &str) -> Result<(), HostError>;

    fn dispatch_input_event(&mut self, event: HostInputEvent) -> Result<(), HostError>;

    fn perform_updates(
        &mut self,
        services: &mut dyn SurfaceHostServices,
        webview: WebViewHandle,
    ) -> Result<Vec<HostEvent>, HostError>;
}

enum PendingSurfaceCommand {
    WebView(WebViewCommand),
    EvaluateJavaScript {
        evaluation_id: String,
        script: String,
    },
}

fn queued_initial_url(
    commands: &VecDeque<PendingSurfaceCommand>,
    is_first_surface_construction: bool,
) -> Option<String> {
    if !is_first_surface_construction {
        return None;
    }
    match commands.front() {
        Some(PendingSurfaceCommand::WebView(WebViewCommand::LoadUrl(request))) => {
            Some(request.url.clone())
        }
        _ => None,
    }
}

struct AttachRollback<'a> {
    surface_webviews: &'a mut HashMap<WebViewHandle, Box<dyn ServoWebViewDriver>>,
    poisoned_webviews: &'a mut HashSet<WebViewHandle>,
    pending_events: &'a mut HashMap<WebViewHandle, VecDeque<HostEvent>>,
    webview: WebViewHandle,
    surface_webview: Option<Box<dyn ServoWebViewDriver>>,
    may_be_attached: bool,
}

impl AttachRollback<'_> {
    fn surface_webview(&mut self) -> &mut dyn ServoWebViewDriver {
        self.surface_webview
            .as_deref_mut()
            .expect("attach rollback owns the surface webview")
    }

    fn commit(mut self) -> Box<dyn ServoWebViewDriver> {
        self.may_be_attached = false;
        self.surface_webview
            .take()
            .expect("attach rollback owns the surface webview")
    }

    fn flush<S: SurfaceHostServices>(
        &mut self,
        services: &mut S,
        commands: &mut VecDeque<PendingSurfaceCommand>,
        promoted_initial_load: bool,
    ) -> Result<(), HostError> {
        let events = self.pending_events.entry(self.webview).or_default();
        flush_attached_webview(
            self.surface_webview
                .as_deref_mut()
                .expect("attach rollback owns the surface webview"),
            services,
            self.webview,
            commands,
            events,
            promoted_initial_load,
        )
    }

    fn rollback(mut self) -> Result<(), HostError> {
        self.restore()
    }

    fn restore(&mut self) -> Result<(), HostError> {
        let Some(mut surface_webview) = self.surface_webview.take() else {
            return Ok(());
        };
        if !surface_webview.is_live() {
            self.surface_webviews.insert(self.webview, surface_webview);
            return Ok(());
        }
        let detach_result = if self.may_be_attached {
            catch_unwind(AssertUnwindSafe(|| surface_webview.detach()))
        } else {
            Ok(Ok(Vec::new()))
        };

        match detach_result {
            Ok(Ok(events)) => {
                self.pending_events
                    .entry(self.webview)
                    .or_default()
                    .extend(events);
                self.surface_webviews.insert(self.webview, surface_webview);
                Ok(())
            }
            Ok(Err(_)) | Err(_) => {
                self.poisoned_webviews.insert(self.webview);
                drop_surface_webview(surface_webview);
                Err(poisoned_webview_error(self.webview))
            }
        }
    }
}

impl Drop for AttachRollback<'_> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn flush_attached_webview<S: SurfaceHostServices>(
    surface_webview: &mut dyn ServoWebViewDriver,
    services: &mut S,
    webview: WebViewHandle,
    commands: &mut VecDeque<PendingSurfaceCommand>,
    events: &mut VecDeque<HostEvent>,
    promoted_initial_load: bool,
) -> Result<(), HostError> {
    if promoted_initial_load {
        events.extend(surface_webview.perform_updates(services, webview)?);
    }
    while let Some(command) = commands.pop_front() {
        let dispatch_result = match &command {
            PendingSurfaceCommand::WebView(command) => {
                surface_webview.dispatch_command(command.clone())
            }
            PendingSurfaceCommand::EvaluateJavaScript {
                evaluation_id,
                script,
            } => surface_webview.evaluate_javascript(evaluation_id, script),
        };
        if let Err(error) = dispatch_result {
            commands.push_front(command);
            return Err(error);
        }
        events.extend(surface_webview.perform_updates(services, webview)?);
    }
    events.extend(surface_webview.perform_updates(services, webview)?);
    Ok(())
}

fn poisoned_webview_error(webview: WebViewHandle) -> HostError {
    HostError::new(format!(
        "webview {} is terminally unavailable after surface state recovery failed",
        webview.raw()
    ))
}

fn unregistered_child_error(child_webview_id: &str) -> HostError {
    HostError::new(format!(
        "managed child webview {child_webview_id} is not registered"
    ))
}

fn drop_surface_webview(surface_webview: Box<dyn ServoWebViewDriver>) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(surface_webview))) {
        // Preserve terminal cleanup even if a custom panic payload would panic again in Drop.
        mem::forget(payload);
    }
}

#[derive(Default)]
struct SurfaceWebViewFactory {
    runtime: RefCell<Option<ServoRuntime>>,
}

impl ServoWebViewFactory for SurfaceWebViewFactory {
    fn shutdown(&self) -> Result<(), HostError> {
        let runtime = self.runtime.borrow_mut().take();
        if let Some(runtime) = runtime {
            runtime.shutdown().map_err(HostError::new)?;
        } else {
            ServoRuntime::shutdown_retained().map_err(HostError::new)?;
        }
        Ok(())
    }

    fn perform_updates(&self) -> Result<(), HostError> {
        let runtime = self.runtime.borrow().clone();
        if let Some(runtime) = runtime {
            runtime.perform_updates().map_err(HostError::new)?;
        }
        Ok(())
    }

    fn create(
        &self,
        target: RenderTarget<'_>,
        surface: HostSurface,
        viewport: SurfaceViewport,
        options: &SurfaceHostOptions,
        initial_url: Option<&str>,
    ) -> Result<Box<dyn ServoWebViewDriver>, HostError> {
        Ok(Box::new(SurfaceWebView::new(
            target,
            surface,
            viewport,
            options,
            initial_url,
            &self.runtime,
        )?))
    }
}

/// A feature-gated Servo surface host implementation behind the public `explorerkit` facade.
///
/// Use this as `explorerkit::runtime::Runtime<explorerkit::surface::SurfaceHost<_>>` from app-owned
/// window or layout frameworks. Browser commands, host input, update pumping, and event draining
/// stay on `explorerkit::runtime::Runtime`; this host supplies the Servo
/// rendering/delegate/embedder-control glue.
pub struct SurfaceHost<S> {
    services: S,
    options: SurfaceHostOptions,
    webview_factory: Rc<dyn ServoWebViewFactory>,
    created_webviews: HashMap<WebViewHandle, SessionHandle>,
    attached_surfaces: HashMap<WebViewHandle, AttachedSurface>,
    surface_webviews: HashMap<WebViewHandle, Box<dyn ServoWebViewDriver>>,
    poisoned_webviews: HashSet<WebViewHandle>,
    pending_commands: HashMap<WebViewHandle, VecDeque<PendingSurfaceCommand>>,
    pending_events: HashMap<WebViewHandle, VecDeque<HostEvent>>,
    calls: Vec<HostCall>,
    registrar: Option<WebViewRegistrar>,
    adoptions: Rc<RefCell<Vec<AdoptedSurfaceWebView>>>,
    new_webview_handlers: HashMap<WebViewHandle, NewWebViewHandler>,
    navigation_state_events: bool,
}

impl<S> SurfaceHost<S> {
    /// Creates a Servo surface host with explicit host services and options.
    pub fn new(services: S, options: SurfaceHostOptions) -> Self {
        Self::with_webview_factory(services, options, Rc::new(SurfaceWebViewFactory::default()))
    }

    fn with_webview_factory(
        services: S,
        options: SurfaceHostOptions,
        webview_factory: Rc<dyn ServoWebViewFactory>,
    ) -> Self {
        Self {
            services,
            options,
            webview_factory,
            created_webviews: HashMap::new(),
            attached_surfaces: HashMap::new(),
            surface_webviews: HashMap::new(),
            poisoned_webviews: HashSet::new(),
            pending_commands: HashMap::new(),
            pending_events: HashMap::new(),
            calls: Vec::new(),
            registrar: None,
            adoptions: Rc::default(),
            new_webview_handlers: HashMap::new(),
            navigation_state_events: false,
        }
    }

    /// Creates a Servo surface host with default wake and clipboard services.
    pub fn with_default_options(services: S) -> Self {
        Self::new(services, SurfaceHostOptions::default())
    }

    /// Borrows the app-provided surface services.
    pub fn services(&self) -> &S {
        &self.services
    }

    /// Mutably borrows the app-provided surface services.
    pub fn services_mut(&mut self) -> &mut S {
        &mut self.services
    }

    /// Returns the host calls observed by this surface host.
    pub fn calls(&self) -> &[HostCall] {
        &self.calls
    }

    pub(crate) fn set_navigation_state_events(&mut self) {
        self.navigation_state_events = true;
    }

    pub(crate) fn native_view_handles(&mut self) -> Vec<WebViewHandle> {
        self.sync_native_views();
        self.created_webviews.keys().copied().collect()
    }

    fn sync_native_views(&mut self) {
        let adoptions = self.adoptions.borrow_mut().drain(..).collect::<Vec<_>>();
        for adopted in adoptions {
            self.created_webviews
                .insert(adopted.webview, adopted.session);
            self.surface_webviews
                .insert(adopted.webview, adopted.driver);
        }
        let retired = self
            .surface_webviews
            .iter()
            .filter(|(_, view)| !view.is_live())
            .map(|(handle, _)| *handle)
            .collect::<Vec<_>>();
        for handle in retired {
            self.surface_webviews.remove(&handle);
            self.created_webviews.remove(&handle);
            self.attached_surfaces.remove(&handle);
            self.pending_commands.remove(&handle);
            self.pending_events.remove(&handle);
            self.new_webview_handlers.remove(&handle);
        }
    }

    fn ensure_created_webview(&mut self, webview: WebViewHandle) -> Result<(), HostError> {
        self.sync_native_views();
        if !self.created_webviews.contains_key(&webview) {
            return Err(HostError::new(format!(
                "webview {} is not registered",
                webview.raw()
            )));
        }
        if self.poisoned_webviews.contains(&webview) {
            return Err(poisoned_webview_error(webview));
        }
        Ok(())
    }
}

impl<S> Drop for SurfaceHost<S> {
    fn drop(&mut self) {
        // Renderer targets must be detached before host-owned native surface services drop.
        self.surface_webviews.clear();
        self.adoptions.borrow_mut().clear();
    }
}

#[allow(private_bounds)]
impl<S: SurfaceHostServices> SurfaceHost<S> {
    fn poison_surface_webview(
        &mut self,
        webview: WebViewHandle,
        surface_webview: Option<Box<dyn ServoWebViewDriver>>,
    ) -> HostError {
        self.attached_surfaces.remove(&webview);
        self.poisoned_webviews.insert(webview);
        if let Some(surface_webview) = surface_webview {
            drop_surface_webview(surface_webview);
        }
        poisoned_webview_error(webview)
    }

    fn update_attached_viewport(
        &mut self,
        webview: WebViewHandle,
        viewport: SurfaceViewport,
        call: HostCall,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        let previous = self
            .attached_surfaces
            .get(&webview)
            .cloned()
            .ok_or_else(|| {
                HostError::new(format!(
                    "webview {} does not have an attached surface",
                    webview.raw()
                ))
            })?;

        let Some(mut surface_webview) = self.surface_webviews.remove(&webview) else {
            return Err(self.poison_surface_webview(webview, None));
        };

        let update_result = catch_unwind(AssertUnwindSafe(|| {
            let target = self
                .services
                .update_render_target(&previous.surface, viewport)
                .map_err(|error| HostError::new(error.to_string()))?;
            surface_webview
                .update_viewport(target, viewport)
                .and_then(|()| surface_webview.perform_updates(&mut self.services, webview))
        }));

        let result = match update_result {
            Ok(Ok(events)) => {
                self.attached_surfaces.insert(
                    webview,
                    AttachedSurface {
                        surface: previous.surface,
                        viewport,
                    },
                );
                self.calls.push(call);
                self.surface_webviews.insert(webview, surface_webview);
                Ok(events)
            }
            Ok(Err(error)) if !surface_webview.is_live() => {
                self.surface_webviews.insert(webview, surface_webview);
                Err(error)
            }
            Ok(Err(error)) => {
                let rollback = catch_unwind(AssertUnwindSafe(|| {
                    let previous_target = self
                        .services
                        .update_render_target(&previous.surface, previous.viewport)
                        .map_err(|error| HostError::new(error.to_string()))?;
                    surface_webview.update_viewport(previous_target, previous.viewport)
                }));
                match rollback {
                    Ok(Ok(())) => {
                        self.surface_webviews.insert(webview, surface_webview);
                        Err(error)
                    }
                    Ok(Err(_)) => Err(self.poison_surface_webview(webview, Some(surface_webview))),
                    Err(payload) => {
                        self.poison_surface_webview(webview, Some(surface_webview));
                        resume_unwind(payload);
                    }
                }
            }
            Err(payload) => {
                self.poison_surface_webview(webview, Some(surface_webview));
                resume_unwind(payload);
            }
        };
        self.sync_native_views();
        result
    }

    fn dispatch_controller_response(
        &mut self,
        webview: WebViewHandle,
        respond: impl FnOnce(&mut dyn ServoWebViewDriver) -> Result<(), HostError>,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        let Some(surface_webview) = self.surface_webviews.get_mut(&webview) else {
            return Ok(Vec::new());
        };

        respond(surface_webview.as_mut())?;
        self.perform_updates(webview)
    }
}

impl<S: SurfaceHostServices> Host for SurfaceHost<S> {
    fn set_webview_registrar(&mut self, registrar: WebViewRegistrar) {
        self.registrar = Some(registrar);
    }

    fn set_new_webview_handler(
        &mut self,
        webview: WebViewHandle,
        handler: Option<NewWebViewHandler>,
    ) -> Result<(), HostError> {
        self.ensure_created_webview(webview)?;
        if !cfg!(target_os = "macos") || self.registrar.is_none() {
            return Err(HostError::new(
                "registered native child creation requires the macOS exportable renderer",
            ));
        }
        if let Some(view) = self.surface_webviews.get_mut(&webview) {
            return view.set_new_webview_handler(handler);
        }
        match handler {
            Some(handler) => {
                self.new_webview_handlers.insert(webview, handler);
            }
            None => {
                self.new_webview_handlers.remove(&webview);
            }
        }
        Ok(())
    }

    fn set_popup_policy(
        &mut self,
        webview: WebViewHandle,
        policy: PopupRequestPolicy,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        if policy == PopupRequestPolicy::ManagedChild {
            return Err(HostError::new(
                "registered child creation requires a per-request decision handler",
            ));
        }
        self.new_webview_handlers.remove(&webview);
        if cfg!(target_os = "macos") && self.registrar.is_some() {
            if let Some(view) = self.surface_webviews.get_mut(&webview) {
                view.set_new_webview_handler(None)?;
            }
        }
        Ok(Vec::new())
    }

    fn attach_managed_child_surface(
        &mut self,
        _: WebViewHandle,
        child_webview_id: &str,
        _: &HostSurface,
        _: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        Err(unregistered_child_error(child_webview_id))
    }

    fn resize_managed_child_surface(
        &mut self,
        _: WebViewHandle,
        child_webview_id: &str,
        _: SurfaceSize,
    ) -> Result<Vec<HostEvent>, HostError> {
        Err(unregistered_child_error(child_webview_id))
    }

    fn update_managed_child_surface_viewport(
        &mut self,
        _: WebViewHandle,
        child_webview_id: &str,
        _: SurfaceViewport,
        _: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        Err(unregistered_child_error(child_webview_id))
    }

    fn detach_managed_child_surface(
        &mut self,
        _: WebViewHandle,
        child_webview_id: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        Err(unregistered_child_error(child_webview_id))
    }

    fn perform_managed_child_updates(
        &mut self,
        _: WebViewHandle,
        child_webview_id: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        Err(unregistered_child_error(child_webview_id))
    }

    fn shutdown(&mut self) -> Result<(), HostError> {
        // Finalization must also retire views left by a failed individual detach.
        // Drop their renderers while native services still live, then retire the engine.
        for (_, view) in self.surface_webviews.drain() {
            drop_surface_webview(view);
        }
        self.attached_surfaces.clear();
        self.pending_commands.clear();
        self.pending_events.clear();
        self.poisoned_webviews.clear();
        self.created_webviews.clear();
        self.adoptions.borrow_mut().clear();
        self.new_webview_handlers.clear();
        self.webview_factory.shutdown()
    }

    fn destroy_webview(&mut self, webview: WebViewHandle) -> Result<(), HostError> {
        self.sync_native_views();
        if !self.created_webviews.contains_key(&webview) {
            return Err(HostError::new(format!(
                "webview {} is not registered",
                webview.raw()
            )));
        }
        // Keep the view and its handles valid if detachment fails, so callers can retry.
        if let Some(view) = self.surface_webviews.get_mut(&webview) {
            view.detach()?;
        }
        self.surface_webviews.remove(&webview);
        self.attached_surfaces.remove(&webview);
        self.pending_commands.remove(&webview);
        self.pending_events.remove(&webview);
        self.poisoned_webviews.remove(&webview);
        self.created_webviews.remove(&webview);
        self.new_webview_handlers.remove(&webview);
        self.calls.push(HostCall::DestroyWebView { webview });
        Ok(())
    }

    fn perform_all_updates(
        &mut self,
        webviews: &[WebViewHandle],
    ) -> Result<Vec<(WebViewHandle, Result<Vec<HostEvent>, HostError>)>, HostError> {
        let mut results = Vec::with_capacity(webviews.len());
        for &webview in webviews {
            let prepared = self.ensure_created_webview(webview).and_then(|()| {
                match self.surface_webviews.get_mut(&webview) {
                    Some(view) => view.before_update(&mut self.services, webview),
                    None => Ok(()),
                }
            });
            results.push((webview, prepared.map(|()| Vec::new())));
        }
        let pump_result = self.webview_factory.perform_updates();
        self.sync_native_views();
        for (webview, result) in &mut results {
            if result.is_ok() {
                if let Some(view) = self.surface_webviews.get_mut(webview) {
                    *result = view.present_pending(&mut self.services, *webview);
                }
                self.calls
                    .push(HostCall::PerformUpdates { webview: *webview });
            }
        }
        self.sync_native_views();
        pump_result?;
        Ok(results)
    }

    fn create_webview(
        &mut self,
        session: SessionHandle,
        webview: WebViewHandle,
    ) -> Result<(), HostError> {
        if self.created_webviews.contains_key(&webview) {
            return Err(HostError::new(format!(
                "webview {} is already registered",
                webview.raw()
            )));
        }

        self.created_webviews.insert(webview, session);
        self.pending_commands.entry(webview).or_default();
        self.pending_events.entry(webview).or_default();
        self.calls
            .push(HostCall::CreateWebView { session, webview });
        Ok(())
    }

    fn attach_surface(
        &mut self,
        webview: WebViewHandle,
        surface: &HostSurface,
        size: SurfaceSize,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.attach_surface_with_viewport(webview, surface, SurfaceViewport::new(size, 1.0))
    }

    fn attach_surface_with_viewport(
        &mut self,
        webview: WebViewHandle,
        surface: &HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        if self.attached_surfaces.contains_key(&webview) {
            return Err(HostError::new(format!(
                "webview {} already has an attached surface",
                webview.raw()
            )));
        }

        let target = self
            .services
            .render_target(surface, viewport)
            .map_err(|error| HostError::new(error.to_string()))?;
        let existing_webview = self.surface_webviews.remove(&webview);
        let was_existing = existing_webview.is_some();
        let promoted_initial_url = queued_initial_url(
            self.pending_commands.entry(webview).or_default(),
            !was_existing,
        );
        let mut rollback = AttachRollback {
            surface_webviews: &mut self.surface_webviews,
            poisoned_webviews: &mut self.poisoned_webviews,
            pending_events: &mut self.pending_events,
            webview,
            surface_webview: existing_webview,
            may_be_attached: false,
        };
        let attach_result = if was_existing {
            rollback.may_be_attached = true;
            rollback
                .surface_webview()
                .attach(target, surface.clone(), viewport)
        } else {
            match self.webview_factory.create(
                target,
                surface.clone(),
                viewport,
                &self.options,
                promoted_initial_url.as_deref(),
            ) {
                Ok(surface_webview) => {
                    rollback.surface_webview = Some(surface_webview);
                    rollback.may_be_attached = true;
                    let configure = if let Some(registrar) = self.registrar.clone() {
                        rollback.surface_webview().bind_runtime(
                            SurfaceRuntimeBinding {
                                webview,
                                session: self.created_webviews[&webview],
                                registrar,
                                adoptions: Rc::downgrade(&self.adoptions),
                                navigation_state_events: self.navigation_state_events,
                            },
                            &self.options,
                        )
                    } else {
                        Ok(())
                    };
                    configure.and_then(|()| {
                        if let Some(handler) = self.new_webview_handlers.remove(&webview) {
                            rollback
                                .surface_webview()
                                .set_new_webview_handler(Some(handler))?;
                        }
                        Ok(())
                    })
                }
                Err(error) => Err(error),
            }
        };
        if let Err(error) = attach_result {
            let result = match rollback.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(rollback_error),
            };
            self.sync_native_views();
            return result;
        }

        let did_promote_initial_load = promoted_initial_url.is_some();
        if did_promote_initial_load {
            self.pending_commands
                .entry(webview)
                .or_default()
                .pop_front();
        }
        let flush_result = rollback.flush(
            &mut self.services,
            self.pending_commands.entry(webview).or_default(),
            did_promote_initial_load,
        );

        let result = match flush_result {
            Ok(()) => {
                let surface_webview = rollback.commit();
                self.attached_surfaces.insert(
                    webview,
                    AttachedSurface {
                        surface: surface.clone(),
                        viewport,
                    },
                );
                self.calls.push(HostCall::AttachSurfaceWithViewport {
                    webview,
                    surface: surface.clone(),
                    viewport,
                });
                self.surface_webviews.insert(webview, surface_webview);
                Ok(self
                    .pending_events
                    .entry(webview)
                    .or_default()
                    .drain(..)
                    .collect())
            }
            Err(error) => match rollback.rollback() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(rollback_error),
            },
        };
        self.sync_native_views();
        result
    }

    fn resize_surface(
        &mut self,
        webview: WebViewHandle,
        size: SurfaceSize,
    ) -> Result<Vec<HostEvent>, HostError> {
        let Some(attached_surface) = self.attached_surfaces.get(&webview) else {
            self.ensure_created_webview(webview)?;
            return Err(HostError::new(format!(
                "webview {} does not have an attached surface",
                webview.raw()
            )));
        };
        self.update_attached_viewport(
            webview,
            attached_surface.viewport.with_size(size),
            HostCall::ResizeSurface { webview, size },
        )
    }

    fn set_surface_scale_factor(
        &mut self,
        webview: WebViewHandle,
        scale_factor: f32,
    ) -> Result<Vec<HostEvent>, HostError> {
        let Some(attached_surface) = self.attached_surfaces.get(&webview) else {
            self.ensure_created_webview(webview)?;
            return Err(HostError::new(format!(
                "webview {} does not have an attached surface",
                webview.raw()
            )));
        };
        self.update_attached_viewport(
            webview,
            attached_surface.viewport.with_scale_factor(scale_factor),
            HostCall::SetSurfaceScaleFactor {
                webview,
                scale_factor,
            },
        )
    }

    fn update_surface_viewport(
        &mut self,
        webview: WebViewHandle,
        viewport: SurfaceViewport,
        _previous_viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.update_attached_viewport(
            webview,
            viewport,
            HostCall::UpdateSurfaceViewport { webview, viewport },
        )
    }

    fn detach_surface(&mut self, webview: WebViewHandle) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        if !self.attached_surfaces.contains_key(&webview) {
            return Err(HostError::new(format!(
                "webview {} does not have an attached surface",
                webview.raw()
            )));
        }

        let events = if let Some(mut surface_webview) = self.surface_webviews.remove(&webview) {
            match catch_unwind(AssertUnwindSafe(|| surface_webview.detach())) {
                Ok(Ok(events)) => {
                    self.surface_webviews.insert(webview, surface_webview);
                    events
                }
                Ok(Err(error)) => {
                    self.surface_webviews.insert(webview, surface_webview);
                    return Err(error);
                }
                Err(_) => {
                    return Err(self.poison_surface_webview(webview, Some(surface_webview)));
                }
            }
        } else {
            Vec::new()
        };

        self.attached_surfaces.remove(&webview);
        self.calls.push(HostCall::DetachSurface { webview });
        Ok(events)
    }

    fn dispatch_webview_command(
        &mut self,
        webview: WebViewHandle,
        command: &WebViewCommand,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;

        if !self.attached_surfaces.contains_key(&webview)
            && !self
                .surface_webviews
                .get(&webview)
                .is_some_and(|view| view.can_dispatch_unattached())
        {
            self.pending_commands
                .entry(webview)
                .or_default()
                .push_back(PendingSurfaceCommand::WebView(command.clone()));
            self.calls.push(HostCall::DispatchWebViewCommand {
                webview,
                command: command.clone(),
            });
            return Ok(Vec::new());
        }
        let Some(surface_webview) = self.surface_webviews.get_mut(&webview) else {
            return Err(HostError::new(format!(
                "webview {} does not have an attached Servo webview",
                webview.raw()
            )));
        };

        surface_webview.dispatch_command(command.clone())?;
        let events = self.perform_updates(webview)?;
        self.calls.push(HostCall::DispatchWebViewCommand {
            webview,
            command: command.clone(),
        });
        Ok(events)
    }

    fn resolve_navigation_request(
        &mut self,
        webview: WebViewHandle,
        navigation_id: &str,
        allow: bool,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.dispatch_controller_response(webview, |surface_webview| {
            surface_webview.resolve_navigation_request(navigation_id, allow)
        })
    }

    fn resolve_simple_dialog(
        &mut self,
        webview: WebViewHandle,
        dialog_id: &str,
        confirmed: bool,
        prompt_value: Option<&str>,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.dispatch_controller_response(webview, |surface_webview| {
            surface_webview.resolve_simple_dialog(dialog_id, confirmed, prompt_value)
        })
    }

    fn resolve_context_menu(
        &mut self,
        webview: WebViewHandle,
        context_menu_id: &str,
        action: ContextMenuAction,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.dispatch_controller_response(webview, |surface_webview| {
            surface_webview.resolve_context_menu(context_menu_id, action)
        })
    }

    fn dismiss_context_menu(
        &mut self,
        webview: WebViewHandle,
        context_menu_id: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.dispatch_controller_response(webview, |surface_webview| {
            surface_webview.dismiss_context_menu(context_menu_id)
        })
    }

    fn evaluate_javascript(
        &mut self,
        webview: WebViewHandle,
        evaluation_id: &str,
        script: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        if !self.attached_surfaces.contains_key(&webview)
            && !self
                .surface_webviews
                .get(&webview)
                .is_some_and(|view| view.can_dispatch_unattached())
        {
            self.pending_commands.entry(webview).or_default().push_back(
                PendingSurfaceCommand::EvaluateJavaScript {
                    evaluation_id: evaluation_id.to_owned(),
                    script: script.to_owned(),
                },
            );
            return Ok(Vec::new());
        }
        let Some(surface_webview) = self.surface_webviews.get_mut(&webview) else {
            return Err(HostError::new(format!(
                "webview {} does not have an attached Servo webview",
                webview.raw()
            )));
        };

        surface_webview.evaluate_javascript(evaluation_id, script)?;
        self.perform_updates(webview)
    }

    fn dispatch_input_event(
        &mut self,
        webview: WebViewHandle,
        event: &HostInputEvent,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        let Some(surface_webview) = self.surface_webviews.get_mut(&webview) else {
            return Ok(Vec::new());
        };

        surface_webview.dispatch_input_event(event.clone())?;
        self.perform_updates(webview)
    }

    fn perform_updates(&mut self, webview: WebViewHandle) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        let Some(surface_webview) = self.surface_webviews.get_mut(&webview) else {
            self.calls.push(HostCall::PerformUpdates { webview });
            return Ok(Vec::new());
        };
        let result = surface_webview.perform_updates(&mut self.services, webview);
        self.sync_native_views();
        self.calls.push(HostCall::PerformUpdates { webview });
        result
    }

    fn observe_webview_events(
        &mut self,
        webview: WebViewHandle,
        _events: &[HostEvent],
    ) -> Result<(), HostError> {
        self.ensure_created_webview(webview)
    }

    fn synthesize_webview_command_events(&self) -> bool {
        false
    }
}

enum ActiveRenderTarget {
    Native {
        rendering_context: Rc<WindowRenderingContext>,
    },
    LocalOffscreen {
        parent_context: Rc<WindowRenderingContext>,
        rendering_context: Rc<OffscreenRenderingContext>,
    },
    #[cfg(target_os = "macos")]
    ExportableOffscreen {
        rendering_context: Rc<ExportableRenderingContext>,
    },
}

impl ActiveRenderTarget {
    fn new(
        target: RenderTarget<'_>,
        viewport: SurfaceViewport,
        refresh_driver: Rc<SurfaceRefreshDriver>,
        options: &SurfaceHostOptions,
    ) -> Result<Self, HostError> {
        match target {
            RenderTarget::Native(native_surface) => {
                let rendering_context = Rc::new(create_rendering_context(
                    native_surface,
                    viewport.size,
                    refresh_driver,
                )?);
                Ok(Self::Native { rendering_context })
            }
            RenderTarget::Offscreen(offscreen_target) => {
                if let Some((parent_surface, parent_size)) = offscreen_target.__local_parent() {
                    let parent_context = Rc::new(create_rendering_context(
                        parent_surface,
                        parent_size,
                        refresh_driver,
                    )?);
                    let rendering_context =
                        Rc::new(parent_context.offscreen_context(physical_size(viewport.size)));
                    return Ok(Self::LocalOffscreen {
                        parent_context,
                        rendering_context,
                    });
                }
                #[cfg(target_os = "macos")]
                {
                    let rendering_context = Rc::new(
                        ExportableRenderingContext::new(
                            exportable_physical_size(viewport)?,
                            options.event_loop_waker.clone(),
                            refresh_driver,
                        )
                        .map_err(|error| HostError::new(error.to_string()))?,
                    );
                    Ok(Self::ExportableOffscreen { rendering_context })
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Err(HostError::new(
                        "exportable offscreen surfaces are only supported on macOS",
                    ))
                }
            }
            #[cfg(test)]
            RenderTarget::Test(_) => Err(HostError::new(
                "test render targets cannot create a real Servo rendering context",
            )),
        }
    }

    fn kind(&self) -> SurfaceMode {
        match self {
            Self::Native { .. } => SurfaceMode::Native,
            Self::LocalOffscreen { .. } => SurfaceMode::Offscreen,
            #[cfg(target_os = "macos")]
            Self::ExportableOffscreen { .. } => SurfaceMode::Offscreen,
        }
    }

    fn rendering_context(&self) -> Rc<dyn RenderingContext> {
        match self {
            Self::Native { rendering_context } => {
                let context: Rc<dyn RenderingContext> = rendering_context.clone();
                context
            }
            Self::LocalOffscreen {
                rendering_context, ..
            } => {
                let context: Rc<dyn RenderingContext> = rendering_context.clone();
                context
            }
            #[cfg(target_os = "macos")]
            Self::ExportableOffscreen { rendering_context } => {
                let context: Rc<dyn RenderingContext> = rendering_context.clone();
                context
            }
        }
    }

    fn webview_size(&self, viewport: SurfaceViewport) -> Result<SurfaceSize, HostError> {
        match self {
            #[cfg(target_os = "macos")]
            Self::ExportableOffscreen { .. } => exportable_physical_size(viewport),
            _ => Ok(viewport.size),
        }
    }

    fn can_paint(&self) -> Result<bool, HostError> {
        match self {
            #[cfg(target_os = "macos")]
            Self::ExportableOffscreen { rendering_context } => rendering_context
                .can_paint()
                .map_err(|error| HostError::new(error.to_string())),
            _ => Ok(true),
        }
    }

    #[cfg(target_os = "macos")]
    fn exportable_context(&self) -> Option<&ExportableRenderingContext> {
        match self {
            Self::ExportableOffscreen { rendering_context } => Some(rendering_context),
            _ => None,
        }
    }

    fn attach_or_update(
        &mut self,
        target: RenderTarget<'_>,
        viewport: SurfaceViewport,
        was_attached: bool,
    ) -> Result<(), HostError> {
        let current_kind = self.kind();
        match (self, target) {
            (Self::Native { rendering_context }, RenderTarget::Native(native_surface)) => {
                let size = physical_size(viewport.size);
                if !was_attached {
                    rendering_context
                        .set_window(native_surface.window_handle(), size)
                        .map_err(|error| HostError::new(format!("{error:?}")))?;
                }
                Ok(())
            }
            (
                Self::LocalOffscreen {
                    parent_context,
                    rendering_context: _,
                },
                RenderTarget::Offscreen(offscreen_target),
            ) => {
                let Some((parent_surface, parent_size)) = offscreen_target.__local_parent() else {
                    return Err(HostError::new(
                        "cannot change a local offscreen target to exportable storage",
                    ));
                };
                let parent_size = physical_size(parent_size);
                if !was_attached {
                    parent_context
                        .set_window(parent_surface.window_handle(), parent_size)
                        .map_err(|error| HostError::new(format!("{error:?}")))?;
                }
                parent_context.resize(parent_size);
                if parent_context.size() != parent_size {
                    parent_context
                        .set_window(parent_surface.window_handle(), parent_size)
                        .map_err(|error| HostError::new(format!("{error:?}")))?;
                    parent_context.resize(parent_size);
                }
                Ok(())
            }
            #[cfg(target_os = "macos")]
            (
                Self::ExportableOffscreen { rendering_context },
                RenderTarget::Offscreen(offscreen_target),
            ) if offscreen_target.__local_parent().is_none() => {
                if !was_attached {
                    rendering_context.attach();
                }
                Ok(())
            }
            (_, mismatched) => Err(HostError::new(format!(
                "cannot change Servo render target from {:?} to {:?}",
                current_kind,
                mismatched.kind()
            ))),
        }
    }

    fn detach(&self) -> Result<(), HostError> {
        let context = match self {
            Self::Native { rendering_context } => rendering_context,
            Self::LocalOffscreen { parent_context, .. } => parent_context,
            #[cfg(target_os = "macos")]
            Self::ExportableOffscreen { rendering_context } => {
                return rendering_context
                    .detach()
                    .map_err(|error| HostError::new(error.to_string()));
            }
        };
        // Surfman's AppKit unbind temporarily selects this context, then restores the
        // previous one before take_window destroys its GL objects. Keep this target
        // current throughout teardown so those deletes cannot affect a sibling.
        context
            .make_current()
            .map_err(|error| HostError::new(format!("{error:?}")))?;
        context
            .take_window()
            .map_err(|error| HostError::new(format!("{error:?}")))
    }

    fn prepare_resize(
        &self,
        target: RenderTarget<'_>,
        viewport: SurfaceViewport,
    ) -> Result<(), HostError> {
        #[cfg(not(target_os = "macos"))]
        let _ = (target, viewport);
        #[cfg(target_os = "macos")]
        if let (
            Self::ExportableOffscreen { rendering_context },
            RenderTarget::Offscreen(offscreen_target),
        ) = (self, target)
        {
            if offscreen_target.__local_parent().is_some() {
                return Err(HostError::new(
                    "cannot resize exportable storage as a local offscreen target",
                ));
            }
            rendering_context
                .prepare_resize_to(exportable_physical_size(viewport)?)
                .map_err(|error| HostError::new(error.to_string()))?;
        }
        Ok(())
    }

    fn resize(&self, target: RenderTarget<'_>, viewport: SurfaceViewport) -> Result<(), HostError> {
        let current_kind = self.kind();
        match (self, target) {
            (Self::Native { rendering_context }, RenderTarget::Native(native_surface)) => {
                let size = physical_size(viewport.size);
                if rendering_context.size() != size {
                    rendering_context
                        .set_window(native_surface.window_handle(), size)
                        .map_err(|error| HostError::new(format!("{error:?}")))?;
                    rendering_context.resize(size);
                }
                Ok(())
            }
            (
                Self::LocalOffscreen {
                    rendering_context, ..
                },
                RenderTarget::Offscreen(offscreen_target),
            ) => {
                if offscreen_target.__local_parent().is_none() {
                    return Err(HostError::new(
                        "cannot resize a local offscreen target as exportable storage",
                    ));
                }
                rendering_context.resize(physical_size(viewport.size));
                Ok(())
            }
            #[cfg(target_os = "macos")]
            (
                Self::ExportableOffscreen { rendering_context },
                RenderTarget::Offscreen(offscreen_target),
            ) if offscreen_target.__local_parent().is_none() => rendering_context
                .finish_resize_to(exportable_physical_size(viewport)?)
                .map_err(|error| HostError::new(error.to_string())),
            (_, mismatched) => Err(HostError::new(format!(
                "cannot resize Servo render target from {:?} with {:?}",
                current_kind,
                mismatched.kind()
            ))),
        }
    }
}

struct SurfaceWebView {
    refresh_driver: Rc<SurfaceRefreshDriver>,
    render_target: ActiveRenderTarget,
    webview: ServoWebView,
    pending_events: VecDeque<HostEvent>,
    surface: Option<HostSurface>,
    viewport: Option<SurfaceViewport>,
    surface_attached: bool,
    adopted_child: bool,
}

impl SurfaceWebView {
    fn new(
        target: RenderTarget<'_>,
        surface: HostSurface,
        viewport: SurfaceViewport,
        options: &SurfaceHostOptions,
        initial_url: Option<&str>,
        runtime: &RefCell<Option<ServoRuntime>>,
    ) -> Result<Self, HostError> {
        let refresh_driver = Rc::new(SurfaceRefreshDriver::default());
        let render_target =
            ActiveRenderTarget::new(target, viewport, refresh_driver.clone(), options)?;
        let rendering_context = render_target.rendering_context();
        let webview_size = render_target.webview_size(viewport)?;
        let existing_runtime = runtime.borrow().clone();
        let runtime_owner = if let Some(runtime) = existing_runtime {
            runtime
        } else {
            let event_loop_waker =
                Box::new(ServoEventLoopWaker::new(options.event_loop_waker.clone()));
            let created = match options.config_directory() {
                Some(config_directory) => ServoRuntime::with_config_directory(
                    event_loop_waker,
                    config_directory.to_owned(),
                ),
                None => ServoRuntime::new(event_loop_waker),
            }
            .map_err(HostError::new)?;
            *runtime.borrow_mut() = Some(created.clone());
            created
        };
        let mut webview = runtime_owner
            .create_webview(ServoWebViewInit {
                rendering_context,
                clipboard_delegate: Rc::new(ServoClipboardAdapter::new(options.clipboard())),
                initial_url: initial_url.map(str::to_owned),
                density: viewport.scale_factor,
                popup_policy: PopupRequestPolicy::DefaultDeny,
                managed_child_rendering_context_factory: None,
            })
            .map_err(HostError::new)?;
        webview.set_hidpi_scale_factor(viewport.scale_factor);
        webview.resize(webview_size);
        webview.request_paint();

        Ok(Self {
            refresh_driver,
            render_target,
            webview,
            pending_events: VecDeque::new(),
            surface: Some(surface),
            viewport: Some(viewport),
            surface_attached: true,
            adopted_child: false,
        })
    }

    fn resize(
        &mut self,
        target: RenderTarget<'_>,
        viewport: SurfaceViewport,
    ) -> Result<(), HostError> {
        let webview_size = self.render_target.webview_size(viewport)?;
        self.render_target.prepare_resize(target, viewport)?;
        self.webview.set_hidpi_scale_factor(viewport.scale_factor);
        // Servo skips its layout/window-size update if the rendering context
        // already has this size before WebView::resize runs.
        self.webview.resize(webview_size);
        self.render_target.resize(target, viewport)?;
        self.webview.request_paint();
        self.viewport = Some(viewport);
        Ok(())
    }
}

impl ServoWebViewDriver for SurfaceWebView {
    fn bind_runtime(
        &mut self,
        binding: SurfaceRuntimeBinding,
        options: &SurfaceHostOptions,
    ) -> Result<(), HostError> {
        self.webview.__bind_runtime(
            binding.webview,
            binding.registrar.clone(),
            binding.navigation_state_events,
        );
        #[cfg(target_os = "macos")]
        {
            let options = options.clone();
            let factory =
                self.webview
                    .__child_factory(Rc::new(move |child, source, parent, request_id| {
                        let adoptions = binding.adoptions.upgrade().ok_or_else(|| {
                            "native child surface host has been dropped".to_owned()
                        })?;
                        let reservation = binding
                            .registrar
                            .reserve_child(source, request_id.to_owned())
                            .map_err(|error| error.to_string())?;
                        let webview = reservation.webview();
                        let refresh_driver = Rc::new(SurfaceRefreshDriver::default());
                        let initial_size = parent.rendering_context().size();
                        let context = Rc::new(
                            ExportableRenderingContext::new(
                                SurfaceSize::new(initial_size.width, initial_size.height),
                                options.event_loop_waker.clone(),
                                refresh_driver.clone(),
                            )
                            .map_err(|error| error.to_string())?,
                        );
                        let render_target = ActiveRenderTarget::ExportableOffscreen {
                            rendering_context: context.clone(),
                        };
                        child.__bind_runtime(
                            webview,
                            binding.registrar.clone(),
                            binding.navigation_state_events,
                        );
                        let parent_webview_id = parent.id().to_string();
                        let parent_url = parent.url().map(|url| url.to_string());
                        let request_id = request_id.to_owned();
                        let session = binding.session;
                        Ok(child.__prepare_child(
                            context,
                            Box::new(move |child, child_webview_id| {
                                let driver = Box::new(SurfaceWebView {
                                    refresh_driver,
                                    render_target,
                                    webview: child,
                                    pending_events: VecDeque::new(),
                                    surface: None,
                                    viewport: None,
                                    surface_attached: false,
                                    adopted_child: true,
                                });
                                adoptions.borrow_mut().push(AdoptedSurfaceWebView {
                                    webview,
                                    session,
                                    driver,
                                });
                                reservation.commit(PopupCreated {
                                    request_id,
                                    parent_webview_id,
                                    child_webview_id,
                                    parent_url,
                                    target_url: None,
                                    window_features: None,
                                    wants_popup: None,
                                });
                            }),
                        ))
                    }));
            self.webview
                .set_managed_child_webview_factory(factory)
                .map_err(HostError::new)?;
        }
        #[cfg(not(target_os = "macos"))]
        let _ = options;
        Ok(())
    }

    fn set_new_webview_handler(
        &mut self,
        handler: Option<NewWebViewHandler>,
    ) -> Result<(), HostError> {
        self.webview
            .__set_new_webview_handler(handler)
            .map_err(HostError::new)
    }

    fn is_live(&self) -> bool {
        self.webview.__is_live()
    }

    fn can_dispatch_unattached(&self) -> bool {
        self.adopted_child
    }

    fn attach(
        &mut self,
        target: RenderTarget<'_>,
        surface: HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<(), HostError> {
        self.render_target
            .attach_or_update(target, viewport, self.surface_attached)?;
        self.surface_attached = true;
        self.surface = Some(surface);
        self.resize(target, viewport)?;
        Ok(())
    }

    fn update_viewport(
        &mut self,
        target: RenderTarget<'_>,
        viewport: SurfaceViewport,
    ) -> Result<(), HostError> {
        self.render_target
            .attach_or_update(target, viewport, self.surface_attached)?;
        self.resize(target, viewport)?;
        Ok(())
    }

    fn detach(&mut self) -> Result<Vec<HostEvent>, HostError> {
        if !self.surface_attached {
            return Ok(Vec::new());
        }

        self.render_target.detach()?;
        self.surface_attached = false;
        self.surface = None;
        let mut events: Vec<_> = self.pending_events.drain(..).collect();
        events.extend(self.webview.drain_events());
        Ok(events)
    }

    fn dispatch_command(&mut self, command: WebViewCommand) -> Result<(), HostError> {
        self.webview
            .dispatch_command(command)
            .map_err(HostError::new)
    }

    fn resolve_navigation_request(
        &mut self,
        navigation_id: &str,
        allow: bool,
    ) -> Result<(), HostError> {
        self.webview
            .resolve_navigation_request(navigation_id, allow)
            .map_err(HostError::new)
    }

    fn resolve_simple_dialog(
        &mut self,
        dialog_id: &str,
        confirmed: bool,
        prompt_value: Option<&str>,
    ) -> Result<(), HostError> {
        self.webview
            .resolve_simple_dialog(dialog_id, confirmed, prompt_value)
            .map_err(HostError::new)
    }

    fn resolve_context_menu(
        &mut self,
        context_menu_id: &str,
        action: ContextMenuAction,
    ) -> Result<(), HostError> {
        self.webview
            .resolve_context_menu(context_menu_id, action)
            .map_err(HostError::new)
    }

    fn dismiss_context_menu(&mut self, context_menu_id: &str) -> Result<(), HostError> {
        self.webview
            .dismiss_context_menu(context_menu_id)
            .map_err(HostError::new)
    }

    fn evaluate_javascript(&mut self, evaluation_id: &str, script: &str) -> Result<(), HostError> {
        self.webview
            .evaluate_javascript(evaluation_id, script)
            .map_err(HostError::new)
    }

    fn dispatch_input_event(&mut self, event: HostInputEvent) -> Result<(), HostError> {
        self.webview
            .dispatch_host_input_event(event)
            .map_err(HostError::new)
    }

    fn perform_updates(
        &mut self,
        services: &mut dyn SurfaceHostServices,
        webview: WebViewHandle,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.before_update(services, webview)?;
        self.pending_events.extend(
            self.webview
                .perform_updates(false, || {}, || {})
                .map_err(HostError::new)?,
        );
        self.present_pending(services, webview)
    }

    fn before_update(
        &mut self,
        services: &mut dyn SurfaceHostServices,
        webview: WebViewHandle,
    ) -> Result<(), HostError> {
        services
            .before_update(webview, self.surface.as_ref(), self.viewport)
            .map_err(|error| HostError::new(error.to_string()))?;
        self.refresh_driver.notify_vsync();
        Ok(())
    }

    fn present_pending(
        &mut self,
        services: &mut dyn SurfaceHostServices,
        webview: WebViewHandle,
    ) -> Result<Vec<HostEvent>, HostError> {
        if !self.webview.__is_live() {
            return Ok(Vec::new());
        }
        let rendering_context = self.render_target.rendering_context();
        let target_kind = self.render_target.kind();
        let surface_attached = self.surface_attached && self.render_target.can_paint()?;
        let surface = self.surface.clone();
        let viewport = self.viewport;
        let did_present = Rc::new(Cell::new(false));
        let did_present_for_closure = did_present.clone();
        let mut present_result: Result<(), HostError> = Ok(());
        let events = self.webview.present_pending(surface_attached, || {
            did_present_for_closure.set(true);
            let Some(surface) = surface.as_ref() else {
                return;
            };
            let Some(viewport) = viewport else {
                return;
            };
            let mut frame = SurfaceFrame::new(
                webview,
                rendering_context.as_ref(),
                viewport,
                target_kind,
                #[cfg(target_os = "macos")]
                self.render_target.exportable_context(),
            );
            present_result = services
                .present_frame(webview, surface, &mut frame)
                .map_err(|error| HostError::new(error.to_string()));
        });

        if did_present.get() {
            if let Err(error) = present_result {
                self.pending_events.extend(events);
                return Err(error);
            }
        }

        let mut pending_events: Vec<_> = self.pending_events.drain(..).collect();
        pending_events.extend(events);
        Ok(pending_events)
    }
}

impl Drop for SurfaceWebView {
    fn drop(&mut self) {
        if self.surface_attached {
            let _ = self.render_target.detach();
            self.surface_attached = false;
        }
    }
}

fn create_rendering_context(
    native_surface: NativeSurface<'_>,
    size: SurfaceSize,
    refresh_driver: Rc<SurfaceRefreshDriver>,
) -> Result<WindowRenderingContext, HostError> {
    WindowRenderingContext::new_with_refresh_driver(
        native_surface.display_handle(),
        native_surface.window_handle(),
        physical_size(size),
        refresh_driver,
    )
    .map_err(|error| HostError::new(format!("{error:?}")))
}

fn physical_size(size: SurfaceSize) -> PhysicalSize<u32> {
    PhysicalSize::new(size.width, size.height)
}

#[cfg(target_os = "macos")]
fn exportable_physical_size(viewport: SurfaceViewport) -> Result<SurfaceSize, HostError> {
    if !viewport.scale_factor.is_finite() || viewport.scale_factor <= 0.0 {
        return Err(HostError::new(format!(
            "display scale must be finite and positive, got {}",
            viewport.scale_factor
        )));
    }
    let width = (viewport.size.width as f64 * viewport.scale_factor as f64).round();
    let height = (viewport.size.height as f64 * viewport.scale_factor as f64).round();
    if !(1.0..=u32::MAX as f64).contains(&width) || !(1.0..=u32::MAX as f64).contains(&height) {
        return Err(HostError::new(format!(
            "logical viewport {}x{} at scale {} has an invalid physical extent",
            viewport.size.width, viewport.size.height, viewport.scale_factor
        )));
    }
    Ok(SurfaceSize::new(width as u32, height as u32))
}

#[derive(Clone)]
struct ServoEventLoopWaker {
    wake: SurfaceEventLoopWaker,
}

impl ServoEventLoopWaker {
    fn new(wake: SurfaceEventLoopWaker) -> Self {
        Self { wake }
    }
}

impl EventLoopWaker for ServoEventLoopWaker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        (self.wake)();
    }
}

struct ServoClipboardAdapter {
    clipboard: Rc<dyn SurfaceClipboard>,
}

impl ServoClipboardAdapter {
    fn new(clipboard: Rc<dyn SurfaceClipboard>) -> Self {
        Self { clipboard }
    }
}

impl ClipboardDelegate for ServoClipboardAdapter {
    fn clear(&self, _webview: WebView) {
        let _ = self.clipboard.clear_text();
    }

    fn get_text(&self, _webview: WebView, request: StringRequest) {
        match self.clipboard.get_text() {
            Ok(text) => request.success(text),
            Err(error) => request.failure(error.to_string()),
        }
    }

    fn set_text(&self, _webview: WebView, new_contents: String) {
        let _ = self.clipboard.set_text(new_contents);
    }
}

#[derive(Default)]
struct SurfaceRefreshDriver {
    start_frame_callbacks: RefCell<Vec<Box<dyn Fn() + Send>>>,
}

impl SurfaceRefreshDriver {
    fn notify_vsync(&self) {
        let start_frame_callbacks: Vec<_> =
            self.start_frame_callbacks.borrow_mut().drain(..).collect();
        for callback in start_frame_callbacks {
            callback();
        }
    }
}

impl RefreshDriver for SurfaceRefreshDriver {
    fn observe_next_frame(&self, start_frame_callback: Box<dyn Fn() + Send + 'static>) {
        self.start_frame_callbacks
            .borrow_mut()
            .push(start_frame_callback);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::{ExplorerKitError, Runtime},
        surface::SurfacePoint,
    };

    #[test]
    fn host_options_expose_the_process_config_directory() {
        let directory = PathBuf::from("browser-profile");
        let options = SurfaceHostOptions::default().with_config_directory(directory.clone());

        assert_eq!(options.config_directory(), Some(directory.as_path()));
        assert_eq!(SurfaceHostOptions::default().config_directory(), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn exportable_offscreen_scales_logical_viewport_exactly_once() {
        let viewport = SurfaceViewport::with_origin(
            SurfacePoint::new(40, 20),
            SurfaceSize::new(480, 300),
            2.0,
        );

        assert_eq!(
            exportable_physical_size(viewport).unwrap(),
            SurfaceSize::new(960, 600)
        );
        assert_eq!(viewport.size, SurfaceSize::new(480, 300));
        assert!(exportable_physical_size(viewport.with_scale_factor(0.0)).is_err());
    }

    #[test]
    fn destroying_a_view_detaches_before_drop_and_keeps_sibling_surface_live() {
        let log = SharedLog::default();
        let faults = FakeFaults::default();
        let mut runtime =
            fake_runtime_with_faults(log.clone(), Rc::new(Cell::new(false)), faults.clone());
        let session = runtime.create_session();
        let first = runtime.create_webview(session).unwrap();
        let second = runtime.create_webview(session).unwrap();
        for (view, name) in [(first, "first"), (second, "second")] {
            runtime
                .attach_surface(view, HostSurface::new(name), SurfaceSize::new(320, 240))
                .unwrap();
        }
        log.take();
        runtime.perform_all_updates().unwrap();
        let updates = log.take();
        assert_eq!(
            updates
                .iter()
                .filter(|entry| *entry == "engine-update")
                .count(),
            1
        );
        assert!(updates
            .iter()
            .any(|entry| entry.starts_with("present:1:first:")));
        assert!(updates
            .iter()
            .any(|entry| entry.starts_with("present:2:second:")));
        faults.fail_detach.set(true);
        assert!(runtime.destroy_webview(first).is_err());
        assert_eq!(faults.drops.get(), 0);
        faults.fail_detach.set(false);
        runtime.destroy_webview(first).unwrap();
        assert_eq!(log.take(), vec!["detach"]);
        assert_eq!(faults.drops.get(), 1);
        runtime.perform_all_updates().unwrap();
        assert!(log
            .take()
            .iter()
            .any(|entry| entry.starts_with("present:2:second:")));
        let replacement = runtime.create_webview(session).unwrap();
        runtime
            .attach_surface(
                replacement,
                HostSurface::new("replacement"),
                SurfaceSize::new(640, 480),
            )
            .unwrap();
        runtime.destroy_webview(second).unwrap();
        runtime.perform_all_updates().unwrap();
        assert_eq!(faults.drops.get(), 2);
        runtime.destroy_webview(replacement).unwrap();
        assert_eq!(faults.drops.get(), 3);
    }

    #[test]
    fn final_shutdown_retires_all_views_and_engine_after_detach_errors() {
        let log = SharedLog::default();
        let faults = FakeFaults::default();
        let mut runtime =
            fake_runtime_with_faults(log.clone(), Rc::new(Cell::new(false)), faults.clone());
        let session = runtime.create_session();
        for name in ["first", "second"] {
            let view = runtime.create_webview(session).unwrap();
            runtime
                .attach_surface(view, HostSurface::new(name), SurfaceSize::new(320, 240))
                .unwrap();
        }
        log.take();
        faults.detach_attempts.set(0);
        faults.fail_detach.set(true);
        assert_eq!(
            runtime.shutdown(),
            Err(ExplorerKitError::Host(HostError::new("detach failed")))
        );
        assert_eq!(faults.detach_attempts.get(), 2);
        assert_eq!(faults.drops.get(), 2);
        assert_eq!(log.take(), vec!["engine-shutdown:drops=2"]);
    }

    #[derive(Clone, Default)]
    struct SharedLog(Rc<RefCell<Vec<String>>>);

    impl SharedLog {
        fn push(&self, entry: impl Into<String>) {
            self.0.borrow_mut().push(entry.into());
        }

        fn take(&self) -> Vec<String> {
            self.0.borrow_mut().drain(..).collect()
        }
    }

    struct NonWinitLayoutServices {
        log: SharedLog,
        fail_present: Rc<Cell<bool>>,
        update_faults: Rc<RefCell<VecDeque<FakeUpdateFault>>>,
        target_kind: SurfaceMode,
    }

    impl NonWinitLayoutServices {
        fn new(
            log: SharedLog,
            fail_present: Rc<Cell<bool>>,
            update_faults: Rc<RefCell<VecDeque<FakeUpdateFault>>>,
        ) -> Self {
            Self {
                log,
                fail_present,
                update_faults,
                target_kind: SurfaceMode::Offscreen,
            }
        }
    }

    impl SurfaceHostServices for NonWinitLayoutServices {
        fn render_target(
            &mut self,
            surface: &HostSurface,
            viewport: SurfaceViewport,
        ) -> Result<RenderTarget<'_>, SurfaceError> {
            self.log.push(format!(
                "target:{}:{}x{}@{}+{},{}",
                surface.id(),
                viewport.size.width,
                viewport.size.height,
                viewport.scale_factor,
                viewport.origin.x,
                viewport.origin.y
            ));
            Ok(RenderTarget::Test(self.target_kind))
        }

        fn update_render_target(
            &mut self,
            surface: &HostSurface,
            viewport: SurfaceViewport,
        ) -> Result<RenderTarget<'_>, SurfaceError> {
            self.log.push(format!(
                "target-update:{}:{}x{}@{}+{},{}",
                surface.id(),
                viewport.size.width,
                viewport.size.height,
                viewport.scale_factor,
                viewport.origin.x,
                viewport.origin.y
            ));
            apply_update_fault(&self.update_faults, "render-target")
                .map_err(|error| SurfaceError::new(error.to_string()))?;
            Ok(RenderTarget::Test(self.target_kind))
        }

        fn before_update(
            &mut self,
            webview: WebViewHandle,
            surface: Option<&HostSurface>,
            viewport: Option<SurfaceViewport>,
        ) -> Result<(), SurfaceError> {
            let surface = surface.map(HostSurface::id).unwrap_or("none");
            let viewport = viewport.expect("fake surface webview always has a viewport");
            self.log.push(format!(
                "before:{}:{}:{}x{}@{}",
                webview.raw(),
                surface,
                viewport.size.width,
                viewport.size.height,
                viewport.scale_factor
            ));
            Ok(())
        }

        fn present_frame(
            &mut self,
            webview: WebViewHandle,
            surface: &HostSurface,
            frame: &mut SurfaceFrame<'_>,
        ) -> Result<(), SurfaceError> {
            self.log.push(format!(
                "present:{}:{}:{}x{}@{}:{:?}",
                webview.raw(),
                surface.id(),
                frame.viewport().size.width,
                frame.viewport().size.height,
                frame.viewport().scale_factor,
                frame.mode()
            ));
            if self.fail_present.get() {
                return Err(SurfaceError::new("present failed"));
            }
            frame.present();
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    enum FakeUpdateFault {
        Pass,
        Fail,
        Panic,
    }

    fn apply_update_fault(
        faults: &RefCell<VecDeque<FakeUpdateFault>>,
        label: &str,
    ) -> Result<(), HostError> {
        match faults
            .borrow_mut()
            .pop_front()
            .unwrap_or(FakeUpdateFault::Pass)
        {
            FakeUpdateFault::Pass => Ok(()),
            FakeUpdateFault::Fail => Err(HostError::new(format!("{label} update failed"))),
            FakeUpdateFault::Panic => panic!("{label} update panic"),
        }
    }

    type FakeEngineUpdate = Box<dyn FnOnce() -> Result<(), HostError>>;

    #[derive(Clone, Default)]
    struct FakeFaults {
        bindings: Rc<RefCell<HashMap<WebViewHandle, SurfaceRuntimeBinding>>>,
        retired: Rc<RefCell<HashSet<WebViewHandle>>>,
        engine_update: Rc<RefCell<Option<FakeEngineUpdate>>>,
        fail_attach: Rc<Cell<bool>>,
        panic_attach: Rc<Cell<bool>>,
        fail_detach: Rc<Cell<bool>>,
        detach_attempts: Rc<Cell<usize>>,
        panic_detach: Rc<Cell<bool>>,
        driver_updates: Rc<RefCell<VecDeque<FakeUpdateFault>>>,
        target_updates: Rc<RefCell<VecDeque<FakeUpdateFault>>>,
        drops: Rc<Cell<usize>>,
    }

    struct FakeFactory {
        log: SharedLog,
        faults: FakeFaults,
    }

    impl ServoWebViewFactory for FakeFactory {
        fn shutdown(&self) -> Result<(), HostError> {
            self.log
                .push(format!("engine-shutdown:drops={}", self.faults.drops.get()));
            Ok(())
        }

        fn perform_updates(&self) -> Result<(), HostError> {
            self.log.push("engine-update");
            let update = self.faults.engine_update.borrow_mut().take();
            update.map_or(Ok(()), |update| update())
        }

        fn create(
            &self,
            target: RenderTarget<'_>,
            surface: HostSurface,
            viewport: SurfaceViewport,
            _options: &SurfaceHostOptions,
            initial_url: Option<&str>,
        ) -> Result<Box<dyn ServoWebViewDriver>, HostError> {
            self.log.push(format!(
                "create:{:?}:{}:{}x{}@{}:{initial_url:?}",
                target.kind(),
                surface.id(),
                viewport.size.width,
                viewport.size.height,
                viewport.scale_factor
            ));
            Ok(Box::new(FakeWebView {
                handle: None,
                log: self.log.clone(),
                target_kind: target.kind(),
                surface: Some(surface),
                viewport,
                attached: true,
                faults: self.faults.clone(),
                pending_events: VecDeque::new(),
                adopted_child: false,
            }))
        }
    }

    struct FakeWebView {
        handle: Option<WebViewHandle>,
        adopted_child: bool,
        log: SharedLog,
        target_kind: SurfaceMode,
        surface: Option<HostSurface>,
        viewport: SurfaceViewport,
        attached: bool,
        faults: FakeFaults,
        pending_events: VecDeque<HostEvent>,
    }

    impl FakeWebView {
        fn record_viewport(&self, label: &str) {
            self.log.push(format!(
                "{}:{}x{}@{}+{},{}",
                label,
                self.viewport.size.width,
                self.viewport.size.height,
                self.viewport.scale_factor,
                self.viewport.origin.x,
                self.viewport.origin.y
            ));
        }
    }

    impl ServoWebViewDriver for FakeWebView {
        fn bind_runtime(
            &mut self,
            binding: SurfaceRuntimeBinding,
            _: &SurfaceHostOptions,
        ) -> Result<(), HostError> {
            self.handle = Some(binding.webview);
            self.faults
                .bindings
                .borrow_mut()
                .insert(binding.webview, binding);
            Ok(())
        }

        fn can_dispatch_unattached(&self) -> bool {
            self.adopted_child
        }

        fn is_live(&self) -> bool {
            self.handle
                .is_none_or(|handle| !self.faults.retired.borrow().contains(&handle))
        }

        fn attach(
            &mut self,
            target: RenderTarget<'_>,
            surface: HostSurface,
            viewport: SurfaceViewport,
        ) -> Result<(), HostError> {
            self.target_kind = target.kind();
            self.surface = Some(surface);
            self.viewport = viewport;
            self.attached = true;
            if self.faults.panic_attach.get() {
                panic!("attach panic");
            }
            if self.faults.fail_attach.get() {
                return Err(HostError::new("attach failed"));
            }
            self.record_viewport("attach");
            Ok(())
        }

        fn update_viewport(
            &mut self,
            target: RenderTarget<'_>,
            viewport: SurfaceViewport,
        ) -> Result<(), HostError> {
            self.target_kind = target.kind();
            self.viewport = viewport;
            self.record_viewport("update");
            apply_update_fault(&self.faults.driver_updates, "driver")?;
            Ok(())
        }

        fn detach(&mut self) -> Result<Vec<HostEvent>, HostError> {
            self.faults
                .detach_attempts
                .set(self.faults.detach_attempts.get() + 1);
            if self.faults.panic_detach.get() {
                panic!("detach panic");
            }
            if self.faults.fail_detach.get() {
                return Err(HostError::new("detach failed"));
            }
            self.attached = false;
            self.surface = None;
            self.log.push("detach");
            Ok(self.pending_events.drain(..).collect())
        }

        fn dispatch_command(&mut self, command: WebViewCommand) -> Result<(), HostError> {
            let command = match command {
                WebViewCommand::LoadUrl(request) => format!("load:{}", request.url),
                WebViewCommand::Reload => "reload".to_owned(),
                WebViewCommand::GoBack => "go-back".to_owned(),
                WebViewCommand::GoForward => "go-forward".to_owned(),
                WebViewCommand::Focus => "focus".to_owned(),
                WebViewCommand::Blur => "blur".to_owned(),
            };
            self.log.push(format!("command:{command}"));
            Ok(())
        }

        fn resolve_navigation_request(
            &mut self,
            navigation_id: &str,
            allow: bool,
        ) -> Result<(), HostError> {
            self.log
                .push(format!("resolve-navigation:{navigation_id}:{allow}"));
            Ok(())
        }

        fn resolve_simple_dialog(
            &mut self,
            dialog_id: &str,
            confirmed: bool,
            prompt_value: Option<&str>,
        ) -> Result<(), HostError> {
            self.log.push(format!(
                "resolve-dialog:{dialog_id}:{confirmed}:{prompt_value:?}"
            ));
            Ok(())
        }

        fn resolve_context_menu(
            &mut self,
            context_menu_id: &str,
            action: ContextMenuAction,
        ) -> Result<(), HostError> {
            self.log.push(format!(
                "resolve-context-menu:{context_menu_id}:{}",
                action.as_str()
            ));
            Ok(())
        }

        fn dismiss_context_menu(&mut self, context_menu_id: &str) -> Result<(), HostError> {
            self.log
                .push(format!("dismiss-context-menu:{context_menu_id}"));
            Ok(())
        }

        fn evaluate_javascript(
            &mut self,
            evaluation_id: &str,
            _script: &str,
        ) -> Result<(), HostError> {
            self.log.push(format!("evaluate:{evaluation_id}"));
            self.pending_events
                .push_back(HostEvent::JavaScriptEvaluationResult {
                    evaluation_id: evaluation_id.to_owned(),
                    ok: true,
                    value_json: Some("null".to_owned()),
                    error_type: None,
                });
            Ok(())
        }

        fn dispatch_input_event(&mut self, _event: HostInputEvent) -> Result<(), HostError> {
            Ok(())
        }

        fn perform_updates(
            &mut self,
            services: &mut dyn SurfaceHostServices,
            webview: WebViewHandle,
        ) -> Result<Vec<HostEvent>, HostError> {
            self.record_viewport("driver-update");
            let update = self.faults.engine_update.borrow_mut().take();
            if let Some(update) = update {
                update()?;
            }
            services
                .before_update(webview, self.surface.as_ref(), Some(self.viewport))
                .map_err(|error| HostError::new(error.to_string()))?;
            if self.attached {
                let surface = self
                    .surface
                    .as_ref()
                    .expect("attached fake webview has a surface");
                let mut frame = SurfaceFrame::for_test(self.viewport, self.target_kind);
                services
                    .present_frame(webview, surface, &mut frame)
                    .map_err(|error| HostError::new(error.to_string()))?;
            }
            Ok(self.pending_events.drain(..).collect())
        }
    }

    impl Drop for FakeWebView {
        fn drop(&mut self) {
            self.faults.drops.set(self.faults.drops.get() + 1);
        }
    }

    type FakeRuntime = Runtime<SurfaceHost<NonWinitLayoutServices>>;

    fn fake_runtime(log: SharedLog, fail_present: Rc<Cell<bool>>) -> FakeRuntime {
        fake_runtime_with_faults(log, fail_present, FakeFaults::default())
    }

    fn fake_runtime_with_faults(
        log: SharedLog,
        fail_present: Rc<Cell<bool>>,
        faults: FakeFaults,
    ) -> FakeRuntime {
        let services =
            NonWinitLayoutServices::new(log.clone(), fail_present, faults.target_updates.clone());
        let host = SurfaceHost::with_webview_factory(
            services,
            SurfaceHostOptions::default(),
            Rc::new(FakeFactory { log, faults }),
        );
        Runtime::new(host)
    }

    fn evaluation_ids(runtime: &mut FakeRuntime) -> Vec<String> {
        runtime
            .drain_events()
            .into_iter()
            .filter_map(|event| match event.event {
                HostEvent::JavaScriptEvaluationResult { evaluation_id, .. } => Some(evaluation_id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn children_committed_during_a_failed_update_are_live_without_attachment_or_url_replay() {
        for pump_fails in [false, true] {
            let log = SharedLog::default();
            let faults = FakeFaults::default();
            let fail_present = Rc::new(Cell::new(false));
            let mut runtime =
                fake_runtime_with_faults(log.clone(), fail_present.clone(), faults.clone());
            let session = runtime.create_session();
            let source = runtime.create_webview(session).unwrap();
            runtime
                .attach_surface(source, HostSurface::new("source"), SurfaceSize::new(80, 60))
                .unwrap();
            runtime.drain_events();
            log.take();
            let binding = faults.bindings.borrow()[&source].clone();
            let child_log = log.clone();
            let child_faults = faults.clone();
            *faults.engine_update.borrow_mut() = Some(Box::new(move || {
                let reservation = binding
                    .registrar
                    .reserve_child(source, "request-1".into())
                    .unwrap();
                let child = reservation.webview();
                let driver = Box::new(FakeWebView {
                    handle: Some(child),
                    adopted_child: true,
                    log: child_log,
                    faults: child_faults,
                    target_kind: SurfaceMode::Offscreen,
                    surface: None,
                    viewport: SurfaceViewport::new(SurfaceSize::new(80, 60), 1.0),
                    attached: false,
                    pending_events: VecDeque::new(),
                });
                binding
                    .adoptions
                    .upgrade()
                    .unwrap()
                    .borrow_mut()
                    .push(AdoptedSurfaceWebView {
                        webview: child,
                        session,
                        driver,
                    });
                binding.registrar.record_event(
                    child,
                    HostEvent::PageTitleChanged {
                        title: Some("original native child".into()),
                    },
                );
                reservation.commit(PopupCreated {
                    request_id: "request-1".into(),
                    parent_webview_id: "native-source".into(),
                    child_webview_id: "native-child".into(),
                    parent_url: None,
                    target_url: None,
                    window_features: None,
                    wants_popup: None,
                });
                if pump_fails {
                    Err(HostError::new("engine failed after creation"))
                } else {
                    Ok(())
                }
            }));
            fail_present.set(!pump_fails);
            assert!(runtime.perform_all_updates().is_err());
            let updates = log.take();
            assert_eq!(
                updates
                    .iter()
                    .filter(|entry| *entry == "engine-update")
                    .count(),
                1
            );
            let events = runtime.drain_events();
            assert_eq!(events.len(), 2);
            let child = events[0].created_webview.unwrap();
            assert_eq!(events[0].webview, source);
            assert_eq!(events[1].webview, child);
            fail_present.set(false);
            runtime.destroy_webview(source).unwrap();
            runtime
                .evaluate_javascript(child, "read", "location.href")
                .unwrap();
            assert_eq!(evaluation_ids(&mut runtime), vec!["read"]);
            runtime
                .attach_surface_with_viewport(
                    child,
                    HostSurface::new("child"),
                    SurfaceViewport::new(SurfaceSize::new(120, 90), 2.0),
                )
                .unwrap();
            let log = log.take();
            assert!(log.iter().any(|entry| entry == "attach:120x90@2+0,0"));
            assert!(!log
                .iter()
                .any(|entry| entry.starts_with("create:") || entry.starts_with("command:load:")));
            faults.fail_detach.set(true);
            assert!(runtime.destroy_webview(child).is_err());
            faults.fail_detach.set(false);
            runtime.destroy_webview(child).unwrap();
        }
    }

    #[test]
    fn attachment_pumps_import_children_and_release_closed_drivers_even_on_error() {
        for resize in [false, true] {
            for fail_pump in [false, true] {
                let log = SharedLog::default();
                let faults = FakeFaults::default();
                let mut runtime = fake_runtime_with_faults(
                    log.clone(),
                    Rc::new(Cell::new(false)),
                    faults.clone(),
                );
                let session = runtime.create_session();
                let source = runtime.create_webview(session).unwrap();
                let size = SurfaceSize::new(80, 60);
                runtime
                    .attach_surface(source, HostSurface::new("source"), size)
                    .unwrap();
                if !resize {
                    runtime.detach_surface(source).unwrap();
                }
                runtime.drain_events();
                let binding = faults.bindings.borrow()[&source].clone();
                let adoptions = binding.adoptions.upgrade().unwrap();
                let child_faults = faults.clone();
                *faults.engine_update.borrow_mut() =
                    Some(Box::new(move || {
                        let reservation = binding
                            .registrar
                            .reserve_child(source, "attachment-pump".into())
                            .unwrap();
                        let child = reservation.webview();
                        binding.adoptions.upgrade().unwrap().borrow_mut().push(
                            AdoptedSurfaceWebView {
                                webview: child,
                                session,
                                driver: Box::new(FakeWebView {
                                    handle: Some(child),
                                    adopted_child: true,
                                    log,
                                    faults: child_faults.clone(),
                                    target_kind: SurfaceMode::Offscreen,
                                    surface: None,
                                    viewport: SurfaceViewport::new(size, 1.0),
                                    attached: false,
                                    pending_events: VecDeque::new(),
                                }),
                            },
                        );
                        reservation.commit(PopupCreated {
                            request_id: "attachment-pump".into(),
                            parent_webview_id: "source-native".into(),
                            child_webview_id: "child-native".into(),
                            parent_url: None,
                            target_url: None,
                            window_features: None,
                            wants_popup: None,
                        });
                        child_faults.retired.borrow_mut().insert(source);
                        binding.registrar.retire_webview(source);
                        if fail_pump {
                            Err(HostError::new("pump failed after close"))
                        } else {
                            Ok(())
                        }
                    }));
                let result = if resize {
                    runtime.resize_surface(source, SurfaceSize::new(120, 90))
                } else {
                    runtime.attach_surface(source, HostSurface::new("reattached"), size)
                };
                assert_eq!(result.is_err(), fail_pump);
                assert!(
                    adoptions.borrow().is_empty(),
                    "children must be imported before returning"
                );
                assert_eq!(
                    faults.drops.get(),
                    1,
                    "native closure must release the driver before returning"
                );
                let events = runtime.drain_events();
                let child = events
                    .iter()
                    .find_map(|event| event.created_webview)
                    .unwrap();
                assert!(events
                    .iter()
                    .any(|event| event.webview == source && event.event == HostEvent::Closed));
                runtime.evaluate_javascript(child, "survivor", "1").unwrap();
                assert_eq!(evaluation_ids(&mut runtime), vec!["survivor"]);
            }
        }
    }

    #[test]
    fn unsupported_legacy_aliases_fail_and_default_denial_needs_no_creation_support() {
        let mut runtime = fake_runtime(SharedLog::default(), Rc::new(Cell::new(false)));
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        runtime
            .set_popup_policy(source, PopupRequestPolicy::DefaultDeny)
            .unwrap();
        assert!(runtime
            .set_popup_policy(source, PopupRequestPolicy::ManagedChild)
            .is_err());
        assert!(runtime
            .attach_managed_child_surface(
                source,
                "absent",
                HostSurface::new("absent"),
                SurfaceViewport::new(SurfaceSize::new(80, 60), 1.0)
            )
            .is_err());
        assert!(runtime
            .perform_managed_child_updates(source, "absent")
            .is_err());
        assert!(runtime.drain_events().is_empty());
    }

    #[test]
    fn first_queued_load_is_promoted_into_webview_construction() {
        let log = SharedLog::default();
        let mut runtime = fake_runtime(log.clone(), Rc::new(Cell::new(false)));
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);

        runtime.load_url(webview, "example.com").unwrap();
        runtime
            .attach_surface_with_viewport(webview, HostSurface::new("gpui-slot"), viewport)
            .unwrap();

        let log = log.take();
        assert!(log
            .iter()
            .any(|entry| entry
                == "create:Offscreen:gpui-slot:320x240@2:Some(\"https://example.com/\")"));
        assert!(!log
            .iter()
            .any(|entry| entry == "command:load:https://example.com/"));
    }

    #[test]
    fn later_queued_load_uses_normal_dispatch_after_promoted_initial_load() {
        let log = SharedLog::default();
        let mut runtime = fake_runtime(log.clone(), Rc::new(Cell::new(false)));
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);

        runtime.load_url(webview, "initial.test").unwrap();
        runtime.load_url(webview, "later.test").unwrap();
        runtime
            .attach_surface_with_viewport(webview, HostSurface::new("loads-slot"), viewport)
            .unwrap();

        let lifecycle: Vec<_> = log
            .take()
            .into_iter()
            .filter(|entry| {
                entry.starts_with("create:")
                    || entry.starts_with("command:")
                    || entry.starts_with("driver-update:")
            })
            .collect();
        assert_eq!(
            lifecycle,
            vec![
                "create:Offscreen:loads-slot:320x240@2:Some(\"https://initial.test/\")",
                "driver-update:320x240@2+0,0",
                "command:load:https://later.test/",
                "driver-update:320x240@2+0,0",
                "driver-update:320x240@2+0,0",
            ]
        );
    }

    #[test]
    fn detach_and_reattach_do_not_replay_promoted_initial_load() {
        let log = SharedLog::default();
        let mut runtime = fake_runtime(log.clone(), Rc::new(Cell::new(false)));
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let surface = HostSurface::new("reattach-slot");

        runtime.load_url(webview, "initial.test").unwrap();
        runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .unwrap();
        runtime.detach_surface(webview).unwrap();
        runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .unwrap();
        runtime.detach_surface(webview).unwrap();
        runtime
            .attach_surface_with_viewport(webview, surface, viewport)
            .unwrap();

        let log = log.take();
        assert_eq!(
            log.iter()
                .filter(|entry| entry.starts_with("create:"))
                .count(),
            1
        );
        assert_eq!(
            log.iter()
                .filter(|entry| entry.as_str() == "command:load:https://initial.test/")
                .count(),
            0
        );
    }

    #[test]
    fn evaluation_before_later_load_is_replayed_in_order_and_completes_once() {
        let log = SharedLog::default();
        let mut runtime = fake_runtime(log.clone(), Rc::new(Cell::new(false)));
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);

        runtime
            .evaluate_javascript(webview, "evaluation-before-load", "1 + 1")
            .unwrap();
        runtime.load_url(webview, "later.test").unwrap();
        runtime
            .attach_surface_with_viewport(webview, HostSurface::new("ordered-slot"), viewport)
            .unwrap();

        let commands: Vec<_> = log
            .take()
            .into_iter()
            .filter(|entry| entry.starts_with("evaluate:") || entry.starts_with("command:"))
            .collect();
        assert_eq!(
            commands,
            vec![
                "evaluate:evaluation-before-load",
                "command:load:https://later.test/",
            ]
        );
        assert_eq!(evaluation_ids(&mut runtime), vec!["evaluation-before-load"]);
    }

    #[test]
    fn detached_evaluations_and_loads_keep_full_fifo_order() {
        let log = SharedLog::default();
        let mut runtime = fake_runtime(log.clone(), Rc::new(Cell::new(false)));
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);

        runtime
            .evaluate_javascript(webview, "evaluation-1", "1")
            .unwrap();
        runtime.load_url(webview, "one.test").unwrap();
        runtime
            .evaluate_javascript(webview, "evaluation-2", "2")
            .unwrap();
        runtime.load_url(webview, "two.test").unwrap();
        runtime
            .evaluate_javascript(webview, "evaluation-3", "3")
            .unwrap();
        runtime
            .attach_surface_with_viewport(webview, HostSurface::new("fifo-slot"), viewport)
            .unwrap();

        let commands: Vec<_> = log
            .take()
            .into_iter()
            .filter(|entry| entry.starts_with("evaluate:") || entry.starts_with("command:"))
            .collect();
        assert_eq!(
            commands,
            vec![
                "evaluate:evaluation-1",
                "command:load:https://one.test/",
                "evaluate:evaluation-2",
                "command:load:https://two.test/",
                "evaluate:evaluation-3",
            ]
        );
        assert_eq!(
            evaluation_ids(&mut runtime),
            vec!["evaluation-1", "evaluation-2", "evaluation-3"]
        );
    }

    #[test]
    fn javascript_evaluation_routes_through_attached_servo_webview() {
        let log = SharedLog::default();
        let mut runtime = fake_runtime(log.clone(), Rc::new(Cell::new(false)));
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        runtime
            .attach_surface_with_viewport(
                webview,
                HostSurface::new("javascript-slot"),
                SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0),
            )
            .unwrap();

        runtime
            .evaluate_javascript(webview, "evaluation-1", "1 + 1")
            .unwrap();

        assert!(log
            .take()
            .iter()
            .any(|entry| entry == "evaluate:evaluation-1"));
    }

    #[test]
    fn javascript_evaluation_queues_while_detached_and_replays_on_attach() {
        let log = SharedLog::default();
        let mut runtime = fake_runtime(log.clone(), Rc::new(Cell::new(false)));
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let surface = HostSurface::new("javascript-slot");
        runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .unwrap();
        runtime.detach_surface(webview).unwrap();
        log.take();

        runtime
            .evaluate_javascript(webview, "evaluation-detached", "1 + 1")
            .unwrap();
        assert!(log.take().is_empty());

        runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .unwrap();
        assert_eq!(evaluation_ids(&mut runtime), vec!["evaluation-detached"]);
        let replay = log.take();
        assert!(replay
            .iter()
            .any(|entry| entry == "evaluate:evaluation-detached"));
        assert!(!replay.iter().any(|entry| entry.starts_with("create:")));

        runtime.detach_surface(webview).unwrap();
        runtime
            .attach_surface_with_viewport(webview, surface, viewport)
            .unwrap();
        assert!(evaluation_ids(&mut runtime).is_empty());
        assert!(!log
            .take()
            .iter()
            .any(|entry| entry == "evaluate:evaluation-detached"));
    }

    #[test]
    fn partial_flush_does_not_replay_an_executed_evaluation_on_retry() {
        let log = SharedLog::default();
        let fail_present = Rc::new(Cell::new(true));
        let mut runtime = fake_runtime(log.clone(), fail_present.clone());
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let surface = HostSurface::new("partial-flush-slot");

        runtime
            .evaluate_javascript(webview, "evaluation-1", "1")
            .unwrap();
        runtime.load_url(webview, "after-evaluation.test").unwrap();
        runtime
            .evaluate_javascript(webview, "evaluation-2", "2")
            .unwrap();
        runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .expect_err("the first present should fail attach");
        assert!(evaluation_ids(&mut runtime).is_empty());

        fail_present.set(false);
        runtime
            .attach_surface_with_viewport(webview, surface, viewport)
            .unwrap();

        let commands: Vec<_> = log
            .take()
            .into_iter()
            .filter(|entry| entry.starts_with("evaluate:") || entry.starts_with("command:"))
            .collect();
        assert_eq!(
            commands,
            vec![
                "evaluate:evaluation-1",
                "command:load:https://after-evaluation.test/",
                "evaluate:evaluation-2",
            ]
        );
        assert_eq!(
            evaluation_ids(&mut runtime),
            vec!["evaluation-1", "evaluation-2"]
        );
    }

    #[test]
    fn attach_panic_rolls_existing_servo_webview_back_to_detached() {
        let log = SharedLog::default();
        let faults = FakeFaults::default();
        let mut runtime =
            fake_runtime_with_faults(log.clone(), Rc::new(Cell::new(false)), faults.clone());
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let surface = HostSurface::new("panic-slot");
        runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .unwrap();
        runtime.detach_surface(webview).unwrap();
        log.take();

        faults.panic_attach.set(true);
        assert!(catch_unwind(AssertUnwindSafe(|| {
            runtime
                .attach_surface_with_viewport(webview, surface.clone(), viewport)
                .unwrap();
        }))
        .is_err());

        faults.panic_attach.set(false);
        runtime
            .attach_surface_with_viewport(webview, surface, viewport)
            .unwrap();
        let recovered = log.take();
        assert!(recovered.iter().any(|entry| entry == "detach"));
        assert!(!recovered.iter().any(|entry| entry.starts_with("create:")));
    }

    #[test]
    fn attach_failure_rolls_back_and_can_retry_the_same_driver() {
        let log = SharedLog::default();
        let faults = FakeFaults::default();
        let mut runtime =
            fake_runtime_with_faults(log.clone(), Rc::new(Cell::new(false)), faults.clone());
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let surface = HostSurface::new("attach-failure-slot");
        runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .unwrap();
        runtime.detach_surface(webview).unwrap();
        log.take();

        faults.fail_attach.set(true);
        runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .expect_err("faulted attach should fail");
        faults.fail_attach.set(false);
        runtime
            .attach_surface_with_viewport(webview, surface, viewport)
            .unwrap();

        let recovered = log.take();
        assert!(!recovered.iter().any(|entry| entry.starts_with("create:")));
        assert_eq!(
            recovered.iter().filter(|entry| *entry == "detach").count(),
            1
        );
    }

    #[test]
    fn detach_failure_keeps_the_attached_driver_retryable() {
        let faults = FakeFaults::default();
        let mut runtime = fake_runtime_with_faults(
            SharedLog::default(),
            Rc::new(Cell::new(false)),
            faults.clone(),
        );
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let surface = HostSurface::new("detach-failure-slot");
        runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .unwrap();

        faults.fail_detach.set(true);
        runtime
            .detach_surface(webview)
            .expect_err("faulted detach should fail");
        faults.fail_detach.set(false);
        runtime.detach_surface(webview).unwrap();
        runtime
            .attach_surface_with_viewport(webview, surface, viewport)
            .unwrap();
    }

    #[test]
    fn rollback_detach_failure_terminally_poisons_future_calls_and_destroys() {
        let faults = FakeFaults::default();
        let mut runtime = fake_runtime_with_faults(
            SharedLog::default(),
            Rc::new(Cell::new(false)),
            faults.clone(),
        );
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let surface = HostSurface::new("rollback-failure-slot");
        runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .unwrap();
        runtime.detach_surface(webview).unwrap();

        faults.fail_attach.set(true);
        faults.fail_detach.set(true);
        let error = runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .expect_err("rollback detach failure should poison the driver");
        assert!(error.to_string().contains("terminally unavailable"));
        assert_eq!(faults.drops.get(), 1);
        faults.fail_attach.set(false);
        faults.fail_detach.set(false);
        assert!(runtime.load_url(webview, "future.test").is_err());
        assert!(runtime
            .attach_surface_with_viewport(webview, surface, viewport)
            .is_err());
        drop(runtime);
        assert_eq!(faults.drops.get(), 1);
    }

    #[test]
    fn attach_and_rollback_panics_terminally_poison_future_calls_and_destroy() {
        let faults = FakeFaults::default();
        let mut runtime = fake_runtime_with_faults(
            SharedLog::default(),
            Rc::new(Cell::new(false)),
            faults.clone(),
        );
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let surface = HostSurface::new("rollback-panic-slot");
        runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .unwrap();
        runtime.detach_surface(webview).unwrap();

        faults.panic_attach.set(true);
        faults.panic_detach.set(true);
        assert!(catch_unwind(AssertUnwindSafe(|| {
            runtime
                .attach_surface_with_viewport(webview, surface.clone(), viewport)
                .unwrap();
        }))
        .is_err());
        assert_eq!(faults.drops.get(), 1);
        faults.panic_attach.set(false);
        faults.panic_detach.set(false);
        assert!(runtime
            .reload(webview)
            .unwrap_err()
            .to_string()
            .contains("terminally unavailable"));
        assert!(runtime
            .attach_surface_with_viewport(webview, surface, viewport)
            .is_err());
        drop(runtime);
        assert_eq!(faults.drops.get(), 1);
    }

    #[test]
    fn non_winit_layout_host_receives_viewport_scale_before_first_update() {
        let log = SharedLog::default();
        let mut runtime = fake_runtime(log.clone(), Rc::new(Cell::new(false)));
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::with_origin(
            SurfacePoint::new(12, 34),
            SurfaceSize::new(320, 240),
            2.5,
        );

        runtime
            .attach_surface_with_viewport(webview, HostSurface::new("gpui-slot"), viewport)
            .unwrap();

        assert_eq!(
            log.take(),
            vec![
                "target:gpui-slot:320x240@2.5+12,34",
                "create:Offscreen:gpui-slot:320x240@2.5:None",
                "driver-update:320x240@2.5+12,34",
                "before:1:gpui-slot:320x240@2.5",
                "present:1:gpui-slot:320x240@2.5:Offscreen",
            ]
        );
    }

    #[test]
    fn controller_responses_reach_attached_surface_webview() {
        let log = SharedLog::default();
        let mut runtime = fake_runtime(log.clone(), Rc::new(Cell::new(false)));
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);

        runtime
            .attach_surface_with_viewport(webview, HostSurface::new("layout-slot"), viewport)
            .unwrap();
        log.take();

        runtime
            .resolve_navigation_request(webview, "navigation-1", true)
            .unwrap();
        runtime
            .resolve_simple_dialog(webview, "dialog-1", true, Some("Servo"))
            .unwrap();
        runtime
            .resolve_context_menu(webview, "context-menu-1", ContextMenuAction::CopyLink)
            .unwrap();
        runtime
            .dismiss_context_menu(webview, "context-menu-2")
            .unwrap();

        assert_eq!(
            log.take(),
            vec![
                "resolve-navigation:navigation-1:true",
                "driver-update:320x240@2+0,0",
                "before:1:layout-slot:320x240@2",
                "present:1:layout-slot:320x240@2:Offscreen",
                "resolve-dialog:dialog-1:true:Some(\"Servo\")",
                "driver-update:320x240@2+0,0",
                "before:1:layout-slot:320x240@2",
                "present:1:layout-slot:320x240@2:Offscreen",
                "resolve-context-menu:context-menu-1:copy-link",
                "driver-update:320x240@2+0,0",
                "before:1:layout-slot:320x240@2",
                "present:1:layout-slot:320x240@2:Offscreen",
                "dismiss-context-menu:context-menu-2",
                "driver-update:320x240@2+0,0",
                "before:1:layout-slot:320x240@2",
                "present:1:layout-slot:320x240@2:Offscreen",
            ]
        );
    }

    #[test]
    fn callback_error_rolls_back_live_host_viewport_state() {
        let log = SharedLog::default();
        let fail_present = Rc::new(Cell::new(false));
        let mut runtime = fake_runtime(log.clone(), fail_present.clone());
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let surface = HostSurface::new("layout-slot");
        let initial = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let resized = SurfaceViewport::new(SurfaceSize::new(640, 480), 3.0);

        runtime
            .attach_surface_with_viewport(webview, surface.clone(), initial)
            .unwrap();
        let _ = runtime.drain_events();
        log.take();

        fail_present.set(true);
        let error = runtime
            .update_surface_viewport(webview, resized)
            .expect_err("present failure should reject the viewport update");
        assert!(matches!(error, ExplorerKitError::Host(_)));
        assert!(runtime.drain_events().is_empty());
        assert_eq!(
            log.take(),
            vec![
                "target-update:layout-slot:640x480@3+0,0",
                "update:640x480@3+0,0",
                "driver-update:640x480@3+0,0",
                "before:1:layout-slot:640x480@3",
                "present:1:layout-slot:640x480@3:Offscreen",
                "target-update:layout-slot:320x240@2+0,0",
                "update:320x240@2+0,0",
            ]
        );

        fail_present.set(false);
        runtime.update_surface_viewport(webview, resized).unwrap();
        let events = runtime.drain_events();
        assert!(events.iter().any(|event| matches!(
            &event.event,
            HostEvent::SurfaceResized { size } if *size == resized.size
        )));
    }

    #[test]
    fn viewport_rollback_failure_terminally_poisons_the_webview() {
        let faults = FakeFaults::default();
        faults
            .driver_updates
            .borrow_mut()
            .extend([FakeUpdateFault::Fail, FakeUpdateFault::Fail]);
        let mut runtime = fake_runtime_with_faults(
            SharedLog::default(),
            Rc::new(Cell::new(false)),
            faults.clone(),
        );
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let initial = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let resized = SurfaceViewport::new(SurfaceSize::new(640, 480), 3.0);
        runtime
            .attach_surface_with_viewport(webview, HostSurface::new("rollback-slot"), initial)
            .unwrap();

        let error = runtime
            .update_surface_viewport(webview, resized)
            .expect_err("failed rollback must be terminal");
        assert_eq!(
            error,
            ExplorerKitError::Host(poisoned_webview_error(webview))
        );
        assert_eq!(faults.drops.get(), 1);
        assert_eq!(
            runtime.reload(webview),
            Err(ExplorerKitError::Host(poisoned_webview_error(webview)))
        );
    }

    #[test]
    fn panicking_render_target_rollback_terminally_poisons_the_webview() {
        let faults = FakeFaults::default();
        faults
            .driver_updates
            .borrow_mut()
            .push_back(FakeUpdateFault::Fail);
        faults
            .target_updates
            .borrow_mut()
            .extend([FakeUpdateFault::Pass, FakeUpdateFault::Panic]);
        let mut runtime = fake_runtime_with_faults(
            SharedLog::default(),
            Rc::new(Cell::new(false)),
            faults.clone(),
        );
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let initial = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let resized = SurfaceViewport::new(SurfaceSize::new(640, 480), 3.0);
        runtime
            .attach_surface_with_viewport(webview, HostSurface::new("panic-rollback-slot"), initial)
            .unwrap();

        assert!(catch_unwind(AssertUnwindSafe(|| {
            runtime.update_surface_viewport(webview, resized).unwrap();
        }))
        .is_err());
        assert_eq!(faults.drops.get(), 1);
        assert_eq!(
            runtime.reload(webview),
            Err(ExplorerKitError::Host(poisoned_webview_error(webview)))
        );
    }
}
