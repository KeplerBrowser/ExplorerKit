//! macOS host adapter for create-time Servo or system-WebView selection.

#[cfg(feature = "macos-webkit")]
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
#[cfg(feature = "macos-webkit")]
use std::collections::VecDeque;
#[cfg(feature = "macos-webkit")]
use std::ffi::c_void;
#[cfg(feature = "macos-webkit")]
use std::ptr::NonNull;
use std::rc::Rc;
#[cfg(feature = "macos-webkit")]
use std::rc::Weak;

#[cfg(feature = "macos-webkit")]
use dpi::{PhysicalPosition, PhysicalSize};
use explorerkit_embedder::site_data::{
    SiteDataError, SiteDataErrorKind, SiteDataOperation, SiteDataScope, SiteDataSelection,
    SiteDataSnapshot, SiteDataWriteSummary,
};
use explorerkit_embedder::{
    ConfigurableHost, ContextMenuAction, Host, HostError, HostEvent, HostInputEvent,
    NewWebViewDecision, NewWebViewRequest, PopupRequestPolicy, SessionHandle, WebViewCommand,
    WebViewHandle, WebViewRegistrar,
};
#[cfg(feature = "macos-webkit")]
use explorerkit_embedder::{JavaScriptEvaluationErrorKind, LoadStatusKind, PopupCreated};
use explorerkit_host::{
    HostSurface, NativeSurface, SurfaceDelegate, SurfaceError, SurfaceSize, SurfaceViewport,
};
#[cfg(feature = "macos-webkit")]
use objc2::{msg_send, rc::Retained, sel, MainThreadMarker, MainThreadOnly, Message};
#[cfg(feature = "macos-webkit")]
use objc2_app_kit::{NSAutoresizingMaskOptions, NSView};
#[cfg(feature = "macos-webkit")]
use objc2_foundation::{NSObject, NSObjectProtocol};
use raw_window_handle::HasWindowHandle;
#[cfg(feature = "macos-webkit")]
use raw_window_handle::{AppKitWindowHandle, HandleError, RawWindowHandle, WindowHandle};
#[cfg(feature = "macos-webkit")]
use wry::{
    PageLoadEvent, PermissionResponse, Rect, WebView, WebViewBuilder, WebViewBuilderExtDarwin,
    WebViewBuilderExtMacos, WebViewExtMacOS,
};

#[cfg(feature = "macos-webkit")]
use crate::surface::SurfaceEventLoopWaker;
#[cfg(feature = "servo")]
use crate::surface::SurfaceHost;
use crate::surface::{SurfaceFrame, SurfaceHostOptions};

#[cfg(feature = "macos-webkit")]
mod configuration;
#[cfg(feature = "macos-webkit")]
mod favicon;
#[cfg(feature = "macos-webkit")]
mod javascript;
#[cfg(feature = "macos-webkit")]
mod native_callbacks;
#[cfg(feature = "macos-webkit")]
mod observation;
#[cfg(feature = "macos-webkit")]
mod site_data;

#[cfg(feature = "macos-webkit-automation")]
pub mod automation;

#[cfg(feature = "macos-webkit")]
use configuration::SystemWebViewContext;
#[cfg(feature = "macos-webkit")]
use native_callbacks::SystemUiDelegateRegistration;
#[cfg(feature = "macos-webkit")]
use objc2_web_kit::{WKNavigationAction, WKWebView, WKWebViewConfiguration, WKWindowFeatures};

#[cfg(feature = "macos-webkit")]
use javascript::{evaluation_event, NativeEvaluationError};

/// Stable WKWebView website-data store identity supplied by the host.
///
/// The identifier belongs only to WebKit. It is not a Servo profile and does
/// not imply storage or credential transfer between engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WebKitDataStoreIdentifier(pub [u8; 16]);

/// macOS engine view selected when a runtime view is created.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum MacOsWebViewKind {
    /// Create the existing Servo-backed view.
    #[default]
    Servo,
    /// Create a WRY-owned child WKWebView.
    SystemWebView,
}

/// Returns the engine integrations compiled into this macOS facade build.
///
/// The order is Servo followed by the system WebView when both are compiled.
/// This does not initialize or probe an engine, promise successful startup, or
/// report engine capabilities or live engine processes. It does not inventory
/// engine libraries elsewhere in the application. macOS supplies the system
/// WebKit engine itself.
/// Runtime view selection must still choose one of these compiled integrations.
pub fn compiled_engines() -> &'static [MacOsWebViewKind] {
    &[
        #[cfg(feature = "servo")]
        MacOsWebViewKind::Servo,
        #[cfg(feature = "macos-webkit")]
        MacOsWebViewKind::SystemWebView,
    ]
}

/// Create-time options for [`MacOsViewHost`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MacOsWebViewOptions {
    /// Selects the concrete engine view for this handle.
    pub kind: MacOsWebViewKind,
    /// Opts this system view into the macOS native automation and prompt paths.
    /// Requires `macos-webkit-automation`; unsupported for Servo views.
    pub webkit_automation: bool,
}

impl MacOsWebViewOptions {
    /// Creates options for one concrete macOS engine view.
    pub fn new(kind: MacOsWebViewKind) -> Self {
        Self {
            kind,
            webkit_automation: false,
        }
    }

    /// Chooses native WebKit automation eligibility before native allocation.
    pub fn with_webkit_automation(mut self, enabled: bool) -> Self {
        self.webkit_automation = enabled;
        self
    }
}

/// Construction options for [`MacOsViewHost`].
pub struct MacOsViewHostOptions {
    surface_host: SurfaceHostOptions,
    webkit_data_store_identifier: WebKitDataStoreIdentifier,
    new_webview_destination_handler: Option<Rc<NewWebViewDestinationHandler>>,
    #[cfg(feature = "macos-webkit-automation")]
    automation_selection: Option<Rc<dyn Fn(WebViewHandle)>>,
}

type NewWebViewDestinationHandler =
    dyn Fn(&NewWebViewRequest) -> Result<AppKitNewWebViewDestination, SurfaceError>;
#[cfg(feature = "macos-webkit")]
type NewWebViewHandler = dyn Fn(&NewWebViewRequest) -> NewWebViewDecision;
#[cfg(feature = "macos-webkit")]
type SystemChildViews = RefCell<HashMap<WebViewHandle, Box<SystemWebViewState>>>;

/// An already mounted app-owned destination for an original native child.
///
/// The retained parent must own its AppKit view and keep its containing window
/// valid until successful detach or destruction. ExplorerKit owns only the
/// native webview and its inner container, not the app window or layout policy.
#[cfg_attr(not(feature = "macos-webkit"), allow(dead_code))]
pub struct AppKitNewWebViewDestination {
    surface: HostSurface,
    viewport: SurfaceViewport,
    parent: Rc<dyn HasWindowHandle>,
}

impl AppKitNewWebViewDestination {
    /// Supplies the surface identity, physical viewport and retained native parent.
    pub fn new(
        surface: HostSurface,
        viewport: SurfaceViewport,
        parent: Rc<dyn HasWindowHandle>,
    ) -> Self {
        Self {
            surface,
            viewport,
            parent,
        }
    }
}

impl MacOsViewHostOptions {
    /// Combines the existing Servo surface-host inputs with a WebKit-only store identity.
    pub fn new(
        surface_host: SurfaceHostOptions,
        webkit_data_store_identifier: WebKitDataStoreIdentifier,
    ) -> Self {
        Self {
            surface_host,
            webkit_data_store_identifier,
            new_webview_destination_handler: None,
            #[cfg(feature = "macos-webkit-automation")]
            automation_selection: None,
        }
    }

    /// Supplies an attached AppKit destination after a native request is allowed.
    ///
    /// The handler runs synchronously on the AppKit thread. It may mount a slot
    /// using independently retained native parents and placement data, but must
    /// not reenter Runtime/GUI state, wait for rendering, or run a nested event
    /// loop. Dropping its returned parent releases a failed provisional slot.
    ///
    /// A SystemWebView creation handler also needs to be installed on the source.
    /// Native children keep WebKit's original configuration and request.
    #[allow(
        clippy::type_complexity,
        reason = "Keep the native destination callback explicit."
    )]
    pub fn with_new_webview_destination_handler(
        mut self,
        handler: Rc<
            dyn Fn(&NewWebViewRequest) -> Result<AppKitNewWebViewDestination, SurfaceError>,
        >,
    ) -> Self {
        self.new_webview_destination_handler = Some(handler);
        self
    }
}

/// Supplies the app-owned AppKit parent view used by a system-WebView attachment.
///
/// The returned [`NativeSurface`] is borrowed from the host. ExplorerKit creates a
/// WRY/WKWebView child inside it and never takes ownership of the app window.
pub trait AppKitViewDelegate: SurfaceDelegate {
    /// Returns the live AppKit parent for this app-owned surface slot.
    fn appkit_parent(
        &mut self,
        surface: &HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<NativeSurface<'_>, SurfaceError>;
}

#[cfg(feature = "macos-webkit")]
type CallbackTarget = (u64, u64);

#[cfg(feature = "macos-webkit")]
struct SystemCallbacks {
    views: HashMap<CallbackTarget, SystemCallbackState>,
    wake: SurfaceEventLoopWaker,
    #[cfg(feature = "macos-webkit-automation")]
    lifetime_sequence: u64,
    #[cfg(test)]
    liveness_checks: Cell<usize>,
}

#[cfg(feature = "macos-webkit")]
impl SystemCallbacks {
    fn new(wake: SurfaceEventLoopWaker) -> Self {
        Self {
            views: HashMap::new(),
            wake,
            #[cfg(feature = "macos-webkit-automation")]
            lifetime_sequence: 0,
            #[cfg(test)]
            liveness_checks: Cell::new(0),
        }
    }
}

#[derive(Default)]
#[cfg(feature = "macos-webkit")]
struct SystemCallbackState {
    #[cfg(feature = "macos-webkit-automation")]
    automation: automation::ViewState,
    observed_url: Option<String>,
    observed_navigation_state: Option<(bool, bool)>,
    events: VecDeque<HostEvent>,
    document_ready: bool,
    document_navigation_pending: bool,
    next_request: u64,
    pending: HashMap<String, u64>,
    favicon_request: u64,
    favicon_selection: favicon::Selection,
    favicons: Option<favicon::IconState>,
}

#[cfg(feature = "macos-webkit")]
impl SystemCallbackState {
    fn retire_evaluations(&mut self, error: JavaScriptEvaluationErrorKind) {
        for (id, _) in self.pending.drain() {
            self.events.push_back(evaluation_event(id, Err(error)));
        }
    }
}

#[derive(Clone)]
#[cfg(feature = "macos-webkit")]
struct SystemCallbackSink {
    target: CallbackTarget,
    callbacks: Rc<RefCell<SystemCallbacks>>,
}

#[cfg(feature = "macos-webkit")]
impl SystemCallbackSink {
    fn register(
        callbacks: Rc<RefCell<SystemCallbacks>>,
        webview: WebViewHandle,
        generation: u64,
    ) -> Self {
        let target = (webview.raw(), generation);
        {
            let mut registry = callbacks.borrow_mut();
            #[cfg(feature = "macos-webkit-automation")]
            {
                registry.lifetime_sequence += 1;
                let born = registry.lifetime_sequence;
                let state = &mut registry.views.entry(target).or_default().automation;
                state.born = born;
                state.handle = Some(webview);
            }
            #[cfg(not(feature = "macos-webkit-automation"))]
            registry.views.entry(target).or_default();
        }
        Self { target, callbacks }
    }

    #[cfg(test)]
    fn register_raw(
        callbacks: Rc<RefCell<SystemCallbacks>>,
        webview: u64,
        generation: u64,
    ) -> Self {
        let target = (webview, generation);
        callbacks.borrow_mut().views.entry(target).or_default();
        Self { target, callbacks }
    }

    fn wake(&self) {
        let wake = self.callbacks.borrow().wake.clone();
        wake();
    }

    fn publish(&self, update: impl FnOnce(&mut SystemCallbackState) -> bool) {
        {
            let mut callbacks = self.callbacks.borrow_mut();
            let Some(state) = callbacks.views.get_mut(&self.target) else {
                return;
            };
            if !update(state) {
                return;
            }
        }
        self.wake();
    }

    fn push(&self, event: HostEvent) {
        self.publish(|state| {
            state.events.push_back(event);
            true
        });
    }

    fn document_committed(&self) {
        let mut pending = Vec::new();
        self.publish(|state| {
            state.document_ready = true;
            state.document_navigation_pending = false;
            state.favicon_request = state.favicon_request.wrapping_add(1);
            pending = state.favicon_selection.retire();
            state.events.push_back(HostEvent::LoadStatusChanged {
                status: LoadStatusKind::Started,
            });
            if let Some(favicons) = &mut state.favicons {
                let favicon = favicons.observe(true, true);
                state
                    .events
                    .push_back(HostEvent::FaviconChanged { favicon });
            }
            // Completion linearizes when it enters this queue. Already finalized
            // results stay before Started, including across a same-URL reload.
            state.retire_evaluations(JavaScriptEvaluationErrorKind::DocumentNotFound);
            true
        });
        favicon::decline(pending);
    }

    fn process_terminated(&self) {
        #[cfg(feature = "macos-webkit-automation")]
        automation::dialogs::retire(self);
        let mut pending = Vec::new();
        self.publish(|state| {
            state.document_ready = false;
            if let Some(favicons) = &mut state.favicons {
                favicons.clear();
            }
            state.favicon_request = state.favicon_request.wrapping_add(1);
            pending = state.favicon_selection.retire();
            state.events.push_back(HostEvent::Crashed {
                url: None,
                reason: "WebKit web content process terminated".to_owned(),
                backtrace: None,
            });
            state.retire_evaluations(JavaScriptEvaluationErrorKind::InternalError);
            true
        });
        favicon::decline(pending);
    }

    fn begin_evaluation(&self, id: &str) -> Result<Option<u64>, HostError> {
        let mut callbacks = self.callbacks.borrow_mut();
        let state = callbacks
            .views
            .get_mut(&self.target)
            .ok_or_else(|| HostError::new("system webview lifetime is no longer live"))?;
        if id.trim().is_empty() {
            return Err(HostError::new("JavaScript evaluation id must not be empty"));
        }
        if state.pending.contains_key(id) {
            return Err(HostError::new(format!(
                "JavaScript evaluation {id} is already pending in this webview"
            )));
        }
        if !state.document_ready {
            state.events.push_back(evaluation_event(
                id,
                Err(JavaScriptEvaluationErrorKind::WebViewNotReady),
            ));
            drop(callbacks);
            self.wake();
            return Ok(None);
        }
        state.next_request = state
            .next_request
            .checked_add(1)
            .ok_or_else(|| HostError::new("JavaScript evaluation token space exhausted"))?;
        let token = state.next_request;
        state.pending.insert(id.to_owned(), token);
        Ok(Some(token))
    }

    fn complete_evaluation(
        &self,
        id: &str,
        token: u64,
        result: Result<String, NativeEvaluationError>,
    ) {
        let mut pending = Vec::new();
        self.publish(|state| {
            // Commit/process retirement removes every old token. Check before any
            // state change: even an old process error must not invalidate a new read.
            if state.pending.get(id) != Some(&token) {
                return false;
            }
            match result {
                Err(
                    NativeEvaluationError::ProcessTerminated
                    | NativeEvaluationError::ViewInvalidated,
                ) => {
                    state.document_ready = false;
                    if let Some(favicons) = &mut state.favicons {
                        favicons.clear();
                    }
                    state.favicon_request = state.favicon_request.wrapping_add(1);
                    pending = state.favicon_selection.retire();
                    state.retire_evaluations(JavaScriptEvaluationErrorKind::InternalError);
                }
                result => {
                    state.pending.remove(id);
                    state.events.push_back(evaluation_event(
                        id,
                        result.map_err(|error| match error {
                            NativeEvaluationError::Result(error) => error,
                            _ => unreachable!("lifetime errors handled above"),
                        }),
                    ));
                }
            }
            true
        });
        favicon::decline(pending);
    }

    fn navigation_requested(&self) {
        // Capture silent same-document aliases before policy guards the departing page.
        self.observe_favicon_history();
        if let Some(state) = self.callbacks.borrow_mut().views.get_mut(&self.target) {
            // Do not infer same-document lineage from a list change between
            // navigation policy and commit. A failed provisional navigation can
            // leave this conservative guard set until the next real commit.
            state.document_navigation_pending = true;
        }
    }

    fn enable_favicons(&self, view: &objc2_web_kit::WKWebView) {
        if let Some(state) = self.callbacks.borrow_mut().views.get_mut(&self.target) {
            state.favicons = Some(favicon::IconState::new(view));
        }
    }

    fn observe_favicon_history(&self) {
        if let Some(state) = self.callbacks.borrow_mut().views.get_mut(&self.target) {
            if state.document_ready {
                if let Some(favicons) = &mut state.favicons {
                    favicons.observe(false, !state.document_navigation_pending);
                }
            }
        }
    }

    fn native_state_observed(&self, url: Option<String>, navigation: (bool, bool)) {
        self.observe_favicon_history();
        self.publish(|state| {
            // A nil URL after process termination must not hide queued results.
            if let Some(url) = url {
                state.observed_url = Some(url);
            }
            state.observed_navigation_state = Some(navigation);
            true
        });
    }

    fn complete_native_favicon(
        &self,
        token: u64,
        image: explorerkit_embedder::FaviconImage,
        bytes: Vec<u8>,
        candidate: favicon::Candidate,
    ) {
        self.publish(|state| {
            if state.document_ready
                && state.favicon_request == token
                && state
                    .favicon_selection
                    .accept(candidate, image.width, image.height)
            {
                if let Some(favicons) = &mut state.favicons {
                    favicons.retain(bytes, !state.document_navigation_pending);
                }
                state.events.push_back(HostEvent::FaviconChanged {
                    favicon: Some(image),
                });
                return true;
            }
            false
        });
    }

    fn begin_favicon(&self) -> Option<u64> {
        let callbacks = self.callbacks.borrow();
        let state = callbacks.views.get(&self.target)?;
        state.document_ready.then_some(state.favicon_request)
    }

    fn favicon_lifetime_error(&self, token: u64) {
        let mut pending = Vec::new();
        self.publish(|state| {
            if !state.document_ready || state.favicon_request != token {
                return false;
            }
            state.document_ready = false;
            state.favicon_request = state.favicon_request.wrapping_add(1);
            pending = state.favicon_selection.retire();
            if let Some(favicons) = &mut state.favicons {
                favicons.clear();
            }
            state.retire_evaluations(JavaScriptEvaluationErrorKind::InternalError);
            true
        });
        favicon::decline(pending);
    }

    fn favicon_is_current(&self, token: u64) -> bool {
        self.callbacks
            .borrow()
            .views
            .get(&self.target)
            .is_some_and(|state| state.document_ready && state.favicon_request == token)
    }

    #[cfg(test)]
    fn complete_favicon(&self, token: u64, image: explorerkit_embedder::FaviconImage) {
        self.complete_native_favicon(token, image, Vec::new(), favicon::Candidate::Compatibility);
    }

    fn drain(&self) -> Vec<HostEvent> {
        self.callbacks
            .borrow_mut()
            .views
            .get_mut(&self.target)
            .map(|state| state.events.drain(..).collect())
            .unwrap_or_default()
    }

    fn clear(&self) {
        if let Some(state) = self.callbacks.borrow_mut().views.get_mut(&self.target) {
            state.events.clear();
        }
    }

    fn invalidate(&self) {
        #[cfg(feature = "macos-webkit-automation")]
        automation::dialogs::retire(self);
        let previous = self.callbacks.borrow_mut().views.remove(&self.target);
        if let Some(mut previous) = previous {
            favicon::decline(previous.favicon_selection.retire());
        }
    }

    fn is_live(&self) -> bool {
        let callbacks = self.callbacks.borrow();
        #[cfg(test)]
        callbacks
            .liveness_checks
            .set(callbacks.liveness_checks.get() + 1);
        callbacks.views.contains_key(&self.target)
    }
}

enum MacOsView {
    #[cfg(feature = "servo")]
    Servo,
    #[cfg(feature = "macos-webkit")]
    System(Box<SystemWebViewState>),
}

#[cfg(feature = "macos-webkit")]
struct SystemWebViewState {
    generation: u64,
    ui_registration: Option<SystemUiDelegateRegistration>,
    #[cfg(test)]
    test_callback_depth: Cell<usize>,
    icon_registration: Option<favicon::IconRegistration>,
    observation: Option<observation::Registration>,
    webview: Option<WebView>,
    container: Option<AppKitContainer>,
    destination_parent: Option<Rc<dyn HasWindowHandle>>,
    handler: Rc<RefCell<Option<Rc<NewWebViewHandler>>>>,
    pending_commands: VecDeque<WebViewCommand>,
    viewport: Option<SurfaceViewport>,
    last_url: Option<String>,
    last_navigation_state: Option<(bool, bool)>,
    capture: Option<site_data::Capture>,
}

#[cfg(feature = "macos-webkit")]
impl SystemWebViewState {
    fn ui_callback_is_active(&self) -> bool {
        #[cfg(test)]
        if self.test_callback_depth.get() != 0 {
            return true;
        }
        self.ui_registration
            .as_ref()
            .is_some_and(|ui| ui.is_active())
    }

    fn new(generation: u64) -> Self {
        Self {
            generation,
            ui_registration: None,
            #[cfg(test)]
            test_callback_depth: Cell::new(0),
            icon_registration: None,
            observation: None,
            webview: None,
            container: None,
            destination_parent: None,
            handler: Rc::default(),
            pending_commands: VecDeque::new(),
            viewport: None,
            last_url: None,
            last_navigation_state: None,
            capture: None,
        }
    }
}

#[derive(Clone)]
#[cfg(feature = "macos-webkit")]
struct SystemNewWebView {
    source: WebViewHandle,
    sink: SystemCallbackSink,
    registrar: Option<WebViewRegistrar>,
    pending: Weak<SystemChildViews>,
    destination: Option<Rc<NewWebViewDestinationHandler>>,
    handler: Rc<RefCell<Option<Rc<NewWebViewHandler>>>>,
    next_request: Rc<Cell<u64>>,
}

#[cfg(feature = "macos-webkit")]
impl SystemNewWebView {
    fn request(
        &self,
        opener: &WKWebView,
        configuration: &WKWebViewConfiguration,
        action: &WKNavigationAction,
        features: &WKWindowFeatures,
    ) -> Option<Retained<WKWebView>> {
        if !self.sink.is_live() {
            return None;
        }
        let sequence = self.next_request.get().checked_add(1)?;
        self.next_request.set(sequence);
        // SAFETY: WebKit supplies live native objects on its AppKit thread.
        let parent_url = unsafe { opener.URL() }
            .and_then(|url| url.absoluteString())
            .map(|url| url.to_string());
        let request = NewWebViewRequest {
            source: self.source,
            request_id: format!("{}:{}:{sequence}", self.source.raw(), self.sink.target.1),
            parent_url,
            target_url: unsafe { action.request().URL() }
                .and_then(|url| url.absoluteString())
                .map(|url| url.to_string()),
            window_features: window_features(features),
            wants_popup: wants_popup(features),
        };
        let handler = self.handler.borrow().clone();
        if handler.as_ref().map(|handler| handler(&request)) != Some(NewWebViewDecision::Create) {
            self.sink.push(HostEvent::PopupRequested {
                request_id: request.request_id,
                parent_webview_id: format!("{:p}", opener as *const WKWebView),
                parent_url: request.parent_url,
                target_url: request.target_url,
                window_features: request.window_features,
                wants_popup: request.wants_popup,
                policy: PopupRequestPolicy::DefaultDeny,
            });
            return None;
        }
        match self.create(
            &request,
            opener,
            configuration,
            handler.expect("Create has a handler"),
        ) {
            Ok(native) => Some(native),
            Err(error) => {
                self.sink.push(HostEvent::PopupCreationFailed {
                    request_id: request.request_id,
                    error_message: error.to_string(),
                });
                None
            }
        }
    }

    fn closed(&self) {
        if !self.sink.is_live() {
            return;
        }
        #[cfg(feature = "macos-webkit-automation")]
        automation::dialogs::retire(&self.sink);
        let events = self.sink.drain();
        self.sink.invalidate();
        if let Some(registrar) = &self.registrar {
            for event in events {
                registrar.record_event(self.source, event);
            }
            registrar.retire_webview(self.source);
            self.sink.wake();
        }
    }

    fn create(
        &self,
        request: &NewWebViewRequest,
        opener: &WKWebView,
        configuration: &WKWebViewConfiguration,
        handler: Rc<NewWebViewHandler>,
    ) -> Result<Retained<objc2_web_kit::WKWebView>, HostError> {
        let pending = self
            .pending
            .upgrade()
            .ok_or_else(|| HostError::new("native host has closed"))?;
        let registrar = self
            .registrar
            .as_ref()
            .ok_or_else(|| HostError::new("native host has no Runtime registrar"))?;
        let registration = registrar.reserve_child(self.source, request.request_id.clone())?;
        let destination =
            self.destination.as_ref().ok_or_else(|| {
                HostError::new("no AppKit child destination handler is installed")
            })?(request)
            .map_err(|error| HostError::new(error.to_string()))?;
        validate_destination(&destination)?;
        let child = registration.webview();
        let sink = SystemCallbackSink::register(self.sink.callbacks.clone(), child, 1);
        #[cfg(feature = "macos-webkit-automation")]
        let automation = self
            .sink
            .callbacks
            .borrow()
            .views
            .get(&self.sink.target)
            .map(|state| (state.automation.owner.clone(), state.automation.eligible));
        #[cfg(feature = "macos-webkit-automation")]
        if let Some((owner, enabled)) = &automation {
            if let Some(owner) = owner.upgrade() {
                owner.initialize_view(&sink, *enabled);
            }
            if *enabled {
                automation::native::configure(configuration, true);
            }
        }
        let mut state = Box::new(SystemWebViewState::new(1));
        *state.handler.borrow_mut() = Some(handler);
        let callbacks = Self {
            source: child,
            sink: sink.clone(),
            registrar: self.registrar.clone(),
            pending: self.pending.clone(),
            destination: self.destination.clone(),
            handler: state.handler.clone(),
            next_request: Rc::default(),
        };
        let built = (|| {
            let container = AppKitContainer::new(&destination.parent)?;
            let native = system_webview_builder(sink.clone())
                .with_webview_configuration(configuration.retain())
                .with_bounds(wry_rect(destination.viewport))
                .with_focused(false)
                .build_as_child(&container)
                .map_err(wry_error)?;
            let ui = SystemUiDelegateRegistration::attach(&native, callbacks)?;
            Ok::<_, HostError>((ui, native, container))
        })();
        let (ui, native, container) = match built {
            Ok(built) => built,
            Err(error) => {
                sink.invalidate();
                return Err(error);
            }
        };
        let native_view = native.webview().into_super();
        #[cfg(feature = "macos-webkit-automation")]
        if let Some((owner, _)) = automation {
            if let Some(owner) = owner.upgrade() {
                owner.attached(&sink, child, &native_view);
            }
        }
        let created = PopupCreated {
            request_id: request.request_id.clone(),
            parent_webview_id: format!("{:p}", opener as *const WKWebView),
            child_webview_id: format!("{:p}", Retained::as_ptr(&native_view)),
            parent_url: request.parent_url.clone(),
            target_url: request.target_url.clone(),
            window_features: request.window_features.clone(),
            wants_popup: request.wants_popup,
        };
        state.ui_registration = Some(ui);
        state.icon_registration = favicon::IconRegistration::attach(&native, sink.clone());
        state.observation = Some(observation::Registration::attach(&native, sink));
        state.webview = Some(native);
        state.container = Some(container);
        state.destination_parent = Some(destination.parent);
        state.viewport = Some(destination.viewport);
        // No native call or last-owner drop occurs under this short staging borrow.
        let previous = pending.borrow_mut().insert(child, state);
        debug_assert!(previous.is_none(), "reserved child handles are unique");
        drop(previous);
        registration.commit_with_surface(created, destination.surface, destination.viewport);
        self.sink.wake();
        Ok(native_view)
    }
}

#[cfg(feature = "macos-webkit")]
fn validate_destination(destination: &AppKitNewWebViewDestination) -> Result<(), HostError> {
    MainThreadMarker::new()
        .ok_or_else(|| HostError::new("native child must be created on the AppKit main thread"))?;
    let viewport = destination.viewport;
    if viewport.size.width == 0
        || viewport.size.height == 0
        || !viewport.scale_factor.is_finite()
        || viewport.scale_factor <= 0.0
    {
        return Err(HostError::new(
            "native child destination needs a positive viewport and scale factor",
        ));
    }
    let parent = appkit_parent_view(&destination.parent)?;
    if parent.window().and_then(|window| window.screen()).is_none() {
        return Err(HostError::new(
            "native child destination is not attached to a window and screen",
        ));
    }
    Ok(())
}

// WebKit's private computed preference, never the optional explicit `_popup` token.
#[cfg(feature = "macos-webkit")]
fn wants_popup(features: &NSObject) -> Option<bool> {
    if !features.respondsToSelector(sel!(_wantsPopup)) {
        return None;
    }
    // SAFETY: The guarded WebKit getter returns Objective-C BOOL. objc2 maps bool
    // to that ABI; no OS-version annotation is used as proof of availability.
    Some(unsafe { msg_send![features, _wantsPopup] })
}

#[cfg(feature = "macos-webkit")]
fn window_features(features: &WKWindowFeatures) -> Option<String> {
    let mut values = Vec::new();
    // SAFETY: WebKit lends these feature hints for the creation callback.
    unsafe {
        if let (Some(width), Some(height)) = (features.width(), features.height()) {
            values.extend([
                format!("width={}", width.doubleValue()),
                format!("height={}", height.doubleValue()),
            ]);
        }
        if let (Some(x), Some(y)) = (features.x(), features.y()) {
            values.extend([
                format!("left={}", x.doubleValue()),
                format!("top={}", y.doubleValue()),
            ]);
        }
    }
    (!values.is_empty()).then(|| values.join(","))
}

/// One macOS runtime host that owns view lifetimes for the compiled Servo
/// and/or WRY/WKWebView integrations.
///
/// The application still owns its top-level window, layout slot, chrome, tabs,
/// and engine-selection policy. Each [`WebViewHandle`] identifies exactly one
/// concrete engine-view lifetime.
pub struct MacOsViewHost<S> {
    #[cfg(feature = "servo")]
    servo: SurfaceHost<S>,
    #[cfg(not(feature = "servo"))]
    services: S,
    #[cfg(feature = "macos-webkit")]
    webkit_data_store_identifier: WebKitDataStoreIdentifier,
    views: HashMap<WebViewHandle, MacOsView>,
    #[cfg(feature = "macos-webkit")]
    system_context: Option<SystemWebViewContext>,
    #[cfg(feature = "macos-webkit")]
    callbacks: Rc<RefCell<SystemCallbacks>>,
    #[cfg(feature = "macos-webkit")]
    registrar: Option<WebViewRegistrar>,
    #[cfg(feature = "macos-webkit")]
    next_generation: u64,
    #[cfg(feature = "macos-webkit")]
    pending_children: Rc<SystemChildViews>,
    #[cfg(feature = "macos-webkit")]
    new_webview_destination_handler: Option<Rc<NewWebViewDestinationHandler>>,
    #[cfg(feature = "macos-webkit-automation")]
    automation: Rc<automation::State>,
}

impl<S> MacOsViewHost<S> {
    /// Creates the macOS host with existing Servo surface services and one WebKit store.
    pub fn new(services: S, options: MacOsViewHostOptions) -> Self {
        #[cfg(feature = "macos-webkit")]
        let wake = options.surface_host.event_loop_waker().clone();
        #[cfg(feature = "servo")]
        let servo = {
            let mut servo = SurfaceHost::new(services, options.surface_host);
            servo.set_navigation_state_events();
            servo
        };
        #[cfg(not(feature = "macos-webkit"))]
        let _ = (
            options.webkit_data_store_identifier,
            options.new_webview_destination_handler,
        );
        #[cfg(feature = "macos-webkit")]
        let callbacks = Rc::new(RefCell::new(SystemCallbacks::new(wake)));
        #[cfg(feature = "macos-webkit-automation")]
        let automation = automation::State::new(callbacks.clone(), options.automation_selection);
        Self {
            #[cfg(feature = "macos-webkit-automation")]
            automation,
            #[cfg(feature = "servo")]
            servo,
            #[cfg(not(feature = "servo"))]
            services,
            #[cfg(feature = "macos-webkit")]
            webkit_data_store_identifier: options.webkit_data_store_identifier,
            views: HashMap::new(),
            #[cfg(feature = "macos-webkit")]
            system_context: None,
            #[cfg(feature = "macos-webkit")]
            callbacks,
            #[cfg(feature = "macos-webkit")]
            registrar: None,
            #[cfg(feature = "macos-webkit")]
            next_generation: 0,
            #[cfg(feature = "macos-webkit")]
            pending_children: Rc::default(),
            #[cfg(feature = "macos-webkit")]
            new_webview_destination_handler: options.new_webview_destination_handler,
        }
    }

    /// Borrows the app-provided surface and AppKit-parent delegate.
    pub fn services(&self) -> &S {
        #[cfg(feature = "servo")]
        {
            self.servo.services()
        }
        #[cfg(not(feature = "servo"))]
        {
            &self.services
        }
    }

    /// Mutably borrows the app-provided surface and AppKit-parent delegate.
    pub fn services_mut(&mut self) -> &mut S {
        #[cfg(feature = "servo")]
        {
            self.servo.services_mut()
        }
        #[cfg(not(feature = "servo"))]
        {
            &mut self.services
        }
    }

    #[allow(
        clippy::type_complexity,
        reason = "Preserve the host batch-update result shape."
    )]
    #[cfg(all(feature = "servo", feature = "macos-webkit"))]
    fn finish_servo_pump(
        &self,
        _webviews: &[WebViewHandle],
        result: Result<Vec<(WebViewHandle, Result<Vec<HostEvent>, HostError>)>, HostError>,
    ) -> Result<Vec<(WebViewHandle, Result<Vec<HostEvent>, HostError>)>, HostError> {
        #[cfg(feature = "macos-webkit")]
        if result.is_err() {
            if let Some(registrar) = &self.registrar {
                for &webview in _webviews {
                    if let Some(MacOsView::System(state)) = self.views.get(&webview) {
                        for event in self.system_sink(webview, state.generation).drain() {
                            registrar.record_event(webview, event);
                        }
                    }
                }
            }
        }
        result
    }

    #[cfg(feature = "macos-webkit")]
    fn next_generation(&mut self) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1);
        self.next_generation
    }

    fn sync_views(&mut self) {
        // Callbacks stage complete owners without borrowing this host. End the
        // staging guard before transfer or destruction can call native code.
        #[cfg(feature = "macos-webkit")]
        {
            let children = std::mem::take(&mut *self.pending_children.borrow_mut());
            for (handle, state) in children {
                if self.system_sink(handle, state.generation).is_live()
                    || state.ui_callback_is_active()
                {
                    self.views.insert(handle, MacOsView::System(state));
                }
            }
            let retired: Vec<_> = self
                .views
                .iter()
                .filter_map(|(&handle, view)| match view {
                    MacOsView::System(state)
                        if !self.system_sink(handle, state.generation).is_live()
                            && !state.ui_callback_is_active() =>
                    {
                        Some(handle)
                    }
                    _ => None,
                })
                .collect();
            let retired: Vec<_> = retired
                .into_iter()
                .filter_map(|handle| self.views.remove(&handle))
                .collect();
            drop(retired);
            #[cfg(feature = "macos-webkit-automation")]
            if self.automation.retirement_pending.get() {
                // Destruction above can itself stage callbacks. Do not release
                // the barrier while any retired owner or UI callback survives.
                let blocked = self
                    .pending_children
                    .borrow()
                    .iter()
                    .any(|(&handle, state)| {
                        !self.system_sink(handle, state.generation).is_live()
                            || state.ui_callback_is_active()
                    })
                    || self.views.iter().any(|(&handle, view)| match view {
                        MacOsView::System(state) => {
                            !self.system_sink(handle, state.generation).is_live()
                                || state.ui_callback_is_active()
                        }
                        #[cfg(feature = "servo")]
                        MacOsView::Servo => false,
                    });
                if !blocked {
                    self.automation.retirement_pending.set(false);
                    // Re-arm queued work after the ownership boundary, once.
                    self.automation.wake();
                }
            }
        }
        #[cfg(feature = "servo")]
        {
            let live = self.servo.native_view_handles();
            self.views
                .retain(|handle, view| !matches!(view, MacOsView::Servo) || live.contains(handle));
            for handle in live {
                self.views.entry(handle).or_insert(MacOsView::Servo);
            }
        }
    }

    fn view_is_servo(&mut self, webview: WebViewHandle) -> Result<bool, HostError> {
        self.sync_views();
        match self.views.get(&webview) {
            #[cfg(feature = "servo")]
            Some(MacOsView::Servo) => Ok(true),
            #[cfg(feature = "macos-webkit")]
            Some(MacOsView::System(_)) => Ok(false),
            None => Err(unknown_webview(webview)),
        }
    }

    #[cfg(feature = "macos-webkit")]
    fn system_sink(&self, webview: WebViewHandle, generation: u64) -> SystemCallbackSink {
        SystemCallbackSink {
            target: (webview.raw(), generation),
            callbacks: self.callbacks.clone(),
        }
    }

    #[cfg(feature = "macos-webkit")]
    fn reset_system_generation(&mut self, webview: WebViewHandle) {
        let old_generation = match self.views.get(&webview) {
            Some(MacOsView::System(state)) => state.generation,
            _ => return,
        };
        #[cfg(feature = "macos-webkit-automation")]
        let saved_automation = self
            .callbacks
            .borrow_mut()
            .views
            .get_mut(&(webview.raw(), old_generation))
            .map(|state| std::mem::take(&mut state.automation));
        self.system_sink(webview, old_generation).invalidate();
        let generation = self.next_generation();
        SystemCallbackSink::register(self.callbacks.clone(), webview, generation);
        #[cfg(feature = "macos-webkit-automation")]
        if let Some(mut saved) = saved_automation {
            saved.native = None;
            saved.attached = false;
            saved.context = None;
            self.callbacks
                .borrow_mut()
                .views
                .get_mut(&(webview.raw(), generation))
                .unwrap()
                .automation = saved;
        }
        if let Some(MacOsView::System(state)) = self.views.get_mut(&webview) {
            state.generation = generation;
            state.last_url = None;
            state.last_navigation_state = None;
        }
    }
}

impl<S> Drop for MacOsViewHost<S> {
    fn drop(&mut self) {
        #[cfg(feature = "macos-webkit-automation")]
        self.automation.disconnect(None);
        #[cfg(feature = "macos-webkit")]
        {
            let pending = std::mem::take(&mut *self.pending_children.borrow_mut());
            for (&webview, state) in &pending {
                self.system_sink(webview, state.generation).invalidate();
            }
            for (&webview, view) in &self.views {
                #[allow(irrefutable_let_patterns)]
                if let MacOsView::System(state) = view {
                    self.system_sink(webview, state.generation).invalidate();
                }
            }
            // WRY removes each WKWebView from its AppKit superview on drop. Do that
            // while the delegate owning those parent views is still alive.
            self.views.clear();
            drop(pending);
        }
    }
}

impl<S> ConfigurableHost for MacOsViewHost<S>
where
    S: AppKitViewDelegate + for<'a> SurfaceDelegate<Frame<'a> = SurfaceFrame<'a>>,
{
    type WebViewOptions = MacOsWebViewOptions;

    fn create_webview_with_options(
        &mut self,
        session: SessionHandle,
        webview: WebViewHandle,
        options: Self::WebViewOptions,
    ) -> Result<(), HostError> {
        if options.webkit_automation {
            if options.kind != MacOsWebViewKind::SystemWebView {
                return Err(HostError::new(
                    "WebKit automation is unavailable for Servo views",
                ));
            }
            #[cfg(not(feature = "macos-webkit-automation"))]
            return Err(HostError::new("macos-webkit-automation is not compiled"));
            #[cfg(feature = "macos-webkit-automation")]
            automation::native::check_available()?;
        }
        match options.kind {
            MacOsWebViewKind::Servo => self.create_webview(session, webview),
            MacOsWebViewKind::SystemWebView => {
                #[cfg(not(feature = "macos-webkit"))]
                {
                    Err(system_not_compiled())
                }
                #[cfg(feature = "macos-webkit")]
                {
                    if self.views.contains_key(&webview) {
                        return Err(duplicate_webview(webview));
                    }
                    let generation = self.next_generation();
                    let sink =
                        SystemCallbackSink::register(self.callbacks.clone(), webview, generation);
                    #[cfg(feature = "macos-webkit-automation")]
                    self.automation
                        .initialize_view(&sink, options.webkit_automation);
                    #[cfg(not(feature = "macos-webkit-automation"))]
                    let _ = sink;
                    self.views.insert(
                        webview,
                        MacOsView::System(Box::new(SystemWebViewState::new(generation))),
                    );
                    Ok(())
                }
            }
        }
    }
}

#[cfg_attr(
    not(all(feature = "servo", feature = "macos-webkit")),
    allow(unused_variables)
)]
impl<S> Host for MacOsViewHost<S>
where
    S: AppKitViewDelegate + for<'a> SurfaceDelegate<Frame<'a> = SurfaceFrame<'a>>,
{
    fn site_data_scope(&mut self, view: WebViewHandle) -> Result<SiteDataScope, SiteDataError> {
        self.sync_views();
        match self.views.get(&view) {
            #[cfg(feature = "servo")]
            Some(MacOsView::Servo) => self.servo.site_data_scope(view),
            #[cfg(feature = "macos-webkit")]
            Some(MacOsView::System(_)) if configuration::named_stores_available() => {
                Ok(SiteDataScope::HostStore)
            }
            #[cfg(feature = "macos-webkit")]
            Some(MacOsView::System(_)) => {
                Err(SiteDataError::admission(SiteDataErrorKind::Unavailable))
            }
            None => Err(SiteDataError::admission(SiteDataErrorKind::UnknownView)),
        }
    }

    fn capture_site_data(
        &mut self,
        source: WebViewHandle,
        selection: SiteDataSelection,
    ) -> Result<SiteDataOperation<SiteDataSnapshot>, SiteDataError> {
        #[cfg(feature = "macos-webkit")]
        {
            self.sync_views();
            if !configuration::named_stores_available() {
                return Err(SiteDataError::admission(SiteDataErrorKind::Unavailable));
            }
            let state = match self.views.get_mut(&source) {
                Some(MacOsView::System(state)) => state,
                #[cfg(feature = "servo")]
                Some(MacOsView::Servo) => {
                    return Err(SiteDataError::admission(SiteDataErrorKind::WrongEngine))
                }
                None => return Err(SiteDataError::admission(SiteDataErrorKind::UnknownView)),
            };
            if state.capture.is_some() {
                return Err(SiteDataError::admission(SiteDataErrorKind::Busy));
            }
            let native = state
                .webview
                .as_ref()
                .ok_or_else(|| SiteDataError::admission(SiteDataErrorKind::NotReady))?;
            let store = configuration::selected_store(
                &native.webview(),
                self.webkit_data_store_identifier.0,
            )
            .ok_or_else(|| SiteDataError::admission(SiteDataErrorKind::Unavailable))?;
            let wake = self.callbacks.borrow().wake.clone();
            let (operation, capture) = site_data::Capture::new(store, selection, wake);
            state.capture = capture;
            Ok(operation)
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            let _ = (source, selection);
            Err(SiteDataError::admission(SiteDataErrorKind::Unavailable))
        }
    }

    fn initialize_site_data(
        &mut self,
        destination: WebViewHandle,
        snapshot: SiteDataSnapshot,
    ) -> Result<SiteDataOperation<SiteDataWriteSummary>, SiteDataError> {
        #[cfg(feature = "servo")]
        {
            self.sync_views();
            match self.views.get(&destination) {
                Some(MacOsView::Servo) => self.servo.initialize_site_data(destination, snapshot),
                #[cfg(feature = "macos-webkit")]
                Some(MacOsView::System(_)) => {
                    Err(SiteDataError::admission(SiteDataErrorKind::WrongEngine))
                }
                None => Err(SiteDataError::admission(SiteDataErrorKind::UnknownView)),
            }
        }
        #[cfg(not(feature = "servo"))]
        {
            let _ = (destination, snapshot);
            Err(SiteDataError::admission(SiteDataErrorKind::Unavailable))
        }
    }

    fn set_webview_registrar(&mut self, registrar: WebViewRegistrar) {
        #[cfg(feature = "macos-webkit")]
        {
            self.registrar = Some(registrar.clone());
        }
        #[cfg(feature = "servo")]
        self.servo.set_webview_registrar(registrar);
    }

    fn set_new_webview_handler(
        &mut self,
        webview: WebViewHandle,
        handler: Option<Rc<dyn Fn(&NewWebViewRequest) -> NewWebViewDecision>>,
    ) -> Result<(), HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return self.servo.set_new_webview_handler(webview, handler);
        }
        #[cfg(feature = "macos-webkit")]
        {
            if handler.is_none() {
                let previous = system_state_mut(&mut self.views, webview)?
                    .handler
                    .borrow_mut()
                    .take();
                drop(previous);
                return Ok(());
            }
            if self.new_webview_destination_handler.is_none() {
                return Err(HostError::new(
                    "native child creation needs an AppKit destination handler",
                ));
            }
            let previous = system_state_mut(&mut self.views, webview)?
                .handler
                .replace(handler);
            drop(previous);
            Ok(())
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn shutdown(&mut self) -> Result<(), HostError> {
        #[cfg(feature = "macos-webkit-automation")]
        self.automation.disconnect(None);
        self.sync_views();
        #[cfg(feature = "macos-webkit")]
        {
            for (&webview, view) in &self.views {
                #[allow(irrefutable_let_patterns)]
                if let MacOsView::System(state) = view {
                    self.system_sink(webview, state.generation).invalidate();
                }
            }
            self.views.retain(|_, view| {
                #[cfg(feature = "servo")]
                {
                    matches!(view, MacOsView::Servo)
                }
                #[cfg(not(feature = "servo"))]
                {
                    let _ = view;
                    false
                }
            });
        }
        #[cfg(feature = "servo")]
        self.servo.shutdown()?;
        self.views.clear();
        Ok(())
    }

    fn create_webview(
        &mut self,
        session: SessionHandle,
        webview: WebViewHandle,
    ) -> Result<(), HostError> {
        #[cfg(not(feature = "servo"))]
        {
            Err(servo_not_compiled())
        }
        #[cfg(feature = "servo")]
        {
            if self.views.contains_key(&webview) {
                return Err(duplicate_webview(webview));
            }
            self.servo.create_webview(session, webview)?;
            self.views.insert(webview, MacOsView::Servo);
            Ok(())
        }
    }

    fn destroy_webview(&mut self, webview: WebViewHandle) -> Result<(), HostError> {
        #[cfg(feature = "macos-webkit-automation")]
        if self
            .registrar
            .as_ref()
            .is_some_and(WebViewRegistrar::is_shutting_down)
        {
            // Must precede view_is_servo: sync_views may already drop retired owners.
            self.automation.disconnect(None);
        }
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            self.servo.destroy_webview(webview)?;
            self.views.remove(&webview);
            return Ok(());
        }
        #[cfg(feature = "macos-webkit")]
        {
            let generation = match self.views.get(&webview) {
                Some(MacOsView::System(state)) => state.generation,
                _ => return Err(unknown_webview(webview)),
            };
            let sink = self.system_sink(webview, generation);
            #[cfg(feature = "macos-webkit-automation")]
            {
                automation::dialogs::retire(&sink);
                if let Some(registrar) = &self.registrar {
                    for event in sink.drain() {
                        registrar.record_event(webview, event);
                    }
                }
            }
            sink.invalidate();
            self.views.remove(&webview);
            Ok(())
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn perform_all_updates(
        &mut self,
        webviews: &[WebViewHandle],
    ) -> Result<Vec<(WebViewHandle, Result<Vec<HostEvent>, HostError>)>, HostError> {
        #[cfg(not(feature = "macos-webkit"))]
        {
            self.sync_views();
            self.servo.perform_all_updates(webviews).map(|results| {
                results
                    .into_iter()
                    .map(|(view, events)| (view, with_navigation_state(events)))
                    .collect()
            })
        }
        #[cfg(feature = "macos-webkit")]
        {
            self.sync_views();
            #[cfg(feature = "servo")]
            let servo_views: Vec<_> = webviews
                .iter()
                .copied()
                .filter(|webview| matches!(self.views.get(webview), Some(MacOsView::Servo)))
                .collect();
            let system_views: std::collections::HashSet<_> = self
                .views
                .iter()
                .filter_map(|(&handle, view)| {
                    matches!(view, MacOsView::System(_)).then_some(handle)
                })
                .collect();
            #[cfg(feature = "macos-webkit-automation")]
            while self.automation.pump_one() {
                self.sync_views();
            }
            #[cfg(feature = "servo")]
            let servo_result = self.servo.perform_all_updates(&servo_views);
            self.sync_views();
            #[cfg(feature = "servo")]
            let mut servo_results: HashMap<_, _> = self
                .finish_servo_pump(webviews, servo_result)?
                .into_iter()
                .collect();

            Ok(webviews
                .iter()
                .copied()
                .map(|webview| {
                    #[cfg(feature = "servo")]
                    if servo_views.contains(&webview) {
                        return (
                            webview,
                            with_navigation_state(
                                servo_results
                                    .remove(&webview)
                                    .unwrap_or_else(|| Ok(Vec::new())),
                            ),
                        );
                    }
                    let result = {
                        match self.views.get(&webview) {
                            Some(MacOsView::System(state))
                                if !self.system_sink(webview, state.generation).is_live() =>
                            {
                                Ok(Vec::new())
                            }
                            Some(MacOsView::System(_)) => {
                                collect_system_updates(&mut self.views, &self.callbacks, webview)
                            }
                            None if system_views.contains(&webview) => Ok(Vec::new()),
                            _ => Err(unknown_webview(webview)),
                        }
                    };
                    (webview, result)
                })
                .collect())
        }
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
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return with_navigation_state(
                self.servo
                    .attach_surface_with_viewport(webview, surface, viewport),
            );
        }
        #[cfg(feature = "macos-webkit")]
        {
            attach_system_view(self, webview, surface, viewport)
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn resize_surface(
        &mut self,
        webview: WebViewHandle,
        size: SurfaceSize,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return with_navigation_state(self.servo.resize_surface(webview, size));
        }
        #[cfg(feature = "macos-webkit")]
        {
            let viewport = system_viewport(&self.views, webview)?.with_size(size);
            update_system_viewport(&mut self.views, webview, viewport)
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn set_surface_scale_factor(
        &mut self,
        webview: WebViewHandle,
        scale_factor: f32,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return with_navigation_state(
                self.servo.set_surface_scale_factor(webview, scale_factor),
            );
        }
        #[cfg(feature = "macos-webkit")]
        {
            let viewport = system_viewport(&self.views, webview)?.with_scale_factor(scale_factor);
            update_system_viewport(&mut self.views, webview, viewport)
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn update_surface_viewport(
        &mut self,
        webview: WebViewHandle,
        viewport: SurfaceViewport,
        previous_viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return with_navigation_state(self.servo.update_surface_viewport(
                webview,
                viewport,
                previous_viewport,
            ));
        }
        #[cfg(feature = "macos-webkit")]
        {
            update_system_viewport(&mut self.views, webview, viewport)
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn detach_surface(&mut self, webview: WebViewHandle) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return with_navigation_state(self.servo.detach_surface(webview));
        }
        #[cfg(feature = "macos-webkit")]
        {
            let state = system_state_mut(&mut self.views, webview)?;
            let native = state.webview.as_ref().ok_or_else(|| {
                HostError::new(format!(
                    "system webview {} has no attached WKWebView",
                    webview.raw()
                ))
            })?;
            native.set_visible(false).map_err(wry_error)?;
            state
                .container
                .as_ref()
                .expect("attached system webview has a container")
                .detach();
            state.viewport = None;
            #[cfg(feature = "macos-webkit-automation")]
            {
                self.callbacks
                    .borrow_mut()
                    .views
                    .get_mut(&(webview.raw(), state.generation))
                    .unwrap()
                    .automation
                    .attached = false;
            }
            // The SDK container and native document survive independently of the
            // initial app slot. Release its lease only after leaving the hierarchy.
            let parent = state.destination_parent.take();
            drop(parent);
            Ok(Vec::new())
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn set_popup_policy(
        &mut self,
        webview: WebViewHandle,
        popup_policy: PopupRequestPolicy,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return with_navigation_state(self.servo.set_popup_policy(webview, popup_policy));
        }
        #[cfg(feature = "macos-webkit")]
        {
            if popup_policy == PopupRequestPolicy::DefaultDeny {
                self.set_new_webview_handler(webview, None)?;
                Ok(Vec::new())
            } else {
                Err(system_unsupported(webview, "managed child popup policy"))
            }
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn attach_managed_child_surface(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
        surface: &HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return {
                with_navigation_state(self.servo.attach_managed_child_surface(
                    webview,
                    child_webview_id,
                    surface,
                    viewport,
                ))
            };
        }
        #[cfg(feature = "macos-webkit")]
        {
            Err(system_unsupported(webview, "managed child surfaces"))
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn resize_managed_child_surface(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
        size: SurfaceSize,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return {
                with_navigation_state(self.servo.resize_managed_child_surface(
                    webview,
                    child_webview_id,
                    size,
                ))
            };
        }
        #[cfg(feature = "macos-webkit")]
        {
            Err(system_unsupported(webview, "managed child surfaces"))
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn update_managed_child_surface_viewport(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
        viewport: SurfaceViewport,
        previous_viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return {
                with_navigation_state(self.servo.update_managed_child_surface_viewport(
                    webview,
                    child_webview_id,
                    viewport,
                    previous_viewport,
                ))
            };
        }
        #[cfg(feature = "macos-webkit")]
        {
            Err(system_unsupported(webview, "managed child surfaces"))
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn detach_managed_child_surface(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return {
                with_navigation_state(
                    self.servo
                        .detach_managed_child_surface(webview, child_webview_id),
                )
            };
        }
        #[cfg(feature = "macos-webkit")]
        {
            Err(system_unsupported(webview, "managed child surfaces"))
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn perform_managed_child_updates(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return {
                with_navigation_state(
                    self.servo
                        .perform_managed_child_updates(webview, child_webview_id),
                )
            };
        }
        #[cfg(feature = "macos-webkit")]
        {
            Err(system_unsupported(webview, "managed child surfaces"))
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn dispatch_webview_command(
        &mut self,
        webview: WebViewHandle,
        command: &WebViewCommand,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return with_navigation_state(self.servo.dispatch_webview_command(webview, command));
        }
        #[cfg(feature = "macos-webkit")]
        {
            dispatch_system_command(&mut self.views, &self.callbacks, webview, command)
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn system_webview_is_loading(&mut self, webview: WebViewHandle) -> Result<bool, HostError> {
        #[cfg(not(feature = "macos-webkit"))]
        {
            self.view_is_servo(webview)?;
            Err(HostError::new(
                "system-WebView loading state is unsupported by Servo",
            ))
        }
        #[cfg(feature = "macos-webkit")]
        {
            // Reconcile only this target; full update/shutdown paths collect unrelated
            // owners. A newly registered native child is queryable before any pump.
            let child = self.pending_children.borrow_mut().remove(&webview);
            if let Some(state) = child {
                self.views.insert(webview, MacOsView::System(state));
            }
            if let Some(MacOsView::System(state)) = self.views.get(&webview) {
                if !self.system_sink(webview, state.generation).is_live()
                    && !state.ui_callback_is_active()
                {
                    self.views.remove(&webview);
                    return Err(unknown_webview(webview));
                }
            } else if self.view_is_servo(webview)? {
                return Err(HostError::new(
                    "system-WebView loading state is unsupported by Servo",
                ));
            }
            let native = system_state_mut(&mut self.views, webview)?
                .webview
                .as_ref()
                .ok_or_else(|| HostError::new("system WebView has no native view yet"))?;
            MainThreadMarker::new().ok_or_else(|| {
                HostError::new("system WebView must be queried on the AppKit main thread")
            })?;
            // SAFETY: the retained WRY view is accessed on its AppKit owning thread.
            Ok(unsafe { native.webview().isLoading() })
        }
    }

    fn resolve_navigation_request(
        &mut self,
        webview: WebViewHandle,
        navigation_id: &str,
        allow: bool,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return {
                with_navigation_state(self.servo.resolve_navigation_request(
                    webview,
                    navigation_id,
                    allow,
                ))
            };
        }
        #[cfg(feature = "macos-webkit")]
        {
            Err(system_unsupported(
                webview,
                "asynchronous navigation policy resolution",
            ))
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn resolve_simple_dialog(
        &mut self,
        webview: WebViewHandle,
        dialog_id: &str,
        confirmed: bool,
        prompt_value: Option<&str>,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return {
                with_navigation_state(self.servo.resolve_simple_dialog(
                    webview,
                    dialog_id,
                    confirmed,
                    prompt_value,
                ))
            };
        }
        #[cfg(feature = "macos-webkit")]
        {
            #[cfg(feature = "macos-webkit-automation")]
            {
                let generation = system_state_mut(&mut self.views, webview)?.generation;
                let sink = self.system_sink(webview, generation);
                automation::dialogs::resolve(&sink, dialog_id, confirmed, prompt_value)?;
                Ok(sink.drain())
            }
            #[cfg(not(feature = "macos-webkit-automation"))]
            Err(system_unsupported(webview, "Servo dialog resolution"))
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn resolve_context_menu(
        &mut self,
        webview: WebViewHandle,
        context_menu_id: &str,
        action: ContextMenuAction,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return {
                with_navigation_state(self.servo.resolve_context_menu(
                    webview,
                    context_menu_id,
                    action,
                ))
            };
        }
        #[cfg(feature = "macos-webkit")]
        {
            Err(system_unsupported(webview, "Servo context-menu resolution"))
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn dismiss_context_menu(
        &mut self,
        webview: WebViewHandle,
        context_menu_id: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return {
                with_navigation_state(self.servo.dismiss_context_menu(webview, context_menu_id))
            };
        }
        #[cfg(feature = "macos-webkit")]
        {
            Err(system_unsupported(webview, "Servo context-menu dismissal"))
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn evaluate_javascript(
        &mut self,
        webview: WebViewHandle,
        evaluation_id: &str,
        script: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return {
                with_navigation_state(self.servo.evaluate_javascript(
                    webview,
                    evaluation_id,
                    script,
                ))
            };
        }
        #[cfg(feature = "macos-webkit")]
        {
            let state = system_state_mut(&mut self.views, webview)?;
            let sink = SystemCallbackSink {
                target: (webview.raw(), state.generation),
                callbacks: self.callbacks.clone(),
            };
            if state.webview.is_some() && MainThreadMarker::new().is_none() {
                return Err(HostError::new(
                    "system webview must evaluate on the AppKit main thread",
                ));
            }
            let Some(token) = sink.begin_evaluation(evaluation_id)? else {
                // Transfer the whole ordered queue, including this readiness failure,
                // before a later construction attempt can reset the native generation.
                return Ok(sink.drain());
            };
            let native = state
                .webview
                .as_ref()
                .expect("an eligible document has a native webview");
            javascript::evaluate(native, sink, evaluation_id, token, script);
            Ok(Vec::new())
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn dispatch_input_event(
        &mut self,
        webview: WebViewHandle,
        event: &HostInputEvent,
    ) -> Result<Vec<HostEvent>, HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return with_navigation_state(self.servo.dispatch_input_event(webview, event));
        }
        #[cfg(feature = "macos-webkit")]
        {
            Err(HostError::new(format!(
            "system webview {} receives native AppKit input; forwarded HostInputEvent is unsupported",
            webview.raw()
        )))
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn synthesize_webview_command_events(&self) -> bool {
        false
    }

    fn perform_updates(&mut self, webview: WebViewHandle) -> Result<Vec<HostEvent>, HostError> {
        #[cfg(feature = "macos-webkit-automation")]
        {
            self.sync_views();
            while self.automation.pump_one() {
                self.sync_views();
            }
        }
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return with_navigation_state(self.servo.perform_updates(webview));
        }
        #[cfg(feature = "macos-webkit")]
        {
            collect_system_updates(&mut self.views, &self.callbacks, webview)
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn observe_webview_events(
        &mut self,
        webview: WebViewHandle,
        events: &[HostEvent],
    ) -> Result<(), HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return self.servo.observe_webview_events(webview, events);
        }
        #[cfg(feature = "macos-webkit")]
        {
            Ok(())
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }

    fn observe_managed_child_webview_events(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
        events: &[HostEvent],
    ) -> Result<(), HostError> {
        let _is_servo = self.view_is_servo(webview)?;
        #[cfg(feature = "servo")]
        if _is_servo {
            return {
                self.servo
                    .observe_managed_child_webview_events(webview, child_webview_id, events)
            };
        }
        #[cfg(feature = "macos-webkit")]
        {
            Err(system_unsupported(webview, "managed child events"))
        }
        #[cfg(not(feature = "macos-webkit"))]
        {
            Err(system_not_compiled())
        }
    }
}

#[cfg(feature = "servo")]
fn with_navigation_state(
    events: Result<Vec<HostEvent>, HostError>,
) -> Result<Vec<HostEvent>, HostError> {
    let events = events?;
    let mut result = Vec::with_capacity(events.len());
    for event in events {
        let navigation_state = match &event {
            HostEvent::HistoryChanged {
                can_go_back,
                can_go_forward,
                ..
            } => Some((*can_go_back, *can_go_forward)),
            _ => None,
        };
        result.push(event);
        if let Some((can_go_back, can_go_forward)) = navigation_state {
            result.push(HostEvent::NavigationStateChanged {
                can_go_back,
                can_go_forward,
            });
        }
    }
    Ok(result)
}

#[cfg(feature = "macos-webkit")]
fn attach_system_view<S>(
    host: &mut MacOsViewHost<S>,
    webview: WebViewHandle,
    surface: &HostSurface,
    viewport: SurfaceViewport,
) -> Result<Vec<HostEvent>, HostError>
where
    S: AppKitViewDelegate + for<'a> SurfaceDelegate<Frame<'a> = SurfaceFrame<'a>>,
{
    let (generation, pending_commands, has_native_view) = {
        let state = system_state_mut(&mut host.views, webview)?;
        if state.viewport.is_some() {
            return Err(HostError::new(format!(
                "system webview {} already has an attached surface",
                webview.raw()
            )));
        }
        (
            state.generation,
            state.pending_commands.iter().cloned().collect::<Vec<_>>(),
            state.webview.is_some(),
        )
    };
    let sink = host.system_sink(webview, generation);
    let data_store_identifier = host.webkit_data_store_identifier;
    let new_window = SystemNewWebView {
        source: webview,
        sink: sink.clone(),
        registrar: host.registrar.clone(),
        pending: Rc::downgrade(&host.pending_children),
        destination: host.new_webview_destination_handler.clone(),
        handler: system_state_mut(&mut host.views, webview)?.handler.clone(),
        next_request: Rc::default(),
    };

    #[cfg(feature = "servo")]
    let services = host.servo.services_mut();
    #[cfg(not(feature = "servo"))]
    let services = &mut host.services;
    let parent_surface = services
        .appkit_parent(surface, viewport)
        .map_err(|error| HostError::new(error.to_string()))?;
    let parent = BorrowedAppKitParent(parent_surface.window_handle());

    if has_native_view {
        let mut events = Vec::new();
        let state = system_state_mut(&mut host.views, webview)?;
        let native = state
            .webview
            .as_ref()
            .expect("system webview presence was checked");
        let container = state
            .container
            .as_ref()
            .expect("system webview presence was checked");
        container.attach(&parent)?;
        if let Err(error) = native
            .set_bounds(wry_rect(viewport))
            .and_then(|()| native.set_visible(true))
        {
            container.detach();
            return Err(wry_error(error));
        }
        for command in &pending_commands {
            match apply_system_command(native, command) {
                Ok(command_events) => events.extend(command_events),
                Err(error) => {
                    let _ = native.set_visible(false);
                    container.detach();
                    return Err(error);
                }
            }
        }
        state.pending_commands.clear();
        state.viewport = Some(viewport);
        #[cfg(feature = "macos-webkit-automation")]
        {
            host.callbacks
                .borrow_mut()
                .views
                .get_mut(&sink.target)
                .unwrap()
                .automation
                .attached = true;
        }
        events.extend(collect_system_updates(
            &mut host.views,
            &host.callbacks,
            webview,
        )?);
        return Ok(events);
    }

    let initial_url = pending_commands
        .first()
        .and_then(|command| match command {
            WebViewCommand::LoadUrl(request) => Some(request.url.as_str()),
            _ => None,
        })
        .unwrap_or("about:blank");
    let promoted_initial_load =
        matches!(pending_commands.first(), Some(WebViewCommand::LoadUrl(_)));

    let container = AppKitContainer::new(&parent)?;
    let built = build_system_webview(
        &container,
        viewport,
        data_store_identifier,
        sink.clone(),
        &mut host.system_context,
    );

    let native = match built {
        Ok(native) => native,
        Err(error) => {
            sink.clear();
            host.reset_system_generation(webview);
            return Err(error);
        }
    };

    let ui = match SystemUiDelegateRegistration::attach(&native, new_window) {
        Ok(ui) => ui,
        Err(error) => {
            drop(native);
            host.reset_system_generation(webview);
            return Err(error);
        }
    };
    #[cfg(feature = "macos-webkit-automation")]
    host.automation.attached(&sink, webview, &native.webview());
    let state = system_state_mut(&mut host.views, webview)?;
    state.ui_registration = Some(ui);
    state.icon_registration = favicon::IconRegistration::attach(&native, sink.clone());
    state.observation = Some(observation::Registration::attach(&native, sink.clone()));
    state.webview = Some(native);
    state.container = Some(container);
    let mut events = Vec::new();
    let result = (|| {
        let native = state.webview.as_ref().unwrap();
        native.load_url(initial_url).map_err(wry_error)?;
        for command in pending_commands
            .iter()
            .skip(usize::from(promoted_initial_load))
        {
            events.extend(apply_system_command(native, command)?);
        }
        Ok::<_, HostError>(())
    })();
    if let Err(error) = result {
        let ui = state.ui_registration.take();
        let icon = state.icon_registration.take();
        let observation = state.observation.take();
        let native = state.webview.take();
        let container = state.container.take();
        sink.invalidate();
        drop((ui, icon, observation, native, container));
        host.reset_system_generation(webview);
        return Err(error);
    }
    state.pending_commands.clear();
    state.viewport = Some(viewport);
    events.extend(collect_system_updates(
        &mut host.views,
        &host.callbacks,
        webview,
    )?);
    Ok(events)
}

#[cfg(feature = "macos-webkit")]
fn build_system_webview(
    parent: &impl HasWindowHandle,
    viewport: SurfaceViewport,
    data_store_identifier: WebKitDataStoreIdentifier,
    sink: SystemCallbackSink,
    context: &mut Option<SystemWebViewContext>,
) -> Result<WebView, HostError> {
    #[cfg(feature = "macos-webkit-automation")]
    let eligible = automation::dialogs::enabled(&sink);
    let mut builder = system_webview_builder(sink)
        .with_bounds(wry_rect(viewport))
        .with_data_store_identifier(data_store_identifier.0);
    if let Some(context) = context {
        let configuration = context.configuration();
        #[cfg(feature = "macos-webkit-automation")]
        if eligible {
            automation::native::configure(&configuration, true);
        }
        builder = builder.with_webview_configuration(configuration);
    }
    #[cfg(feature = "macos-webkit-automation")]
    if context.is_none() && eligible {
        let mtm =
            MainThreadMarker::new().ok_or_else(|| HostError::new("AppKit main thread required"))?;
        // Preserve Wry's selected named data-store behavior when supplying the
        // first configuration ourselves to set eligibility before allocation.
        let configuration = unsafe { WKWebViewConfiguration::new(mtm) };
        if configuration::named_stores_available() {
            let identifier = objc2_foundation::NSUUID::from_bytes(data_store_identifier.0);
            unsafe {
                configuration.setWebsiteDataStore(
                    &objc2_web_kit::WKWebsiteDataStore::dataStoreForIdentifier(&identifier, mtm),
                );
            }
        }
        automation::native::configure(&configuration, true);
        builder = builder.with_webview_configuration(configuration);
    }
    let native = builder.build_as_child(parent).map_err(wry_error)?;
    if context.is_none() {
        // SAFETY: WRY constructed this native view on AppKit's thread.
        let configuration = unsafe { native.webview().configuration() };
        *context = Some(SystemWebViewContext::from_configuration(&configuration));
    }
    Ok(native)
}

#[cfg(feature = "macos-webkit")]
fn system_webview_builder(sink: SystemCallbackSink) -> WebViewBuilder<'static> {
    let title_sink = sink.clone();
    let load_sink = sink.clone();
    let navigation_sink = sink.clone();
    let crash_sink = sink;
    WebViewBuilder::new()
        .with_accept_first_mouse(true)
        .with_navigation_handler(move |_| {
            navigation_sink.navigation_requested();
            true
        })
        .with_download_started_handler(|_, _| false)
        .with_permission_handler(|_| PermissionResponse::Default)
        .with_document_title_changed_handler(move |title| {
            title_sink.push(HostEvent::PageTitleChanged {
                title: (!title.is_empty()).then_some(title),
            });
        })
        .with_on_page_load_handler(move |event, _| match event {
            PageLoadEvent::Started => load_sink.document_committed(),
            PageLoadEvent::Finished => load_sink.push(HostEvent::LoadStatusChanged {
                status: LoadStatusKind::Complete,
            }),
        })
        .with_on_web_content_process_terminate_handler(move || {
            crash_sink.process_terminated();
        })
}

#[cfg(feature = "macos-webkit")]
fn dispatch_system_command(
    views: &mut HashMap<WebViewHandle, MacOsView>,
    callbacks: &Rc<RefCell<SystemCallbacks>>,
    webview: WebViewHandle,
    command: &WebViewCommand,
) -> Result<Vec<HostEvent>, HostError> {
    let state = system_state_mut(views, webview)?;
    if matches!(command, WebViewCommand::StopLoading) && state.webview.is_none() {
        return Err(HostError::new("system WebView has no native view yet"));
    }
    if should_queue_system_command(state.webview.is_some(), state.viewport.is_some(), command) {
        state.pending_commands.push_back(command.clone());
        return Ok(Vec::new());
    }
    let native = state
        .webview
        .as_ref()
        .expect("an attached system view has a native webview");

    if matches!(
        command,
        WebViewCommand::LoadUrl(_)
            | WebViewCommand::Reload
            | WebViewCommand::GoBack
            | WebViewCommand::GoForward
    ) {
        // goBack/goForward may update currentItem synchronously, before WebKit
        // calls the navigation policy delegate. Preserve the departing identity.
        SystemCallbackSink {
            target: (webview.raw(), state.generation),
            callbacks: callbacks.clone(),
        }
        .navigation_requested();
    }
    let mut events = apply_system_command(native, command)?;
    events.extend(collect_system_updates(views, callbacks, webview)?);
    Ok(events)
}

#[cfg(feature = "macos-webkit")]
fn should_queue_system_command(
    has_native_view: bool,
    has_surface: bool,
    command: &WebViewCommand,
) -> bool {
    !has_native_view
        || !has_surface && matches!(command, WebViewCommand::Focus | WebViewCommand::Blur)
}

#[cfg(feature = "macos-webkit")]
fn apply_system_command(
    native: &WebView,
    command: &WebViewCommand,
) -> Result<Vec<HostEvent>, HostError> {
    let event = match command {
        WebViewCommand::LoadUrl(request) => {
            native.load_url(&request.url).map_err(wry_error)?;
            None
        }
        WebViewCommand::Reload => {
            native.reload().map_err(wry_error)?;
            None
        }
        WebViewCommand::StopLoading => {
            MainThreadMarker::new().ok_or_else(|| {
                HostError::new("system WebView must stop on the AppKit main thread")
            })?;
            // SAFETY: WRY retains this WKWebView and its delegates on the AppKit thread.
            unsafe { native.webview().stopLoading() };
            None
        }
        WebViewCommand::GoBack => {
            native.go_back().map_err(wry_error)?;
            None
        }
        WebViewCommand::GoForward => {
            native.go_forward().map_err(wry_error)?;
            None
        }
        WebViewCommand::Focus => {
            native.focus().map_err(wry_error)?;
            Some(HostEvent::FocusChanged { is_focused: true })
        }
        WebViewCommand::Blur => {
            blur_system_webview(native)?;
            Some(HostEvent::FocusChanged { is_focused: false })
        }
    };
    Ok(event.into_iter().collect())
}

#[cfg(feature = "macos-webkit")]
fn blur_system_webview(native: &WebView) -> Result<(), HostError> {
    let webview = native.webview();
    // SAFETY: WRY retains the WKWebView's ExplorerKit-owned container, and an
    // attached container's superview is the current app-owned parent.
    let container = unsafe { webview.superview() };
    let parent = container
        .as_ref()
        .and_then(|container| unsafe { container.superview() });
    let Some(window) = parent.as_ref().and_then(|parent| parent.window()) else {
        return native.focus_parent().map_err(wry_error);
    };
    if !window.makeFirstResponder(parent.as_deref().map(|view| &**view)) {
        window.makeFirstResponder(None);
    }
    Ok(())
}

#[cfg(feature = "macos-webkit")]
fn collect_system_updates(
    views: &mut HashMap<WebViewHandle, MacOsView>,
    callbacks: &Rc<RefCell<SystemCallbacks>>,
    webview: WebViewHandle,
) -> Result<Vec<HostEvent>, HostError> {
    let generation = match views.get(&webview) {
        Some(MacOsView::System(state)) => state.generation,
        #[cfg(feature = "servo")]
        Some(MacOsView::Servo) => {
            return Err(HostError::new(format!(
                "webview {} is Servo-backed",
                webview.raw()
            )))
        }
        None => return Err(unknown_webview(webview)),
    };
    let sink = SystemCallbackSink {
        target: (webview.raw(), generation),
        callbacks: callbacks.clone(),
    };
    let mut events = sink.drain();
    let (url, navigation_state) = callbacks
        .borrow()
        .views
        .get(&sink.target)
        .map(|state| (state.observed_url.clone(), state.observed_navigation_state))
        .unwrap_or_default();
    let state = system_state_mut(views, webview)?;
    if state.capture.as_mut().is_some_and(site_data::Capture::pump) {
        state.capture.take();
    }
    // Diff the latest native observation at drain time, preserving burst coalescing
    // and queued-result order without state or history reads from WebKit.
    if let Some(url) = url {
        if state.last_url.as_deref() != Some(url.as_str()) {
            state.last_url = Some(url.clone());
            events.push(HostEvent::UrlChanged { url });
        }
    }

    if let Some(navigation_state) = navigation_state {
        if state.last_navigation_state != Some(navigation_state) {
            state.last_navigation_state = Some(navigation_state);
            events.push(HostEvent::NavigationStateChanged {
                can_go_back: navigation_state.0,
                can_go_forward: navigation_state.1,
            });
        }
    }
    Ok(events)
}

#[cfg(feature = "macos-webkit")]
fn update_system_viewport(
    views: &mut HashMap<WebViewHandle, MacOsView>,
    webview: WebViewHandle,
    viewport: SurfaceViewport,
) -> Result<Vec<HostEvent>, HostError> {
    let state = system_state_mut(views, webview)?;
    let native = state.webview.as_ref().ok_or_else(|| {
        HostError::new(format!(
            "system webview {} has no attached WKWebView",
            webview.raw()
        ))
    })?;
    native.set_bounds(wry_rect(viewport)).map_err(wry_error)?;
    state.viewport = Some(viewport);
    Ok(Vec::new())
}

#[cfg(feature = "macos-webkit")]
fn system_viewport(
    views: &HashMap<WebViewHandle, MacOsView>,
    webview: WebViewHandle,
) -> Result<SurfaceViewport, HostError> {
    match views.get(&webview) {
        Some(MacOsView::System(state)) => state.viewport.ok_or_else(|| {
            HostError::new(format!(
                "system webview {} does not have an attached surface",
                webview.raw()
            ))
        }),
        #[cfg(feature = "servo")]
        Some(MacOsView::Servo) => Err(HostError::new(format!(
            "webview {} is Servo-backed",
            webview.raw()
        ))),
        None => Err(unknown_webview(webview)),
    }
}

#[cfg(feature = "macos-webkit")]
fn system_state_mut(
    views: &mut HashMap<WebViewHandle, MacOsView>,
    webview: WebViewHandle,
) -> Result<&mut SystemWebViewState, HostError> {
    match views.get_mut(&webview) {
        Some(MacOsView::System(state)) => Ok(state),
        #[cfg(feature = "servo")]
        Some(MacOsView::Servo) => Err(HostError::new(format!(
            "webview {} is Servo-backed",
            webview.raw()
        ))),
        None => Err(unknown_webview(webview)),
    }
}

#[cfg(feature = "macos-webkit")]
fn wry_rect(viewport: SurfaceViewport) -> Rect {
    Rect {
        position: PhysicalPosition::new(viewport.origin.x, viewport.origin.y).into(),
        size: PhysicalSize::new(viewport.size.width, viewport.size.height).into(),
    }
}

fn unknown_webview(webview: WebViewHandle) -> HostError {
    HostError::new(format!("webview {} is not registered", webview.raw()))
}

fn duplicate_webview(webview: WebViewHandle) -> HostError {
    HostError::new(format!("webview {} is already registered", webview.raw()))
}

#[cfg(feature = "macos-webkit")]
fn system_unsupported(webview: WebViewHandle, capability: &str) -> HostError {
    HostError::new(format!(
        "system webview {} does not support {capability}",
        webview.raw()
    ))
}

#[cfg(feature = "macos-webkit")]
fn wry_error(error: wry::Error) -> HostError {
    HostError::new(error.to_string())
}

#[cfg(feature = "macos-webkit")]
struct BorrowedAppKitParent<'a>(WindowHandle<'a>);

#[cfg(feature = "macos-webkit")]
impl HasWindowHandle for BorrowedAppKitParent<'_> {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Ok(self.0)
    }
}

#[cfg(feature = "macos-webkit")]
struct AppKitContainer(Retained<NSView>);

#[cfg(feature = "macos-webkit")]
impl AppKitContainer {
    fn new(parent: &impl HasWindowHandle) -> Result<Self, HostError> {
        let parent = appkit_parent_view(parent)?;
        let main_thread = MainThreadMarker::new().ok_or_else(|| {
            HostError::new("system webview must attach on the AppKit main thread")
        })?;
        let view = NSView::initWithFrame(NSView::alloc(main_thread), parent.bounds());
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        parent.addSubview(&view);
        Ok(Self(view))
    }

    fn attach(&self, parent: &impl HasWindowHandle) -> Result<(), HostError> {
        let parent = appkit_parent_view(parent)?;
        self.0.setFrame(parent.bounds());
        parent.addSubview(&self.0);
        Ok(())
    }

    fn detach(&self) {
        self.0.removeFromSuperview();
    }
}

#[cfg(feature = "macos-webkit")]
impl Drop for AppKitContainer {
    fn drop(&mut self) {
        self.detach();
    }
}

#[cfg(feature = "macos-webkit")]
impl HasWindowHandle for AppKitContainer {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let handle = AppKitWindowHandle::new(NonNull::from(&*self.0).cast::<c_void>());
        // SAFETY: the returned handle borrows the retained NSView owned by self.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AppKit(handle)) })
    }
}

#[cfg(feature = "macos-webkit")]
fn appkit_parent_view(parent: &impl HasWindowHandle) -> Result<&NSView, HostError> {
    let handle = parent
        .window_handle()
        .map_err(|error| HostError::new(error.to_string()))?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err(HostError::new(
            "system webview requires an AppKit parent view",
        ));
    };
    // SAFETY: the app guarantees that the borrowed raw handle identifies a
    // live NSView for the duration of this call on AppKit's owning thread.
    Ok(unsafe { handle.ns_view.cast::<NSView>().as_ref() })
}

#[cfg(all(test, feature = "macos-webkit"))]
mod tests {
    use super::*;

    objc2::define_class!(
        #[unsafe(super(NSObject))]
        #[ivars = bool]
        struct PopupPreferenceFixture;

        unsafe impl NSObjectProtocol for PopupPreferenceFixture {}

        impl PopupPreferenceFixture {
            #[unsafe(method(_wantsPopup))]
            fn native_preference(&self) -> bool {
                *objc2::DefinedClass::ivars(self)
            }
        }
    );

    #[test]
    fn native_popup_preference_preserves_bool_and_missing_selector() {
        use objc2::AnyThread;
        assert_eq!(wants_popup(&NSObject::new()), None);
        for value in [false, true] {
            let allocated = PopupPreferenceFixture::alloc().set_ivars(value);
            // SAFETY: Initialize the fixture's NSObject superclass normally.
            let receiver: Retained<PopupPreferenceFixture> =
                unsafe { msg_send![super(allocated), init] };
            assert_eq!(wants_popup(&receiver), Some(value));
        }
    }

    use crate::input::HostInputEvent;
    use crate::runtime::{ExplorerKitError, Runtime};
    use crate::surface::SurfaceTarget;

    struct NoopDelegate;

    impl SurfaceDelegate for NoopDelegate {
        type Frame<'a> = SurfaceFrame<'a>;

        fn render_target(
            &mut self,
            _surface: &HostSurface,
            _viewport: SurfaceViewport,
        ) -> Result<SurfaceTarget<'_>, SurfaceError> {
            Err(SurfaceError::new("not used by this test"))
        }

        fn present_frame(
            &mut self,
            _surface: &HostSurface,
            frame: &mut Self::Frame<'_>,
        ) -> Result<(), SurfaceError> {
            frame.present();
            Ok(())
        }
    }

    impl AppKitViewDelegate for NoopDelegate {
        fn appkit_parent(
            &mut self,
            _surface: &HostSurface,
            _viewport: SurfaceViewport,
        ) -> Result<NativeSurface<'_>, SurfaceError> {
            Err(SurfaceError::new("not used by this test"))
        }
    }

    fn runtime() -> Runtime<MacOsViewHost<NoopDelegate>> {
        Runtime::new(MacOsViewHost::new(
            NoopDelegate,
            MacOsViewHostOptions::new(
                SurfaceHostOptions::default(),
                WebKitDataStoreIdentifier([0x34; 16]),
            ),
        ))
    }

    #[test]
    fn native_state_wakes_after_publication_even_without_a_queued_event() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        thread_local! {
            static CALLBACKS: RefCell<Weak<RefCell<SystemCallbacks>>> = const { RefCell::new(Weak::new()) };
        }
        let wakes = Arc::new(AtomicUsize::new(0));
        let observed = wakes.clone();
        let callbacks = Rc::new(RefCell::new(SystemCallbacks::new(Arc::new(move || {
            CALLBACKS.with(|slot| {
                let callbacks = slot.borrow().upgrade().unwrap();
                let callbacks = callbacks
                    .try_borrow_mut()
                    .expect("wake outside state borrow");
                let state = callbacks.views.get(&(1, 1)).unwrap();
                assert_eq!(state.observed_navigation_state, Some((false, false)));
                assert!(state.events.is_empty());
            });
            observed.fetch_add(1, Ordering::Relaxed);
        }))));
        CALLBACKS.with(|slot| *slot.borrow_mut() = Rc::downgrade(&callbacks));
        let sink = SystemCallbackSink::register_raw(callbacks, 1, 1);
        sink.native_state_observed(None, (false, false));
        sink.native_state_observed(None, (false, false)); // e.g. loading changed twice
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
        sink.invalidate();
        sink.native_state_observed(Some("stale".into()), (true, true));
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn native_state_drains_coalesce_after_results_without_a_native_view() {
        let (mut runtime, host) = staging_runtime();
        let session = runtime.create_session();
        let view = runtime.create_webview(session).unwrap();
        let sink = host.borrow().system_sink(view, 1);
        sink.push(HostEvent::PageTitleChanged {
            title: Some("queued".into()),
        });
        sink.native_state_observed(Some("https://example.test/a".into()), (false, false));
        sink.native_state_observed(Some("https://example.test/b".into()), (true, false));
        let mut host = host.borrow_mut();
        let callbacks = host.callbacks.clone();
        assert_eq!(
            collect_system_updates(&mut host.views, &callbacks, view).unwrap(),
            vec![
                HostEvent::PageTitleChanged {
                    title: Some("queued".into())
                },
                HostEvent::UrlChanged {
                    url: "https://example.test/b".into()
                },
                HostEvent::NavigationStateChanged {
                    can_go_back: true,
                    can_go_forward: false
                },
            ]
        );
        sink.native_state_observed(None, (true, false));
        assert!(collect_system_updates(&mut host.views, &callbacks, view)
            .unwrap()
            .is_empty());
        sink.invalidate();
        let replacement = SystemCallbackSink::register(callbacks.clone(), view, 2);
        system_state_mut(&mut host.views, view).unwrap().generation = 2;
        sink.native_state_observed(Some("stale".into()), (false, true));
        assert!(replacement.drain().is_empty());
        assert!(collect_system_updates(&mut host.views, &callbacks, view)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn native_result_writers_publish_before_waking_and_reject_stale_callbacks() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        thread_local! {
            static CALLBACKS: RefCell<Weak<RefCell<SystemCallbacks>>> = const { RefCell::new(Weak::new()) };
        }
        let wakes = Arc::new(AtomicUsize::new(0));
        let observed = wakes.clone();
        let callbacks = Rc::new(RefCell::new(SystemCallbacks::new(Arc::new(move || {
            CALLBACKS.with(|slot| {
                let callbacks = slot.borrow().upgrade().unwrap();
                let state = callbacks
                    .try_borrow_mut()
                    .expect("wake runs outside callback borrows");
                assert!(
                    state.views.values().any(|view| !view.events.is_empty()),
                    "publish before wake"
                );
            });
            observed.fetch_add(1, Ordering::Relaxed);
        }))));
        CALLBACKS.with(|slot| *slot.borrow_mut() = Rc::downgrade(&callbacks));
        let sink = SystemCallbackSink::register_raw(callbacks.clone(), 1, 1);
        let wakes = &*wakes;
        let published = || {
            let before = wakes.load(Ordering::Relaxed);
            move || assert!(wakes.load(Ordering::Relaxed) > before)
        };

        let check = published();
        assert_eq!(sink.begin_evaluation("unready").unwrap(), None);
        check();
        sink.drain();
        let check = published();
        sink.push(HostEvent::PageTitleChanged {
            title: Some("title".into()),
        });
        check();
        sink.drain();
        let check = published();
        sink.document_committed();
        check();
        sink.drain();
        let token = sink.begin_evaluation("ok").unwrap().unwrap();
        let check = published();
        sink.complete_evaluation("ok", token, Ok("null".into()));
        check();
        sink.drain();
        for error in [
            NativeEvaluationError::ProcessTerminated,
            NativeEvaluationError::ViewInvalidated,
            NativeEvaluationError::Result(JavaScriptEvaluationErrorKind::EvaluationFailure),
        ] {
            sink.document_committed();
            sink.drain();
            let token = sink.begin_evaluation("error").unwrap().unwrap();
            let check = published();
            sink.complete_evaluation("error", token, Err(error));
            check();
            assert!(!sink.drain().is_empty());
        }
        sink.document_committed();
        sink.drain();
        sink.begin_evaluation("navigation").unwrap();
        let check = published();
        sink.document_committed();
        check();
        assert_eq!(
            sink.drain().len(),
            2,
            "commit and retired read share a notification"
        );
        sink.begin_evaluation("crash").unwrap();
        let check = published();
        sink.process_terminated();
        check();
        assert_eq!(
            sink.drain().len(),
            2,
            "crash and retired read share a notification"
        );
        sink.document_committed();
        sink.drain();
        let icon = sink.begin_favicon().unwrap();
        let check = published();
        sink.complete_favicon(icon, favicon(1));
        check();
        sink.drain();

        let old_read = sink.begin_evaluation("stale").unwrap().unwrap();
        sink.document_committed();
        sink.drain();
        let before = wakes.load(Ordering::Relaxed);
        sink.complete_evaluation("stale", old_read, Ok("null".into()));
        sink.complete_favicon(icon, favicon(2));
        assert_eq!(wakes.load(Ordering::Relaxed), before);
        sink.invalidate();
        let replacement = SystemCallbackSink::register_raw(callbacks, 1, 2);
        sink.push(HostEvent::Closed);
        sink.document_committed();
        sink.process_terminated();
        sink.complete_evaluation(
            "stale",
            old_read,
            Err(NativeEvaluationError::ProcessTerminated),
        );
        sink.complete_favicon(icon, favicon(3));
        assert_eq!(wakes.load(Ordering::Relaxed), before);
        assert!(replacement.drain().is_empty());
        let check = published();
        replacement.push(HostEvent::PageTitleChanged {
            title: Some("replacement".into()),
        });
        check();
        CALLBACKS.with(|slot| *slot.borrow_mut() = Weak::new());
    }

    #[test]
    fn native_wake_hints_coalesce_and_keep_independent_hosts_separate() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };

        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        let make_host = |pending: Arc<AtomicBool>, store| {
            MacOsViewHost::new(
                NoopDelegate,
                MacOsViewHostOptions::new(
                    SurfaceHostOptions::new(
                        Arc::new(move || {
                            pending.store(true, Ordering::Release);
                        }),
                        Rc::new(crate::surface::MemoryClipboard::default()),
                    ),
                    WebKitDataStoreIdentifier([store; 16]),
                ),
            )
        };
        let first_host = Rc::new(RefCell::new(make_host(first.clone(), 1)));
        let mut first_runtime = Runtime::new(StagedHost(first_host.clone()));
        let first_session = first_runtime.create_session();
        let first_view = first_runtime.create_webview(first_session).unwrap();
        let second_host = Rc::new(RefCell::new(make_host(second.clone(), 2)));
        let mut second_runtime = Runtime::new(StagedHost(second_host.clone()));
        let second_session = second_runtime.create_session();
        let second_view = second_runtime.create_webview(second_session).unwrap();
        let first_sink = first_host.borrow().system_sink(first_view, 1);
        let second_sink = second_host.borrow().system_sink(second_view, 1);
        first_sink.push(HostEvent::PageTitleChanged {
            title: Some("first".into()),
        });
        first_sink.document_committed();
        assert!(first.swap(false, Ordering::AcqRel));
        assert!(!second.load(Ordering::Acquire));
        first_runtime.perform_all_updates().unwrap();
        assert_eq!(first_runtime.drain_events().len(), 2);
        second_sink.push(HostEvent::PageTitleChanged {
            title: Some("second".into()),
        });
        assert!(second.swap(false, Ordering::AcqRel));
        assert!(!first.load(Ordering::Acquire));
        second_runtime.perform_all_updates().unwrap();
        assert_eq!(second_runtime.drain_events().len(), 1);
        first_runtime.destroy_webview(first_view).unwrap();
        first_sink.push(HostEvent::Closed);
        assert!(!first.load(Ordering::Acquire));
        second_sink.push(HostEvent::PageTitleChanged {
            title: Some("survivor".into()),
        });
        assert!(second.swap(false, Ordering::AcqRel));
        second_runtime.perform_all_updates().unwrap();
        assert_eq!(second_runtime.drain_events().len(), 1);
    }

    struct StagedHost(Rc<RefCell<MacOsViewHost<NoopDelegate>>>);

    impl Host for StagedHost {
        fn set_webview_registrar(&mut self, registrar: WebViewRegistrar) {
            self.0.borrow_mut().set_webview_registrar(registrar);
        }
        fn create_webview(
            &mut self,
            session: SessionHandle,
            webview: WebViewHandle,
        ) -> Result<(), HostError> {
            self.0.borrow_mut().create_webview_with_options(
                session,
                webview,
                MacOsWebViewOptions::new(MacOsWebViewKind::SystemWebView),
            )
        }
        fn destroy_webview(&mut self, webview: WebViewHandle) -> Result<(), HostError> {
            self.0.borrow_mut().destroy_webview(webview)
        }
        fn dispatch_webview_command(
            &mut self,
            webview: WebViewHandle,
            command: &WebViewCommand,
        ) -> Result<Vec<HostEvent>, HostError> {
            self.0
                .borrow_mut()
                .dispatch_webview_command(webview, command)
        }
        fn system_webview_is_loading(&mut self, view: WebViewHandle) -> Result<bool, HostError> {
            self.0.borrow_mut().system_webview_is_loading(view)
        }
        fn observe_webview_events(
            &mut self,
            webview: WebViewHandle,
            events: &[HostEvent],
        ) -> Result<(), HostError> {
            self.0.borrow_mut().observe_webview_events(webview, events)
        }
        fn perform_updates(&mut self, webview: WebViewHandle) -> Result<Vec<HostEvent>, HostError> {
            self.0.borrow_mut().perform_updates(webview)
        }
        fn perform_all_updates(
            &mut self,
            views: &[WebViewHandle],
        ) -> Result<Vec<(WebViewHandle, Result<Vec<HostEvent>, HostError>)>, HostError> {
            self.0.borrow_mut().perform_all_updates(views)
        }
        fn set_new_webview_handler(
            &mut self,
            webview: WebViewHandle,
            handler: Option<Rc<NewWebViewHandler>>,
        ) -> Result<(), HostError> {
            self.0
                .borrow_mut()
                .set_new_webview_handler(webview, handler)
        }
        fn shutdown(&mut self) -> Result<(), HostError> {
            self.0.borrow_mut().shutdown()
        }
    }

    fn staging_runtime() -> (
        Runtime<StagedHost>,
        Rc<RefCell<MacOsViewHost<NoopDelegate>>>,
    ) {
        let host = Rc::new(RefCell::new(MacOsViewHost::new(
            NoopDelegate,
            MacOsViewHostOptions::new(
                SurfaceHostOptions::default(),
                WebKitDataStoreIdentifier([0x34; 16]),
            ),
        )));
        (Runtime::new(StagedHost(host.clone())), host)
    }

    fn staged_child(host: &MacOsViewHost<NoopDelegate>, source: WebViewHandle) -> WebViewHandle {
        let registration = host
            .registrar
            .as_ref()
            .unwrap()
            .reserve_child(source, "staged".into())
            .unwrap();
        let child = registration.webview();
        SystemCallbackSink::register(host.callbacks.clone(), child, 1);
        host.pending_children
            .borrow_mut()
            .insert(child, Box::new(SystemWebViewState::new(1)));
        registration.commit_with_surface(
            PopupCreated {
                request_id: "staged".into(),
                parent_webview_id: source.raw().to_string(),
                child_webview_id: child.raw().to_string(),
                parent_url: None,
                target_url: None,
                window_features: None,
                wants_popup: None,
            },
            HostSurface::new(format!("slot-{}", child.raw())),
            SurfaceViewport::new(SurfaceSize::new(120, 90), 1.0),
        );
        child
    }

    #[test]
    fn loading_queries_validate_one_system_lifetime_per_call_without_a_pump() {
        for count in [4, 24, 62] {
            for staged in [false, true] {
                let (mut runtime, host) = staging_runtime();
                let session = runtime.create_session();
                let source = runtime.create_webview(session).unwrap();
                let views: Vec<_> = (0..count)
                    .map(|_| {
                        if staged {
                            staged_child(&host.borrow(), source)
                        } else {
                            runtime.create_webview(session).unwrap()
                        }
                    })
                    .collect();
                host.borrow().callbacks.borrow().liveness_checks.set(0);
                for (index, &view) in views.iter().enumerate() {
                    assert!(runtime
                        .system_webview_is_loading(view)
                        .unwrap_err()
                        .to_string()
                        .contains("no native view"));
                    assert_eq!(
                        host.borrow().pending_children.borrow().len(),
                        if staged { count - index - 1 } else { 0 },
                        "a query adopts only its own original child"
                    );
                }
                assert_eq!(
                    host.borrow().callbacks.borrow().liveness_checks.get(),
                    count,
                    "query bookkeeping must not walk sibling views"
                );
            }
        }
    }

    #[test]
    fn loading_query_adopts_nested_children_and_rejects_retired_targets() {
        let (mut runtime, host) = staging_runtime();
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let child = staged_child(&host.borrow(), source);
        let nested = staged_child(&host.borrow(), child);
        assert!(runtime
            .system_webview_is_loading(nested)
            .unwrap_err()
            .to_string()
            .contains("no native view"));
        assert!(host.borrow().views.contains_key(&nested));
        assert!(host.borrow().pending_children.borrow().contains_key(&child));

        runtime.destroy_webview(source).unwrap();
        assert!(runtime
            .system_webview_is_loading(nested)
            .unwrap_err()
            .to_string()
            .contains("no native view"));
        host.borrow().system_sink(nested, 1).invalidate();
        assert!(host
            .borrow_mut()
            .system_webview_is_loading(nested)
            .unwrap_err()
            .to_string()
            .contains("not registered"));
        assert!(!host.borrow().views.contains_key(&nested));
        assert!(host.borrow().views.contains_key(&child));

        host.borrow()
            .registrar
            .as_ref()
            .unwrap()
            .retire_webview(nested);
        assert_eq!(
            runtime.system_webview_is_loading(nested),
            Err(ExplorerKitError::UnknownWebView(nested))
        );
        runtime.destroy_webview(child).unwrap();
        assert_eq!(
            runtime.system_webview_is_loading(child),
            Err(ExplorerKitError::UnknownWebView(child))
        );
    }

    #[test]
    #[cfg(feature = "servo")]
    fn loading_query_preserves_servo_and_unknown_handle_errors() {
        let mut runtime = runtime();
        let session = runtime.create_session();
        let view = runtime.create_webview(session).unwrap();
        assert!(runtime
            .system_webview_is_loading(view)
            .unwrap_err()
            .to_string()
            .contains("unsupported by Servo"));
        runtime.destroy_webview(view).unwrap();
        assert_eq!(
            runtime.system_webview_is_loading(view),
            Err(ExplorerKitError::UnknownWebView(view))
        );
    }

    #[test]
    fn staged_children_and_nested_sources_are_commandable_before_the_next_pump() {
        let (mut runtime, host) = staging_runtime();
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let child = staged_child(&host.borrow(), source);
        let nested = staged_child(&host.borrow(), child);
        assert!(!host.borrow().views.contains_key(&child));
        let events = runtime.drain_events();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.created_webview.is_some())
                .count(),
            2
        );
        runtime.reload(nested).unwrap();
        assert!(host.borrow().pending_children.borrow().is_empty());
        assert!(
            matches!(host.borrow().views.get(&nested), Some(MacOsView::System(state)) if state.pending_commands.back() == Some(&WebViewCommand::Reload))
        );
        runtime.destroy_webview(source).unwrap();
        runtime.reload(child).unwrap();
        runtime.destroy_webview(child).unwrap();
        runtime.reload(nested).unwrap();
    }

    #[test]
    fn staged_child_favicon_callbacks_survive_source_close_and_retire_with_the_child() {
        let (mut runtime, host) = staging_runtime();
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let child = staged_child(&host.borrow(), source);
        let sink = host.borrow().system_sink(child, 1);
        sink.document_committed();
        let token = sink.begin_favicon().unwrap();
        sink.complete_favicon(token, favicon(1));
        assert!(!host.borrow().views.contains_key(&child));

        runtime.destroy_webview(source).unwrap();
        runtime.perform_updates(child).unwrap();
        let events = runtime.drain_events();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.event, HostEvent::FaviconChanged { .. }))
                .map(|event| (event.webview, event.event.clone()))
                .collect::<Vec<_>>(),
            vec![(
                child,
                HostEvent::FaviconChanged {
                    favicon: Some(favicon(1))
                }
            )]
        );

        let stale = sink.begin_favicon().unwrap();
        sink.document_committed();
        sink.complete_favicon(stale, favicon(2));
        runtime.perform_updates(child).unwrap();
        assert!(!runtime
            .drain_events()
            .iter()
            .any(|event| matches!(event.event, HostEvent::FaviconChanged { .. })));
        let pending = sink.begin_favicon().unwrap();
        runtime.destroy_webview(child).unwrap();
        runtime.drain_events();
        sink.complete_favicon(pending, favicon(3));
        assert!(runtime.drain_events().is_empty());
    }

    #[test]
    fn native_close_retires_staged_and_adopted_children_once_before_cleanup() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };

        for adopted in [false, true] {
            let (mut runtime, host) = staging_runtime();
            let session = runtime.create_session();
            let source = runtime.create_webview(session).unwrap();
            let child = staged_child(&host.borrow(), source);
            if adopted {
                runtime.reload(child).unwrap();
            }
            let pending = Arc::new(AtomicBool::new(false));
            let scheduled = pending.clone();
            host.borrow().callbacks.borrow_mut().wake = Arc::new(move || {
                scheduled.store(true, Ordering::Release);
            });
            let callbacks = {
                let host = host.borrow();
                SystemNewWebView {
                    source: child,
                    sink: host.system_sink(child, 1),
                    registrar: host.registrar.clone(),
                    pending: Rc::downgrade(&host.pending_children),
                    destination: None,
                    handler: Rc::default(),
                    next_request: Rc::default(),
                }
            };
            callbacks.sink.push(HostEvent::PageTitleChanged {
                title: Some("before-close".into()),
            });
            assert!(pending.swap(false, Ordering::AcqRel));
            callbacks.closed();
            assert!(
                pending.swap(false, Ordering::AcqRel),
                "native close schedules its journal drain"
            );
            callbacks.closed();
            callbacks.sink.push(HostEvent::PageTitleChanged {
                title: Some("stale".into()),
            });
            assert!(
                !pending.load(Ordering::Acquire),
                "retired callbacks do not schedule new results"
            );
            assert_eq!(
                runtime.reload(child),
                Err(ExplorerKitError::UnknownWebView(child))
            );
            let events = runtime.drain_events();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.webview == child && event.event == HostEvent::Closed)
                    .count(),
                1
            );
            let child_events: Vec<_> = events
                .iter()
                .filter(|event| event.webview == child)
                .map(|event| &event.event)
                .collect();
            assert_eq!(
                child_events[child_events.len() - 2],
                &HostEvent::PageTitleChanged {
                    title: Some("before-close".into())
                }
            );
            assert_eq!(child_events.last(), Some(&&HostEvent::Closed));
            runtime.perform_all_updates().unwrap();
            assert!(!host.borrow().views.contains_key(&child));
            assert!(host.borrow().pending_children.borrow().is_empty());
            runtime.reload(source).unwrap();
            let host_ref = host.borrow();
            let generation = match host_ref.views.get(&source).unwrap() {
                MacOsView::System(state) => state.generation,
                #[cfg(feature = "servo")]
                _ => unreachable!(),
            };
            let source_callbacks = SystemNewWebView {
                source,
                sink: host_ref.system_sink(source, generation),
                ..callbacks
            };
            drop(host_ref);
            source_callbacks.closed();
            runtime.perform_all_updates().unwrap();
            assert!(
                host.borrow().views.is_empty(),
                "an empty live batch must release the last native owner"
            );
        }
    }

    struct ParentLeaseProbe {
        pending: Weak<SystemChildViews>,
        dropped: Rc<Cell<usize>>,
    }

    impl HasWindowHandle for ParentLeaseProbe {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            Err(HandleError::Unavailable)
        }
    }

    impl Drop for ParentLeaseProbe {
        fn drop(&mut self) {
            if let Some(pending) = self.pending.upgrade() {
                assert!(
                    pending.try_borrow_mut().is_ok(),
                    "lease dropped under staging guard"
                );
            }
            self.dropped.set(self.dropped.get() + 1);
        }
    }

    #[cfg(feature = "macos-webkit-automation")]
    #[test]
    fn automation_retirement_barrier_waits_for_owners_and_callback_exit() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        struct Lease {
            owner: Weak<automation::State>,
            pending: Weak<SystemChildViews>,
            events: Rc<RefCell<Vec<&'static str>>>,
        }
        impl HasWindowHandle for Lease {
            fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
                Err(HandleError::Unavailable)
            }
        }
        impl Drop for Lease {
            fn drop(&mut self) {
                assert!(
                    self.owner.upgrade().unwrap().retirement_pending.get(),
                    "barrier must outlive owner release"
                );
                assert!(
                    self.pending.upgrade().unwrap().try_borrow_mut().is_ok(),
                    "no staging guard crosses owner destruction"
                );
                self.events.borrow_mut().push("drop");
            }
        }
        for staged in [false, true] {
            for active in [false, true] {
                for disconnect in [false, true] {
                    let (mut runtime, host) = staging_runtime();
                    let session = runtime.create_session();
                    let source = runtime.create_webview(session).unwrap();
                    let candidate = if staged {
                        staged_child(&host.borrow(), source)
                    } else {
                        runtime.create_webview(session).unwrap()
                    };
                    let events = Rc::new(RefCell::new(Vec::new()));
                    let wake_count = Arc::new(AtomicUsize::new(0));
                    let signal = wake_count.clone();
                    let (owner, token) = {
                        let mut host = host.borrow_mut();
                        host.callbacks.borrow_mut().wake = Arc::new(move || {
                            signal.fetch_add(1, Ordering::Relaxed);
                        });
                        let owner = host.automation.clone();
                        let token =
                            automation::tests::seed_retirement_probe(&owner, events.clone());
                        let lease = Rc::new(Lease {
                            owner: Rc::downgrade(&owner),
                            pending: Rc::downgrade(&host.pending_children),
                            events: events.clone(),
                        });
                        let generation = if staged {
                            let mut pending = host.pending_children.borrow_mut();
                            let state = pending.get_mut(&candidate).unwrap();
                            state.destination_parent = Some(lease);
                            state.test_callback_depth.set(usize::from(active));
                            state.generation
                        } else {
                            let state = system_state_mut(&mut host.views, candidate).unwrap();
                            state.destination_parent = Some(lease);
                            state.test_callback_depth.set(usize::from(active));
                            state.generation
                        };
                        host.system_sink(candidate, generation).invalidate();
                        host.registrar.as_ref().unwrap().retire_webview(candidate);
                        (owner, token)
                    };
                    assert!(
                        automation::tests::resolve_probe(&owner, token, Some(source))
                            .unwrap_err()
                            .to_string()
                            .contains("retirement")
                    );
                    assert!(
                        !owner.pump_one(),
                        "queued native work stays queued at the barrier"
                    );
                    assert!(events.borrow().is_empty());
                    if disconnect {
                        owner.disconnect(None);
                        assert!(owner.retirement_pending.get());
                        assert!(automation::tests::resolve_probe(&owner, token, None)
                            .unwrap_err()
                            .to_string()
                            .contains("stale"));
                    }
                    let before = wake_count.load(Ordering::Relaxed);
                    host.borrow_mut().sync_views();
                    if active {
                        assert!(events.borrow().is_empty());
                        assert!(owner.retirement_pending.get());
                        assert_eq!(wake_count.load(Ordering::Relaxed), before);
                        system_state_mut(&mut host.borrow_mut().views, candidate)
                            .unwrap()
                            .test_callback_depth
                            .set(0);
                        host.borrow_mut().sync_views();
                    }
                    assert_eq!(*events.borrow(), ["drop"]);
                    assert!(!owner.retirement_pending.get());
                    assert_eq!(wake_count.load(Ordering::Relaxed), before + 1);
                    host.borrow_mut().sync_views();
                    assert_eq!(
                        wake_count.load(Ordering::Relaxed),
                        before + 1,
                        "clearing the barrier wakes once, without a spin"
                    );
                    if disconnect {
                        assert!(automation::tests::resolve_probe(&owner, token, None)
                            .unwrap_err()
                            .to_string()
                            .contains("stale"));
                        assert!(!owner.pump_one());
                    } else {
                        automation::tests::resolve_probe(&owner, token, None).unwrap();
                        assert!(
                            owner.pump_one(),
                            "paused native command resumes after owner release"
                        );
                        assert!(!owner.pump_one());
                        assert_eq!(*events.borrow(), ["drop", "dispatch", "dispatch"]);
                    }
                    assert!(host.borrow().views.contains_key(&source));
                    assert!(!host.borrow().views.contains_key(&candidate));
                }
            }
        }
    }

    #[cfg(feature = "macos-webkit-automation")]
    #[test]
    fn final_shutdown_disconnects_before_owner_retirement_including_sync_views() {
        struct Lease {
            check: Box<dyn Fn()>,
            count: Rc<Cell<usize>>,
        }
        impl HasWindowHandle for Lease {
            fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
                Err(HandleError::Unavailable)
            }
        }
        impl Drop for Lease {
            fn drop(&mut self) {
                (self.check)();
                self.count.set(self.count.get() + 1);
            }
        }
        for retired_count in 0..=2 {
            let (mut runtime, host) = staging_runtime();
            let session = runtime.create_session();
            let first = runtime.create_webview(session).unwrap();
            let second = runtime.create_webview(session).unwrap();
            let count = Rc::new(Cell::new(0));
            {
                let mut host = host.borrow_mut();
                let check = automation::tests::pending_shutdown_probe(&host.automation);
                system_state_mut(&mut host.views, first)
                    .unwrap()
                    .destination_parent = Some(Rc::new(Lease {
                    check: Box::new(check),
                    count: count.clone(),
                }));
                if retired_count > 0 {
                    let generation = system_state_mut(&mut host.views, first).unwrap().generation;
                    host.system_sink(first, generation).invalidate();
                    host.registrar.as_ref().unwrap().retire_webview(first);
                    // first stays in the host until second's view_is_servo calls
                    // sync_views. Disconnect must already have happened then.
                    assert!(host.views.contains_key(&first));
                    assert!(host.views.contains_key(&second));
                    if retired_count == 2 {
                        let generation = system_state_mut(&mut host.views, second)
                            .unwrap()
                            .generation;
                        host.system_sink(second, generation).invalidate();
                        host.registrar.as_ref().unwrap().retire_webview(second);
                    }
                }
            }
            runtime.shutdown().unwrap();
            assert_eq!(count.get(), 1);
        }
    }

    #[test]
    fn pending_owner_leases_release_after_early_retirement_and_host_shutdown() {
        for early_close in [false, true] {
            let (mut runtime, host) = staging_runtime();
            let session = runtime.create_session();
            let source = runtime.create_webview(session).unwrap();
            let child = staged_child(&host.borrow(), source);
            let dropped = Rc::new(Cell::new(0));
            {
                let host = host.borrow();
                host.pending_children
                    .borrow_mut()
                    .get_mut(&child)
                    .unwrap()
                    .destination_parent = Some(Rc::new(ParentLeaseProbe {
                    pending: Rc::downgrade(&host.pending_children),
                    dropped: dropped.clone(),
                }));
                if early_close {
                    host.system_sink(child, 1).invalidate();
                    host.registrar.as_ref().unwrap().retire_webview(child);
                }
            }
            if early_close {
                runtime.perform_updates(source).unwrap();
                assert!(!host.borrow().views.contains_key(&child));
                assert_eq!(
                    runtime.reload(child),
                    Err(ExplorerKitError::UnknownWebView(child))
                );
            } else {
                runtime.shutdown().unwrap();
            }
            assert_eq!(dropped.get(), 1);
        }
    }

    #[test]
    fn system_child_handler_requires_an_appkit_destination_provider() {
        let calls = Rc::new(Cell::new(0));
        let requested = calls.clone();
        let mut runtime = Runtime::new(MacOsViewHost::new(
            NoopDelegate,
            MacOsViewHostOptions::new(
                SurfaceHostOptions::default(),
                WebKitDataStoreIdentifier([0x34; 16]),
            )
            .with_new_webview_destination_handler(Rc::new(move |_| {
                requested.set(requested.get() + 1);
                Err(SurfaceError::new("no slot"))
            })),
        ));
        let session = runtime.create_session();
        let source = runtime
            .create_webview_with_options(
                session,
                MacOsWebViewOptions::new(MacOsWebViewKind::SystemWebView),
            )
            .unwrap();
        runtime
            .set_new_webview_handler(source, Some(Rc::new(|_| NewWebViewDecision::Create)))
            .unwrap();
        runtime.set_new_webview_handler(source, None).unwrap();
        assert_eq!(calls.get(), 0);

        let mut no_destination = super::tests::runtime();
        let session = no_destination.create_session();
        let source = no_destination
            .create_webview_with_options(
                session,
                MacOsWebViewOptions::new(MacOsWebViewKind::SystemWebView),
            )
            .unwrap();
        assert!(no_destination
            .set_new_webview_handler(source, Some(Rc::new(|_| NewWebViewDecision::Create)))
            .unwrap_err()
            .to_string()
            .contains("AppKit destination handler"));
    }

    #[test]
    fn replacing_a_child_policy_releases_captured_resources_outside_the_cell_borrow() {
        struct Probe(Weak<RefCell<Option<Rc<NewWebViewHandler>>>>, Rc<Cell<bool>>);
        impl Drop for Probe {
            fn drop(&mut self) {
                assert!(self.0.upgrade().unwrap().try_borrow_mut().is_ok());
                self.1.set(true);
            }
        }
        let (mut runtime, host) = staging_runtime();
        host.borrow_mut().new_webview_destination_handler =
            Some(Rc::new(|_| Err(SurfaceError::new("unused"))));
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let cell = match host.borrow().views.get(&source).unwrap() {
            MacOsView::System(state) => state.handler.clone(),
            #[cfg(feature = "servo")]
            _ => unreachable!(),
        };
        let dropped = Rc::new(Cell::new(false));
        let probe = Probe(Rc::downgrade(&cell), dropped.clone());
        runtime
            .set_new_webview_handler(
                source,
                Some(Rc::new(move |_| {
                    let _keep = &probe;
                    NewWebViewDecision::Deny
                })),
            )
            .unwrap();
        runtime
            .set_new_webview_handler(source, Some(Rc::new(|_| NewWebViewDecision::Create)))
            .unwrap();
        assert!(dropped.get());
    }

    #[test]
    fn system_webview_rejects_forwarded_host_input() {
        let mut runtime = runtime();
        let session = runtime.create_session();
        let webview = runtime
            .create_webview_with_options(
                session,
                MacOsWebViewOptions::new(MacOsWebViewKind::SystemWebView),
            )
            .unwrap();

        let error = runtime
            .dispatch_input_event(webview, HostInputEvent::Focus { is_focused: true })
            .unwrap_err();
        assert!(matches!(error, ExplorerKitError::Host(_)));
        assert!(error.to_string().contains("receives native AppKit input"));
    }

    #[test]
    fn system_webview_discards_callbacks_after_destroy_and_replacement() {
        let callbacks = Rc::new(RefCell::new(SystemCallbacks::new(std::sync::Arc::new(
            || {},
        ))));
        let first = SystemCallbackSink::register_raw(callbacks.clone(), 1, 1);
        first.push(HostEvent::FocusChanged { is_focused: true });
        assert_eq!(first.drain().len(), 1);

        first.invalidate();
        let replacement = SystemCallbackSink::register_raw(callbacks, 1, 2);
        first.push(HostEvent::FocusChanged { is_focused: false });
        replacement.push(HostEvent::FocusChanged { is_focused: true });

        assert!(first.drain().is_empty());
        assert_eq!(
            replacement.drain(),
            vec![HostEvent::FocusChanged { is_focused: true }]
        );
    }

    #[test]
    fn uninstantiated_system_stop_preserves_pending_load_and_callback_state() {
        let (mut runtime, host) = staging_runtime();
        let session = runtime.create_session();
        let view = runtime.create_webview(session).unwrap();
        runtime.load_url(view, "pending.test").unwrap();
        runtime.drain_events();
        let generation = system_state_mut(&mut host.borrow_mut().views, view)
            .unwrap()
            .generation;
        assert!(runtime
            .stop_loading(view)
            .unwrap_err()
            .to_string()
            .contains("no native view"));
        assert!(runtime
            .system_webview_is_loading(view)
            .unwrap_err()
            .to_string()
            .contains("no native view"));
        let mut host = host.borrow_mut();
        let state = system_state_mut(&mut host.views, view).unwrap();
        assert_eq!(state.generation, generation);
        assert_eq!(state.pending_commands.len(), 1);
        assert!(matches!(
            state.pending_commands.front(),
            Some(WebViewCommand::LoadUrl(_))
        ));
        assert!(state.webview.is_none());
        assert!(runtime.drain_events().is_empty());
        assert!(!should_queue_system_command(
            true,
            false,
            &WebViewCommand::StopLoading
        ));
    }

    #[test]
    fn detached_system_focus_and_blur_wait_for_reattach() {
        assert!(should_queue_system_command(
            true,
            false,
            &WebViewCommand::Focus
        ));
        assert!(should_queue_system_command(
            true,
            false,
            &WebViewCommand::Blur
        ));
        assert!(!should_queue_system_command(
            true,
            true,
            &WebViewCommand::Focus
        ));
    }

    #[test]
    #[cfg(feature = "servo")]
    fn default_creation_remains_servo_backed() {
        let mut runtime = runtime();
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        runtime
            .dispatch_input_event(webview, HostInputEvent::Focus { is_focused: true })
            .unwrap();
    }

    #[test]
    #[cfg(feature = "servo")]
    fn servo_history_also_reports_the_common_navigation_state() {
        let events = with_navigation_state(Ok(vec![HostEvent::HistoryChanged {
            entries: vec!["https://example.test/".to_owned()],
            current: 0,
            can_go_back: false,
            can_go_forward: true,
        }]))
        .unwrap();

        assert!(matches!(
            events.as_slice(),
            [
                HostEvent::HistoryChanged { .. },
                HostEvent::NavigationStateChanged {
                    can_go_back: false,
                    can_go_forward: true
                }
            ]
        ));
    }

    struct EvaluationRuntime {
        runtime: Runtime<MacOsViewHost<NoopDelegate>>,
        callbacks: Rc<RefCell<SystemCallbacks>>,
    }

    #[test]
    #[cfg(feature = "servo")]
    fn shared_pump_errors_remain_host_errors_with_or_without_servo_views() {
        struct FailedPumpHost(MacOsViewHost<NoopDelegate>);
        impl Host for FailedPumpHost {
            fn set_webview_registrar(&mut self, registrar: WebViewRegistrar) {
                self.0.set_webview_registrar(registrar);
            }
            fn create_webview(
                &mut self,
                session: SessionHandle,
                webview: WebViewHandle,
            ) -> Result<(), HostError> {
                if self.0.views.is_empty() {
                    self.0.create_webview_with_options(
                        session,
                        webview,
                        MacOsWebViewOptions::new(MacOsWebViewKind::SystemWebView),
                    )
                } else {
                    self.0.create_webview(session, webview)
                }
            }
            fn observe_webview_events(
                &mut self,
                _: WebViewHandle,
                _: &[HostEvent],
            ) -> Result<(), HostError> {
                Ok(())
            }
            fn perform_all_updates(
                &mut self,
                webviews: &[WebViewHandle],
            ) -> Result<Vec<(WebViewHandle, Result<Vec<HostEvent>, HostError>)>, HostError>
            {
                self.0
                    .finish_servo_pump(webviews, Err(HostError::new("shared pump failed")))
            }
        }
        for include_servo in [false, true] {
            let host = MacOsViewHost::new(
                NoopDelegate,
                MacOsViewHostOptions::new(
                    SurfaceHostOptions::default(),
                    WebKitDataStoreIdentifier([0x39; 16]),
                ),
            );
            let callbacks = host.callbacks.clone();
            let mut runtime = Runtime::new(FailedPumpHost(host));
            let session = runtime.create_session();
            let system = runtime.create_webview(session).unwrap();
            if include_servo {
                runtime.create_webview(session).unwrap();
            }
            let target = *callbacks.borrow().views.keys().next().unwrap();
            let sink = SystemCallbackSink { target, callbacks };
            let title = HostEvent::PageTitleChanged {
                title: Some("system callback survives".into()),
            };
            sink.push(title.clone());
            assert!(
                matches!(runtime.perform_all_updates(), Err(explorerkit_embedder::RuntimeError::Host(error)) if error.message() == "shared pump failed")
            );
            let events = runtime.drain_events();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].webview, system);
            assert_eq!(events[0].event, title);
            assert!(runtime.perform_all_updates().is_err());
            assert!(runtime.drain_events().is_empty());
        }
    }

    impl EvaluationRuntime {
        fn new() -> Self {
            let host = MacOsViewHost::new(
                NoopDelegate,
                MacOsViewHostOptions::new(
                    SurfaceHostOptions::default(),
                    WebKitDataStoreIdentifier([0x39; 16]),
                ),
            );
            let callbacks = host.callbacks.clone();
            Self {
                runtime: Runtime::new(host),
                callbacks,
            }
        }

        fn view(&mut self) -> (WebViewHandle, SystemCallbackSink) {
            let session = self.runtime.create_session();
            let webview = self
                .runtime
                .create_webview_with_options(
                    session,
                    MacOsWebViewOptions::new(MacOsWebViewKind::SystemWebView),
                )
                .unwrap();
            let target = *self
                .callbacks
                .borrow()
                .views
                .keys()
                .find(|(handle, _)| *handle == webview.raw())
                .unwrap();
            (
                webview,
                SystemCallbackSink {
                    target,
                    callbacks: self.callbacks.clone(),
                },
            )
        }

        fn events(&mut self) -> Vec<(WebViewHandle, HostEvent)> {
            self.runtime.perform_all_updates().unwrap();
            self.runtime
                .drain_events()
                .into_iter()
                .map(|event| {
                    assert!(event.managed_child_webview_id.is_none());
                    (event.webview, event.event)
                })
                .collect()
        }
    }

    fn favicon(marker: u8) -> explorerkit_embedder::FaviconImage {
        explorerkit_embedder::FaviconImage {
            width: 1,
            height: 1,
            format: explorerkit_embedder::FaviconPixelFormat::RGBA8,
            bytes: vec![marker, 0, 0, 255],
        }
    }

    #[test]
    fn current_document_favicon_successes_route_only_to_their_runtime_view() {
        let mut test = EvaluationRuntime::new();
        let (a, first) = test.view();
        let (b, second) = test.view();
        assert_eq!(first.begin_favicon(), None);
        first.document_committed();
        second.document_committed();
        test.events();
        let earlier = first.begin_favicon().unwrap();
        let latest = first.begin_favicon().unwrap();
        let sibling = second.begin_favicon().unwrap();
        first.complete_favicon(earlier, favicon(1));
        second.complete_favicon(sibling, favicon(2));
        first.complete_favicon(latest, favicon(3));
        first.complete_favicon(earlier, favicon(4));
        let events = test.events();
        assert_eq!(
            events
                .iter()
                .filter(|(v, _)| *v == a)
                .cloned()
                .collect::<Vec<_>>(),
            vec![1, 3, 4]
                .into_iter()
                .map(|marker| (
                    a,
                    HostEvent::FaviconChanged {
                        favicon: Some(favicon(marker))
                    }
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            events
                .iter()
                .filter(|(v, _)| *v == b)
                .cloned()
                .collect::<Vec<_>>(),
            vec![(
                b,
                HostEvent::FaviconChanged {
                    favicon: Some(favicon(2))
                }
            )]
        );
    }

    #[test]
    fn favicon_commit_preserves_finalized_events_and_retires_pending_without_clear() {
        for transfer_to_runtime in [false, true] {
            let mut test = EvaluationRuntime::new();
            let (view, sink) = test.view();
            sink.document_committed();
            test.events();
            let finalized = sink.begin_favicon().unwrap();
            sink.complete_favicon(finalized, favicon(1));
            if transfer_to_runtime {
                test.runtime.perform_updates(view).unwrap();
            }
            let pending = sink.begin_favicon().unwrap();
            sink.document_committed(); // Same-URL reload is also a new document.
            sink.complete_favicon(pending, favicon(2));
            assert!(!sink.favicon_is_current(pending));
            assert_eq!(
                test.events(),
                vec![
                    (
                        view,
                        HostEvent::FaviconChanged {
                            favicon: Some(favicon(1))
                        }
                    ),
                    (
                        view,
                        HostEvent::LoadStatusChanged {
                            status: LoadStatusKind::Started
                        }
                    ),
                ]
            );
        }
    }

    #[test]
    fn favicon_process_and_lifetime_retirement_reject_late_completions() {
        let mut test = EvaluationRuntime::new();
        let (view, sink) = test.view();
        sink.document_committed();
        test.events();
        let pending = sink.begin_favicon().unwrap();
        sink.process_terminated();
        sink.complete_favicon(pending, favicon(1));
        assert_eq!(sink.begin_favicon(), None);
        let events = test.events();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], (_, HostEvent::Crashed { .. })));
        sink.document_committed();
        test.events();
        let after_recovery = sink.begin_favicon().unwrap();
        sink.complete_favicon(pending, favicon(2));
        assert!(test.events().is_empty());
        sink.complete_favicon(after_recovery, favicon(3));
        test.runtime.destroy_webview(view).unwrap();
        sink.complete_favicon(after_recovery, favicon(4));
        assert!(test.events().is_empty());
        let replacement =
            SystemCallbackSink::register_raw(test.callbacks.clone(), view.raw(), sink.target.1 + 1);
        replacement.document_committed();
        replacement.drain();
        sink.complete_favicon(after_recovery, favicon(5));
        assert!(replacement.drain().is_empty());
        let token = replacement.begin_favicon().unwrap();
        replacement.complete_favicon(token, favicon(6));
        assert_eq!(
            replacement.drain(),
            vec![HostEvent::FaviconChanged {
                favicon: Some(favicon(6))
            }]
        );
    }

    #[test]
    fn favicon_url_and_load_completion_events_do_not_retire_the_document_request() {
        let callbacks = Rc::new(RefCell::new(SystemCallbacks::new(std::sync::Arc::new(
            || {},
        ))));
        let sink = SystemCallbackSink::register_raw(callbacks, 1, 1);
        sink.document_committed();
        sink.drain();
        let token = sink.begin_favicon().unwrap();
        sink.push(HostEvent::UrlChanged {
            url: "https://example.org/#fragment".into(),
        });
        sink.push(HostEvent::LoadStatusChanged {
            status: LoadStatusKind::Complete,
        });
        assert!(sink.favicon_is_current(token));
        sink.drain();
        sink.complete_favicon(token, favicon(1));
        assert_eq!(
            sink.drain(),
            vec![HostEvent::FaviconChanged {
                favicon: Some(favicon(1))
            }]
        );
    }

    fn success(id: &str) -> HostEvent {
        evaluation_event(id, Ok(r#"{"type":"string","value":"snapshot"}"#.to_owned()))
    }

    fn complete(sink: &SystemCallbackSink, id: &str, token: u64) {
        sink.complete_evaluation(
            id,
            token,
            Ok(r#"{"type":"string","value":"snapshot"}"#.to_owned()),
        );
    }

    #[test]
    fn unready_evaluation_is_correlated_and_never_deferred() {
        let mut test = EvaluationRuntime::new();
        let (view, sink) = test.view();
        assert!(test
            .runtime
            .evaluate_javascript(view, " \t", "unused")
            .is_err());
        test.runtime
            .evaluate_javascript(view, "unready", "must not be retained")
            .unwrap();
        // Readiness failures transfer immediately to Runtime's queue. Even a
        // failed first native construction resetting its sink cannot erase them.
        sink.clear();
        assert_eq!(
            test.events(),
            vec![(
                view,
                evaluation_event(
                    "unready",
                    Err(JavaScriptEvaluationErrorKind::WebViewNotReady),
                )
            )]
        );
        sink.document_committed();
        assert_eq!(
            test.events(),
            vec![(
                view,
                HostEvent::LoadStatusChanged {
                    status: LoadStatusKind::Started
                }
            )]
        );
        assert!(sink.callbacks.borrow().views[&sink.target]
            .pending
            .is_empty());
        test.runtime.destroy_webview(view).unwrap();
        assert!(matches!(
            test.runtime.evaluate_javascript(view, "closed", "unused"),
            Err(ExplorerKitError::UnknownWebView(_))
        ));
        assert!(test.events().is_empty());
    }

    #[test]
    fn per_view_ids_reject_pending_duplicates_and_allow_out_of_order_results() {
        let mut test = EvaluationRuntime::new();
        let (a, first) = test.view();
        let (b, second) = test.view();
        first.document_committed();
        second.document_committed();
        test.events();
        let text = first.begin_evaluation("same-id").unwrap().unwrap();
        let metadata = first.begin_evaluation(" metadata ").unwrap().unwrap();
        let sibling = second.begin_evaluation("same-id").unwrap().unwrap();
        assert!(first.begin_evaluation("same-id").is_err());
        complete(&first, " metadata ", metadata);
        complete(&first, "same-id", text);
        complete(&second, "same-id", sibling);
        let events = test.events();
        assert_eq!(
            events
                .iter()
                .filter(|(view, _)| *view == a)
                .cloned()
                .collect::<Vec<_>>(),
            vec![(a, success(" metadata ")), (a, success("same-id")),]
        );
        assert_eq!(
            events
                .iter()
                .filter(|(view, _)| *view == b)
                .cloned()
                .collect::<Vec<_>>(),
            vec![(b, success("same-id"))]
        );
        let reused = first.begin_evaluation("same-id").unwrap().unwrap();
        complete(&first, "same-id", text);
        assert!(test.events().is_empty());
        complete(&first, "same-id", reused);
        complete(&first, "same-id", reused);
        assert_eq!(test.events(), vec![(a, success("same-id"))]);
    }

    #[test]
    fn commit_preserves_historical_results_at_both_sdk_queue_stages() {
        for transfer_before_commit in [false, true] {
            let mut test = EvaluationRuntime::new();
            let (view, sink) = test.view();
            sink.document_committed();
            test.events();
            let finalized = sink.begin_evaluation("finalized").unwrap().unwrap();
            let pending = sink.begin_evaluation("pending").unwrap().unwrap();
            complete(&sink, "finalized", finalized);
            if transfer_before_commit {
                test.runtime.perform_updates(view).unwrap();
            }
            // Every observed commit is a boundary; no URL comparison is involved.
            sink.document_committed();
            complete(&sink, "pending", pending);
            let replacement = sink.begin_evaluation("pending").unwrap().unwrap();
            sink.complete_evaluation(
                "pending",
                pending,
                Err(NativeEvaluationError::ProcessTerminated),
            );
            complete(&sink, "pending", pending);
            assert!(sink.begin_evaluation("pending").is_err());
            complete(&sink, "pending", replacement);
            assert_eq!(
                test.events(),
                vec![
                    (view, success("finalized")),
                    (
                        view,
                        HostEvent::LoadStatusChanged {
                            status: LoadStatusKind::Started
                        }
                    ),
                    (
                        view,
                        evaluation_event(
                            "pending",
                            Err(JavaScriptEvaluationErrorKind::DocumentNotFound)
                        )
                    ),
                    (view, success("pending")),
                ]
            );
            // A repeated same-URL commit also retires current pending work.
            let reload = sink.begin_evaluation("reload").unwrap().unwrap();
            sink.document_committed();
            complete(&sink, "reload", reload);
            assert_eq!(
                test.events(),
                vec![
                    (
                        view,
                        HostEvent::LoadStatusChanged {
                            status: LoadStatusKind::Started
                        }
                    ),
                    (
                        view,
                        evaluation_event(
                            "reload",
                            Err(JavaScriptEvaluationErrorKind::DocumentNotFound)
                        )
                    ),
                ]
            );
        }
    }

    #[test]
    fn commands_url_changes_and_load_completion_are_not_commit_boundaries() {
        let mut test = EvaluationRuntime::new();
        let (view, sink) = test.view();
        sink.document_committed();
        test.events();
        let token = sink.begin_evaluation("old-document").unwrap().unwrap();
        test.runtime
            .dispatch_webview_command(view, WebViewCommand::Reload)
            .unwrap();
        sink.push(HostEvent::UrlChanged {
            url: "https://fixture.test/spa".to_owned(),
        });
        sink.push(HostEvent::LoadStatusChanged {
            status: LoadStatusKind::Complete,
        });
        complete(&sink, "old-document", token);
        assert_eq!(
            test.events(),
            vec![
                (
                    view,
                    HostEvent::UrlChanged {
                        url: "https://fixture.test/spa".to_owned()
                    }
                ),
                (
                    view,
                    HostEvent::LoadStatusChanged {
                        status: LoadStatusKind::Complete
                    }
                ),
                (view, success("old-document")),
            ]
        );
    }

    #[test]
    fn close_discards_both_queues_and_pending_callbacks_for_either_sibling() {
        for close_first in [false, true] {
            for transfer_before_close in [false, true] {
                let mut test = EvaluationRuntime::new();
                let first = test.view();
                let second = test.view();
                let ((closed, sink), (survivor, sibling)) = if close_first {
                    (first, second)
                } else {
                    (second, first)
                };
                sink.document_committed();
                sibling.document_committed();
                test.events();
                let queued = sink.begin_evaluation("queued").unwrap().unwrap();
                let pending = sink.begin_evaluation("shared").unwrap().unwrap();
                let live = sibling.begin_evaluation("shared").unwrap().unwrap();
                complete(&sink, "queued", queued);
                if transfer_before_close {
                    test.runtime.perform_updates(closed).unwrap();
                }
                test.runtime.destroy_webview(closed).unwrap();
                let (replacement, new_sink) = test.view();
                assert_ne!(replacement, closed);
                new_sink.document_committed();
                let new = new_sink.begin_evaluation("shared").unwrap().unwrap();
                complete(&sink, "shared", pending);
                sink.process_terminated();
                sink.document_committed();
                complete(&sibling, "shared", live);
                complete(&new_sink, "shared", new);
                let events = test.events();
                assert!(!events.iter().any(|(view, _)| *view == closed));
                assert_eq!(
                    events
                        .iter()
                        .filter(|(view, _)| *view == survivor)
                        .cloned()
                        .collect::<Vec<_>>(),
                    vec![(survivor, success("shared"))]
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|(view, _)| *view == replacement)
                        .cloned()
                        .collect::<Vec<_>>(),
                    vec![
                        (
                            replacement,
                            HostEvent::LoadStatusChanged {
                                status: LoadStatusKind::Started
                            }
                        ),
                        (replacement, success("shared")),
                    ]
                );
            }
        }
    }

    #[test]
    fn termination_error_and_notification_orders_retire_each_request_once() {
        for notification_first in [false, true] {
            let mut test = EvaluationRuntime::new();
            let (view, sink) = test.view();
            sink.document_committed();
            test.events();
            let finalized = sink.begin_evaluation("finalized").unwrap().unwrap();
            let one = sink.begin_evaluation("one").unwrap().unwrap();
            let two = sink.begin_evaluation("two").unwrap().unwrap();
            complete(&sink, "finalized", finalized);
            if notification_first {
                sink.process_terminated();
            }
            sink.complete_evaluation("one", one, Err(NativeEvaluationError::ProcessTerminated));
            // Ineligibility applies even before the notification arrives.
            assert_eq!(sink.begin_evaluation("unready").unwrap(), None);
            if !notification_first {
                sink.process_terminated();
            }
            complete(&sink, "two", two);
            sink.complete_evaluation("one", one, Err(NativeEvaluationError::ProcessTerminated));
            let events = test.events();
            assert_eq!(events.len(), 5);
            assert_eq!(events[0], (view, success("finalized")));
            assert_eq!(
                events
                    .iter()
                    .filter(|(_, event)| matches!(event, HostEvent::Crashed { .. }))
                    .count(),
                1
            );
            for id in ["one", "two"] {
                assert_eq!(
                    events
                        .iter()
                        .filter(|(_, event)| *event
                            == evaluation_event(
                                id,
                                Err(JavaScriptEvaluationErrorKind::InternalError)
                            ))
                        .count(),
                    1
                );
            }
            assert!(events.contains(&(
                view,
                evaluation_event(
                    "unready",
                    Err(JavaScriptEvaluationErrorKind::WebViewNotReady)
                )
            )));
            sink.document_committed();
            let reused = sink.begin_evaluation("one").unwrap().unwrap();
            sink.complete_evaluation("one", one, Err(NativeEvaluationError::ProcessTerminated));
            complete(&sink, "one", reused);
            assert_eq!(
                test.events(),
                vec![
                    (
                        view,
                        HostEvent::LoadStatusChanged {
                            status: LoadStatusKind::Started
                        }
                    ),
                    (view, success("one")),
                ]
            );
        }
    }

    #[test]
    fn termination_preserves_finalized_failure_and_retires_only_pending_reads() {
        for transfer_finalized_to_runtime in [false, true] {
            let mut test = EvaluationRuntime::new();
            let (view, sink) = test.view();
            sink.document_committed();
            test.events();
            let finalized = sink.begin_evaluation("finalized").unwrap().unwrap();
            let pending = sink.begin_evaluation("pending").unwrap().unwrap();
            sink.complete_evaluation(
                "finalized",
                finalized,
                Err(NativeEvaluationError::Result(
                    JavaScriptEvaluationErrorKind::SerializationError,
                )),
            );
            if transfer_finalized_to_runtime {
                test.runtime.perform_all_updates().unwrap();
            }

            sink.process_terminated();
            assert_eq!(sink.begin_evaluation("later").unwrap(), None);
            complete(&sink, "pending", pending);
            sink.complete_evaluation(
                "finalized",
                finalized,
                Err(NativeEvaluationError::ProcessTerminated),
            );

            let events = test.events();
            assert_eq!(events.len(), 4);
            assert_eq!(
                events[0],
                (
                    view,
                    evaluation_event(
                        "finalized",
                        Err(JavaScriptEvaluationErrorKind::SerializationError)
                    )
                )
            );
            assert_eq!(events[1].0, view);
            assert!(matches!(events[1].1, HostEvent::Crashed { .. }));
            assert_eq!(
                events[2],
                (
                    view,
                    evaluation_event("pending", Err(JavaScriptEvaluationErrorKind::InternalError))
                )
            );
            assert_eq!(
                events[3],
                (
                    view,
                    evaluation_event("later", Err(JavaScriptEvaluationErrorKind::WebViewNotReady))
                )
            );
            assert!(test.events().is_empty());
        }
    }

    #[test]
    fn native_view_invalidation_retires_pending_and_makes_later_reads_unready() {
        let mut test = EvaluationRuntime::new();
        let (view, sink) = test.view();
        sink.document_committed();
        test.events();
        let one = sink.begin_evaluation("one").unwrap().unwrap();
        let two = sink.begin_evaluation("two").unwrap().unwrap();
        sink.complete_evaluation("one", one, Err(NativeEvaluationError::ViewInvalidated));
        complete(&sink, "two", two);
        assert_eq!(sink.begin_evaluation("later").unwrap(), None);
        let events = test.events();
        assert_eq!(events.len(), 3);
        for id in ["one", "two"] {
            assert!(events.contains(&(
                view,
                evaluation_event(id, Err(JavaScriptEvaluationErrorKind::InternalError))
            )));
        }
        assert!(events.contains(&(
            view,
            evaluation_event("later", Err(JavaScriptEvaluationErrorKind::WebViewNotReady))
        )));
    }
}

#[cfg(not(feature = "servo"))]
fn servo_not_compiled() -> HostError {
    HostError::new("Servo backend is not compiled; enable explorerkit feature 'servo'")
}

#[cfg(not(feature = "macos-webkit"))]
fn system_not_compiled() -> HostError {
    HostError::new(
        "SystemWebView backend is not compiled; enable explorerkit feature 'macos-webkit'",
    )
}

#[cfg(test)]
mod build_selection_tests {
    use super::*;
    use crate::surface::SurfaceTarget;
    use crate::Runtime;
    use std::cell::Cell;

    #[derive(Default)]
    struct UnattachedDelegate {
        native_requests: Cell<usize>,
    }

    impl SurfaceDelegate for UnattachedDelegate {
        type Frame<'a> = SurfaceFrame<'a>;

        fn render_target(
            &mut self,
            _: &HostSurface,
            _: SurfaceViewport,
        ) -> Result<SurfaceTarget<'_>, SurfaceError> {
            self.native_requests.set(self.native_requests.get() + 1);
            Err(SurfaceError::new("this fixture must not request a surface"))
        }

        fn present_frame(
            &mut self,
            _: &HostSurface,
            frame: &mut Self::Frame<'_>,
        ) -> Result<(), SurfaceError> {
            // Keep the same consumer callback body compilable in every build.
            let _ = (
                frame.info(),
                frame.viewport(),
                frame.mode(),
                frame.did_present(),
            );
            let _: Option<crate::surface::SurfaceFrameImage> = frame.read_rgba();
            let _: Option<crate::surface::macos::GpuFrame> = frame.take_gpu_frame()?;
            frame.present();
            Ok(())
        }
    }

    impl AppKitViewDelegate for UnattachedDelegate {
        fn appkit_parent(
            &mut self,
            _: &HostSurface,
            _: SurfaceViewport,
        ) -> Result<NativeSurface<'_>, SurfaceError> {
            self.native_requests.set(self.native_requests.get() + 1);
            Err(SurfaceError::new("this fixture must not request a parent"))
        }
    }

    fn host() -> MacOsViewHost<UnattachedDelegate> {
        MacOsViewHost::new(
            UnattachedDelegate::default(),
            MacOsViewHostOptions::new(
                SurfaceHostOptions::default(),
                WebKitDataStoreIdentifier([0; 16]),
            ),
        )
    }

    #[test]
    fn reports_compiled_integrations_without_native_initialization() {
        #[cfg(all(feature = "servo", feature = "macos-webkit"))]
        let expected = [MacOsWebViewKind::Servo, MacOsWebViewKind::SystemWebView];
        #[cfg(all(feature = "servo", not(feature = "macos-webkit")))]
        let expected = [MacOsWebViewKind::Servo];
        #[cfg(all(feature = "macos-webkit", not(feature = "servo")))]
        let expected = [MacOsWebViewKind::SystemWebView];
        assert_eq!(compiled_engines(), &expected);
        let mut host = host();
        assert!(host.views.is_empty());
        assert_eq!(host.services().native_requests.get(), 0);
        assert_eq!(host.services_mut().native_requests.get(), 0);
        assert_eq!(MacOsWebViewOptions::default().kind, MacOsWebViewKind::Servo);
    }

    #[test]
    fn compiled_kind_creates_and_disposes_without_attaching_a_surface() {
        for &kind in compiled_engines() {
            let mut runtime = Runtime::new(host());
            let session = runtime.create_session();
            let view = runtime
                .create_webview_with_options(session, MacOsWebViewOptions::new(kind))
                .unwrap();
            runtime.load_url(view, "https://example.invalid/").unwrap();
            runtime.destroy_webview(view).unwrap();
            assert!(runtime.load_url(view, "https://example.invalid/").is_err());
        }
    }

    #[cfg(not(all(feature = "servo", feature = "macos-webkit")))]
    #[test]
    fn absent_kind_rejects_creation_without_disturbing_a_sibling() {
        let mut runtime = Runtime::new(host());
        let session = runtime.create_session();
        let sibling = runtime
            .create_webview_with_options(session, MacOsWebViewOptions::new(compiled_engines()[0]))
            .unwrap();
        #[cfg(not(feature = "servo"))]
        let (absent, message) = (
            MacOsWebViewKind::Servo,
            "Servo backend is not compiled; enable explorerkit feature 'servo'",
        );
        #[cfg(not(feature = "macos-webkit"))]
        let (absent, message) = (
            MacOsWebViewKind::SystemWebView,
            "SystemWebView backend is not compiled; enable explorerkit feature 'macos-webkit'",
        );
        let error = runtime
            .create_webview_with_options(session, MacOsWebViewOptions::new(absent))
            .unwrap_err();
        assert!(error.to_string().contains(message));
        #[cfg(not(feature = "servo"))]
        assert!(runtime
            .create_webview(session)
            .unwrap_err()
            .to_string()
            .contains(message));
        runtime
            .load_url(sibling, "https://example.invalid/still-live")
            .unwrap();
        runtime.destroy_webview(sibling).unwrap();
        runtime.perform_all_updates().unwrap();
        assert!(runtime.drain_events().is_empty());
    }
}
