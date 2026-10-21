//! Reusable ExplorerKit runtime/webview ownership for facade and host adapters.
//!
//! This module keeps shared command dispatch, mockable webview state, and
//! event draining behind `explorerkit-embedder` so facades do not duplicate the
//! Servo-shaped runtime path.

use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::rc::Rc;
use std::sync::{Arc, Mutex, Weak};

use crate::{
    ContextMenuAction, HostEvent, HostInputEvent, LoadStatusKind, NavigationError,
    NavigationRequest, PopupCreated, PopupRequestPolicy, ServoAdapterState,
};
use explorerkit_host::{HostSurface, SurfaceSize, SurfaceViewport};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionHandle(u64);

impl SessionHandle {
    pub fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WebViewHandle(u64);

impl WebViewHandle {
    pub fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplorerKitEvent {
    pub webview: WebViewHandle,
    /// The registered original child, only on its source's `PopupCreated` receipt.
    pub created_webview: Option<WebViewHandle>,
    pub managed_child_webview_id: Option<String>,
    pub event: HostEvent,
}

/// Metadata supplied during a synchronous native child-creation request.
///
/// ExplorerKit owns this interface. Servo's `request_create_new` supplies no
/// target URL, window features or popup preference; absent native metadata remains `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWebViewRequest {
    /// The actual source, including for a request made by an existing child.
    pub source: WebViewHandle,
    /// Opaque identifier unique within this Runtime's lifetime.
    pub request_id: String,
    /// The source's last known URL, when available.
    pub parent_url: Option<String>,
    /// The native target URL, when the engine supplies one.
    pub target_url: Option<String>,
    /// Native window-feature metadata, when supplied.
    pub window_features: Option<String>,
    /// Engine-computed popup preference; `None` when unavailable.
    /// A known `false` does not promise tab placement.
    pub wants_popup: Option<bool>,
}

/// Immediate disposition of an original native child-creation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewWebViewDecision {
    /// Apply the engine's native denial.
    Deny,
    /// Prepare and retain the original child using the backend's presentation requirements.
    ///
    /// Servo allows later surface attachment. A native backend may require an
    /// application-provided destination during the synchronous creation callback.
    Create,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebViewCommand {
    LoadUrl(NavigationRequest),
    Reload,
    /// Request the backend's native stop operation, where supported.
    StopLoading,
    GoBack,
    GoForward,
    Focus,
    Blur,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostCall {
    CreateWebView {
        session: SessionHandle,
        webview: WebViewHandle,
    },
    DestroyWebView {
        webview: WebViewHandle,
    },
    AttachSurface {
        webview: WebViewHandle,
        surface: HostSurface,
        size: SurfaceSize,
    },
    AttachSurfaceWithViewport {
        webview: WebViewHandle,
        surface: HostSurface,
        viewport: SurfaceViewport,
    },
    ResizeSurface {
        webview: WebViewHandle,
        size: SurfaceSize,
    },
    SetSurfaceScaleFactor {
        webview: WebViewHandle,
        scale_factor: f32,
    },
    UpdateSurfaceViewport {
        webview: WebViewHandle,
        viewport: SurfaceViewport,
    },
    DetachSurface {
        webview: WebViewHandle,
    },
    SetPopupPolicy {
        webview: WebViewHandle,
        popup_policy: PopupRequestPolicy,
    },
    AttachManagedChildSurface {
        webview: WebViewHandle,
        child_webview_id: String,
        surface: HostSurface,
        viewport: SurfaceViewport,
    },
    ResizeManagedChildSurface {
        webview: WebViewHandle,
        child_webview_id: String,
        size: SurfaceSize,
    },
    UpdateManagedChildSurfaceViewport {
        webview: WebViewHandle,
        child_webview_id: String,
        viewport: SurfaceViewport,
    },
    DetachManagedChildSurface {
        webview: WebViewHandle,
        child_webview_id: String,
    },
    PerformManagedChildUpdates {
        webview: WebViewHandle,
        child_webview_id: String,
    },
    DispatchWebViewCommand {
        webview: WebViewHandle,
        command: WebViewCommand,
    },
    ResolveNavigationRequest {
        webview: WebViewHandle,
        navigation_id: String,
        allow: bool,
    },
    PerformUpdates {
        webview: WebViewHandle,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError {
    message: String,
}

impl HostError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for HostError {}

#[derive(Debug, PartialEq, Eq)]
pub enum RuntimeError {
    UnknownSession(SessionHandle),
    UnknownWebView(WebViewHandle),
    SurfaceAlreadyAttached(WebViewHandle),
    SurfaceNotAttached(WebViewHandle),
    ManagedChildSurfaceAlreadyAttached(WebViewHandle, String),
    ManagedChildSurfaceNotAttached(WebViewHandle, String),
    InvalidUrl(NavigationError),
    Host(HostError),
    WebView {
        webview: WebViewHandle,
        error: HostError,
    },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSession(session) => {
                write!(formatter, "session {} is not registered", session.raw())
            }
            Self::UnknownWebView(webview) => {
                write!(formatter, "webview {} is not registered", webview.raw())
            }
            Self::SurfaceAlreadyAttached(webview) => {
                write!(
                    formatter,
                    "webview {} already has an attached surface",
                    webview.raw()
                )
            }
            Self::SurfaceNotAttached(webview) => {
                write!(
                    formatter,
                    "webview {} does not have an attached surface",
                    webview.raw()
                )
            }
            Self::ManagedChildSurfaceAlreadyAttached(webview, child_webview_id) => {
                write!(
                    formatter,
                    "managed child webview {child_webview_id} for webview {} already has an attached surface",
                    webview.raw()
                )
            }
            Self::ManagedChildSurfaceNotAttached(webview, child_webview_id) => {
                write!(
                    formatter,
                    "managed child webview {child_webview_id} for webview {} does not have an attached surface",
                    webview.raw()
                )
            }
            Self::InvalidUrl(error) => write!(formatter, "{error}"),
            Self::Host(error) => write!(formatter, "{error}"),
            Self::WebView { webview, error } => {
                write!(formatter, "webview {}: {error}", webview.raw())
            }
        }
    }
}

impl Error for RuntimeError {}

impl From<NavigationError> for RuntimeError {
    fn from(error: NavigationError) -> Self {
        Self::InvalidUrl(error)
    }
}

impl From<HostError> for RuntimeError {
    fn from(error: HostError) -> Self {
        Self::Host(error)
    }
}

pub trait Host {
    /// Installs the Runtime's weak registrar once, before root creation.
    ///
    /// Ignoring this integration hook does not enable child creation.
    fn set_webview_registrar(&mut self, _registrar: WebViewRegistrar) {}

    /// Performs final host shutdown after view destruction has been attempted.
    ///
    /// Releases any views whose individual destruction failed before retiring shared
    /// resources. The runtime calls this even when a view reports a teardown error.
    /// Servo-backed hosts stop the process engine permanently; ordinary host drop
    /// instead releases the active owner while retaining the engine for reuse.
    fn shutdown(&mut self) -> Result<(), HostError> {
        Ok(())
    }

    fn create_webview(
        &mut self,
        session: SessionHandle,
        webview: WebViewHandle,
    ) -> Result<(), HostError>;

    /// Destroys one view, detaching its renderer before releasing its resources.
    ///
    /// Servo-backed hosts close a view by dropping its last `servo::WebView` handle.
    /// The default rejects destruction so hosts cannot silently retain a removed view.
    fn destroy_webview(&mut self, _webview: WebViewHandle) -> Result<(), HostError> {
        Err(HostError::new(
            "this host does not support destroying individual webviews",
        ))
    }

    /// Updates the supplied views, preserving each result's originating handle.
    ///
    /// A shared-engine host can pump once and then present/drain every view. The outer
    /// error belongs to the host/engine; individual failures remain view-scoped.
    #[allow(
        clippy::type_complexity,
        reason = "Preserve the host's existing per-view result shape."
    )]
    fn perform_all_updates(
        &mut self,
        webviews: &[WebViewHandle],
    ) -> Result<Vec<(WebViewHandle, Result<Vec<HostEvent>, HostError>)>, HostError> {
        Ok(webviews
            .iter()
            .map(|&view| (view, self.perform_updates(view)))
            .collect())
    }

    fn attach_surface(
        &mut self,
        _webview: WebViewHandle,
        _surface: &HostSurface,
        _size: SurfaceSize,
    ) -> Result<Vec<HostEvent>, HostError> {
        Ok(Vec::new())
    }

    fn attach_surface_with_viewport(
        &mut self,
        webview: WebViewHandle,
        surface: &HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        let mut events = self.attach_surface(webview, surface, viewport.size)?;
        events.extend(self.set_surface_scale_factor(webview, viewport.scale_factor)?);
        Ok(events)
    }

    fn resize_surface(
        &mut self,
        _webview: WebViewHandle,
        _size: SurfaceSize,
    ) -> Result<Vec<HostEvent>, HostError> {
        Ok(Vec::new())
    }

    fn set_surface_scale_factor(
        &mut self,
        _webview: WebViewHandle,
        _scale_factor: f32,
    ) -> Result<Vec<HostEvent>, HostError> {
        Ok(Vec::new())
    }

    fn update_surface_viewport(
        &mut self,
        webview: WebViewHandle,
        viewport: SurfaceViewport,
        previous_viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        let mut events = Vec::new();
        if previous_viewport.size != viewport.size {
            events.extend(self.resize_surface(webview, viewport.size)?);
        }
        if previous_viewport.scale_factor != viewport.scale_factor {
            events.extend(self.set_surface_scale_factor(webview, viewport.scale_factor)?);
        }
        Ok(events)
    }

    fn detach_surface(&mut self, _webview: WebViewHandle) -> Result<Vec<HostEvent>, HostError> {
        Ok(Vec::new())
    }

    fn set_popup_policy(
        &mut self,
        _webview: WebViewHandle,
        _popup_policy: PopupRequestPolicy,
    ) -> Result<Vec<HostEvent>, HostError> {
        Ok(Vec::new())
    }

    /// Installs the synchronous per-request handler for one view.
    ///
    /// This ExplorerKit-owned seam wraps the engine's original native request.
    /// The callback must not reenter Runtime or present app UI. Unsupported hosts
    /// reject the operation; `None` means denial on hosts implementing this seam.
    #[allow(
        clippy::type_complexity,
        reason = "Keep the approved callback signature explicit."
    )]
    fn set_new_webview_handler(
        &mut self,
        _webview: WebViewHandle,
        _handler: Option<Rc<dyn Fn(&NewWebViewRequest) -> NewWebViewDecision>>,
    ) -> Result<(), HostError> {
        Err(HostError::new(
            "this host does not support registered native children",
        ))
    }

    fn attach_managed_child_surface(
        &mut self,
        _webview: WebViewHandle,
        _child_webview_id: &str,
        _surface: &HostSurface,
        _viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        Ok(Vec::new())
    }

    fn resize_managed_child_surface(
        &mut self,
        _webview: WebViewHandle,
        _child_webview_id: &str,
        _size: SurfaceSize,
    ) -> Result<Vec<HostEvent>, HostError> {
        Ok(Vec::new())
    }

    fn update_managed_child_surface_viewport(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
        viewport: SurfaceViewport,
        previous_viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        let mut events = Vec::new();
        if previous_viewport.size != viewport.size {
            events.extend(self.resize_managed_child_surface(
                webview,
                child_webview_id,
                viewport.size,
            )?);
        }
        Ok(events)
    }

    fn detach_managed_child_surface(
        &mut self,
        _webview: WebViewHandle,
        _child_webview_id: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        Ok(Vec::new())
    }

    fn perform_managed_child_updates(
        &mut self,
        _webview: WebViewHandle,
        _child_webview_id: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        Ok(Vec::new())
    }

    fn dispatch_webview_command(
        &mut self,
        _webview: WebViewHandle,
        command: &WebViewCommand,
    ) -> Result<Vec<HostEvent>, HostError> {
        if matches!(command, WebViewCommand::StopLoading) {
            return Err(HostError::new(
                "native stop loading is unsupported by this host",
            ));
        }
        Ok(Vec::new())
    }

    /// Read current native system-WebView loading state; unavailable hosts return an error.
    fn system_webview_is_loading(&mut self, _webview: WebViewHandle) -> Result<bool, HostError> {
        Err(HostError::new(
            "system-WebView loading state is unsupported by this host",
        ))
    }

    fn resolve_navigation_request(
        &mut self,
        _webview: WebViewHandle,
        navigation_id: &str,
        _allow: bool,
    ) -> Result<Vec<HostEvent>, HostError> {
        Err(HostError::new(format!(
            "navigation request {navigation_id} cannot be resolved by this host"
        )))
    }

    /// Forward a retained Servo simple-dialog completion to this host.
    ///
    /// Provenance is `WebViewDelegate::show_embedder_control` with
    /// `EmbedderControl::SimpleDialog`. Completion uses the retained dialog's
    /// confirm/dismiss operations and optional prompt `set_current_value`; this
    /// method adds no capability or policy beyond that existing Servo path.
    fn resolve_simple_dialog(
        &mut self,
        _webview: WebViewHandle,
        dialog_id: &str,
        _confirmed: bool,
        _prompt_value: Option<&str>,
    ) -> Result<Vec<HostEvent>, HostError> {
        Err(HostError::new(format!(
            "simple dialog {dialog_id} cannot be resolved by this host"
        )))
    }

    /// Forward a retained Servo context-menu selection to this host.
    ///
    /// Provenance is `WebViewDelegate::show_embedder_control` with
    /// `EmbedderControl::ContextMenu`, completed through its retained `select`
    /// operation. This method adds no capability or policy beyond that existing
    /// Servo path.
    fn resolve_context_menu(
        &mut self,
        _webview: WebViewHandle,
        context_menu_id: &str,
        _action: ContextMenuAction,
    ) -> Result<Vec<HostEvent>, HostError> {
        Err(HostError::new(format!(
            "context menu {context_menu_id} cannot be resolved by this host"
        )))
    }

    /// Forward dismissal of a retained Servo context menu to this host.
    ///
    /// Provenance is `WebViewDelegate::show_embedder_control` with
    /// `EmbedderControl::ContextMenu`, completed through its retained `dismiss`
    /// operation. This method adds no capability or policy beyond that existing
    /// Servo path.
    fn dismiss_context_menu(
        &mut self,
        _webview: WebViewHandle,
        context_menu_id: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        Err(HostError::new(format!(
            "context menu {context_menu_id} cannot be dismissed by this host"
        )))
    }

    /// Forward ExplorerKit's JavaScript evaluation operation to the owning engine.
    ///
    /// Servo provenance is `WebView::evaluate_javascript` and its completion callback.
    /// The macOS system host uses its WRY-owned WKWebView's native
    /// `evaluateJavaScript:completionHandler:` in the main document's page world.
    /// Immediate host dispatch failure returns [`HostError`]; accepted evaluation
    /// success or script/serialization failure completes asynchronously as
    /// [`HostEvent::JavaScriptEvaluationResult`] with the caller's identifier.
    /// Engine-specific readiness, supported values and document/lifetime rules
    /// belong to the host; this method adds no policy to the existing Servo path.
    fn evaluate_javascript(
        &mut self,
        _webview: WebViewHandle,
        evaluation_id: &str,
        _script: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        Err(HostError::new(format!(
            "JavaScript evaluation {evaluation_id} is not supported by this host"
        )))
    }

    fn dispatch_input_event(
        &mut self,
        _webview: WebViewHandle,
        _event: &HostInputEvent,
    ) -> Result<Vec<HostEvent>, HostError> {
        Ok(Vec::new())
    }

    fn synthesize_webview_command_events(&self) -> bool {
        true
    }

    fn perform_updates(&mut self, _webview: WebViewHandle) -> Result<Vec<HostEvent>, HostError> {
        Ok(Vec::new())
    }

    fn observe_webview_events(
        &mut self,
        webview: WebViewHandle,
        events: &[HostEvent],
    ) -> Result<(), HostError>;

    fn observe_managed_child_webview_events(
        &mut self,
        webview: WebViewHandle,
        _child_webview_id: &str,
        events: &[HostEvent],
    ) -> Result<(), HostError> {
        self.observe_webview_events(webview, events)
    }
}

/// Host extension for view implementations that require create-time options.
///
/// The base [`Host`] contract and [`Runtime::create_webview`] stay unchanged for
/// existing hosts. Concrete hosts expose only their own option type through this
/// seam rather than a global engine or profile abstraction.
pub trait ConfigurableHost: Host {
    /// Create-time options understood by this concrete host.
    type WebViewOptions;

    /// Creates one view with host-specific options for the allocated handle.
    fn create_webview_with_options(
        &mut self,
        session: SessionHandle,
        webview: WebViewHandle,
        options: Self::WebViewOptions,
    ) -> Result<(), HostError>;
}

#[derive(Debug, Clone, PartialEq)]
struct AttachedSurface {
    surface: HostSurface,
    viewport: SurfaceViewport,
}

#[derive(Debug, Default)]
struct WebViewState {
    adapter: ServoAdapterState,
    surface: Option<AttachedSurface>,
    managed_child_surfaces: HashMap<String, AttachedSurface>,
    history: Vec<String>,
    history_index: Option<usize>,
    is_focused: bool,
}

impl WebViewState {
    fn current_history_url(&self) -> Option<&str> {
        self.history_index
            .and_then(|index| self.history.get(index))
            .map(String::as_str)
    }

    fn push_history_url(&mut self, url: String) {
        if let Some(index) = self.history_index {
            self.history.truncate(index + 1);
        }

        self.history.push(url);
        self.history_index = Some(self.history.len() - 1);
    }

    fn notify_history_changed(&mut self) {
        let Some(current) = self.history_index else {
            return;
        };

        self.adapter.notify_history_changed(
            self.history.clone(),
            current,
            current > 0,
            current + 1 < self.history.len(),
        );
    }
}

#[derive(Debug, Default)]
struct RuntimeRegistry {
    next_session_id: u64,
    next_webview_id: u64,
    sessions: HashSet<SessionHandle>,
    webviews: HashMap<WebViewHandle, RegisteredWebView>,
    journal: VecDeque<ExplorerKitEvent>,
    events: VecDeque<ExplorerKitEvent>,
}

#[derive(Debug)]
struct RegisteredWebView {
    session: SessionHandle,
    state: WebViewState,
    lineage: Option<ChildLineage>,
    reservation: Option<ReservedChild>,
    registers_children: bool,
}

#[derive(Debug)]
struct ChildLineage {
    source: WebViewHandle,
    originating_root: WebViewHandle,
    native_id: String,
}

#[derive(Debug, Default)]
struct ReservedChild {
    events: VecDeque<HostEvent>,
    retired: bool,
}

impl RegisteredWebView {
    fn new(session: SessionHandle) -> Self {
        Self {
            session,
            state: WebViewState::default(),
            lineage: None,
            reservation: None,
            registers_children: false,
        }
    }
}

impl RuntimeRegistry {
    fn allocate_webview(&mut self) -> Result<WebViewHandle, HostError> {
        self.next_webview_id = self
            .next_webview_id
            .checked_add(1)
            .ok_or_else(|| HostError::new("webview handle capacity exhausted"))?;
        Ok(WebViewHandle(self.next_webview_id))
    }

    fn live_view(&self, webview: WebViewHandle) -> Option<&RegisteredWebView> {
        self.webviews
            .get(&webview)
            .filter(|view| view.reservation.is_none())
    }

    fn drain_journal(&mut self) {
        self.events.append(&mut self.journal);
    }

    fn destroy_webview(&mut self, webview: WebViewHandle) {
        self.webviews.remove(&webview);
        for queue in [&mut self.events, &mut self.journal] {
            queue.retain(|event| event.webview != webview || event.created_webview.is_some());
        }
    }
}

/// Weak, capability-limited access to a Runtime's registration and event journal.
///
/// Native objects and application callbacks stay in the owning host. No registry
/// guard is held across a host, engine, presentation or application callback.
#[derive(Debug, Clone)]
pub struct WebViewRegistrar {
    registry: Weak<Mutex<RuntimeRegistry>>,
}

impl WebViewRegistrar {
    /// Reserves an ordinary handle in the live source's existing session.
    ///
    /// Reserve before accepting the native request. Until commit, the handle is
    /// not commandable and its callbacks are staged. Dropping the reservation
    /// rolls it back without reusing its handle.
    pub fn reserve_child(
        &self,
        source: WebViewHandle,
        request_id: String,
    ) -> Result<ChildWebViewRegistration, HostError> {
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| HostError::new("the child Runtime has been dropped"))?;
        let mut registry = registry.lock().expect("Runtime registry poisoned");
        let source_view = registry
            .live_view(source)
            .ok_or_else(|| HostError::new("the child source is not live"))?;
        let session = source_view.session;
        let originating_root = source_view
            .lineage
            .as_ref()
            .map_or(source, |lineage| lineage.originating_root);
        let webview = registry.allocate_webview()?;
        let mut view = RegisteredWebView::new(session);
        view.registers_children = true;
        view.reservation = Some(ReservedChild::default());
        view.lineage = Some(ChildLineage {
            source,
            originating_root,
            native_id: String::new(),
        });
        registry.webviews.insert(webview, view);
        registry
            .webviews
            .get_mut(&source)
            .unwrap()
            .registers_children = true;
        Ok(ChildWebViewRegistration {
            registrar: self.clone(),
            webview,
            request_id,
            committed: false,
        })
    }

    /// Records a host-processed event for an existing live or reserved identity.
    ///
    /// Late events for retired identities are discarded. Native close uses
    /// [`Self::retire_webview`] instead of separately recording `Closed`.
    pub fn record_event(&self, webview: WebViewHandle, event: HostEvent) {
        if matches!(event, HostEvent::Closed) {
            self.retire_webview(webview);
            return;
        }
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let mut registry = registry.lock().expect("Runtime registry poisoned");
        let Some(view) = registry.webviews.get_mut(&webview) else {
            return;
        };
        if let Some(reserved) = &mut view.reservation {
            if !reserved.retired {
                reserved.events.push_back(event);
            }
        } else {
            registry.journal.push_back(ExplorerKitEvent {
                webview,
                created_webview: None,
                managed_child_webview_id: None,
                event,
            });
        }
    }

    /// Retires one native document and records `Closed` exactly once.
    ///
    /// A pre-commit close is staged after its future creation receipt. Other
    /// documents and inert source lineage remain valid.
    pub fn retire_webview(&self, webview: WebViewHandle) {
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let mut registry = registry.lock().expect("Runtime registry poisoned");
        let Some(view) = registry.webviews.get_mut(&webview) else {
            return;
        };
        if let Some(reserved) = &mut view.reservation {
            if !reserved.retired {
                reserved.retired = true;
                reserved.events.push_back(HostEvent::Closed);
            }
        } else {
            registry.webviews.remove(&webview);
            registry.journal.push_back(ExplorerKitEvent {
                webview,
                created_webview: None,
                managed_child_webview_id: None,
                event: HostEvent::Closed,
            });
        }
    }
}

/// A prepared child handle whose native acceptance has not yet been committed.
#[derive(Debug)]
pub struct ChildWebViewRegistration {
    registrar: WebViewRegistrar,
    webview: WebViewHandle,
    request_id: String,
    committed: bool,
}

impl ChildWebViewRegistration {
    /// Returns the fixed identity to capture in the child's native delegates.
    pub fn webview(&self) -> WebViewHandle {
        self.webview
    }

    /// Commits the already accepted original native child without a fallible step.
    ///
    /// The creation receipt precedes staged child callbacks, including an early
    /// close. The native host must finish all recoverable preparation first.
    pub fn commit(self, created: PopupCreated) {
        self.commit_prepared(created, None);
    }

    /// Commits an original native child already attached to an app-owned surface.
    ///
    /// Records the initial surface without reentering the host. The creation
    /// receipt precedes `SurfaceAttached` and any staged native callbacks. Finish
    /// all recoverable native preparation before calling this method.
    pub fn commit_with_surface(
        self,
        created: PopupCreated,
        surface: HostSurface,
        viewport: SurfaceViewport,
    ) {
        self.commit_prepared(created, Some(AttachedSurface { surface, viewport }));
    }

    fn commit_prepared(mut self, created: PopupCreated, surface: Option<AttachedSurface>) {
        self.committed = true;
        let Some(registry) = self.registrar.registry.upgrade() else {
            return;
        };
        let mut registry = registry.lock().expect("Runtime registry poisoned");
        let view = registry
            .webviews
            .get_mut(&self.webview)
            .expect("prepared child remains reserved until commit");
        let reserved = view.reservation.take().expect("child commits once");
        let initial_size = surface.as_ref().map(|attached| attached.viewport.size);
        view.state.surface = surface;
        let lineage = view.lineage.as_mut().expect("child has source lineage");
        lineage.native_id = created.child_webview_id.clone();
        let source = lineage.source;
        registry.journal.push_back(ExplorerKitEvent {
            webview: source,
            created_webview: Some(self.webview),
            managed_child_webview_id: None,
            event: HostEvent::PopupCreated {
                request_id: self.request_id.clone(),
                parent_webview_id: created.parent_webview_id,
                child_webview_id: created.child_webview_id,
                parent_url: created.parent_url,
                target_url: created.target_url,
                window_features: created.window_features,
                wants_popup: created.wants_popup,
                policy: PopupRequestPolicy::ManagedChild,
            },
        });
        if let Some(size) = initial_size {
            registry.journal.push_back(ExplorerKitEvent {
                webview: self.webview,
                created_webview: None,
                managed_child_webview_id: None,
                event: HostEvent::SurfaceAttached { size },
            });
        }
        registry
            .journal
            .extend(reserved.events.into_iter().map(|event| ExplorerKitEvent {
                webview: self.webview,
                created_webview: None,
                managed_child_webview_id: None,
                event,
            }));
        if reserved.retired {
            registry.webviews.remove(&self.webview);
        }
    }
}

impl Drop for ChildWebViewRegistration {
    fn drop(&mut self) {
        if !self.committed {
            if let Some(registry) = self.registrar.registry.upgrade() {
                registry
                    .lock()
                    .expect("Runtime registry poisoned")
                    .webviews
                    .remove(&self.webview);
            }
        }
    }
}

#[derive(Debug)]
pub struct Runtime<H> {
    host: H,
    registry: Arc<Mutex<RuntimeRegistry>>,
}

impl<H: Host> Runtime<H> {
    pub fn new(mut host: H) -> Self {
        let registry = Arc::new(Mutex::new(RuntimeRegistry::default()));
        host.set_webview_registrar(WebViewRegistrar {
            registry: Arc::downgrade(&registry),
        });
        Self { host, registry }
    }

    pub fn create_session(&mut self) -> SessionHandle {
        let mut registry = self.registry.lock().expect("Runtime registry poisoned");
        registry.drain_journal();
        registry.next_session_id = registry
            .next_session_id
            .checked_add(1)
            .expect("session handle capacity exhausted");
        let session = SessionHandle(registry.next_session_id);
        registry.sessions.insert(session);
        session
    }

    pub fn create_webview(
        &mut self,
        session: SessionHandle,
    ) -> Result<WebViewHandle, RuntimeError> {
        let webview = self.register_root(session)?;
        if let Err(error) = self.call_host(|host| host.create_webview(session, webview)) {
            self.registry
                .lock()
                .expect("Runtime registry poisoned")
                .webviews
                .remove(&webview);
            return Err(error);
        }
        Ok(webview)
    }

    /// Creates one view through a concrete host's create-time option seam.
    ///
    /// Existing hosts continue to use [`Self::create_webview`]. The options are
    /// owned by the concrete host and do not imply a global engine selection API.
    pub fn create_webview_with_options(
        &mut self,
        session: SessionHandle,
        options: <H as ConfigurableHost>::WebViewOptions,
    ) -> Result<WebViewHandle, RuntimeError>
    where
        H: ConfigurableHost,
    {
        let webview = self.register_root(session)?;
        if let Err(error) =
            self.call_host(|host| host.create_webview_with_options(session, webview, options))
        {
            self.registry
                .lock()
                .expect("Runtime registry poisoned")
                .webviews
                .remove(&webview);
            return Err(error);
        }
        Ok(webview)
    }

    /// Closes one view and invalidates its handle without closing its siblings or session.
    ///
    /// The host detaches the renderer before dropping the underlying view. On success,
    /// queued ordinary events for this view are discarded; committed child-creation
    /// receipts survive. Later commands return `UnknownWebView`.
    pub fn destroy_webview(&mut self, webview: WebViewHandle) -> Result<(), RuntimeError> {
        self.webview_state(webview, |_| ())?;
        self.call_host(|host| host.destroy_webview(webview))?;
        self.registry
            .lock()
            .expect("Runtime registry poisoned")
            .destroy_webview(webview);
        Ok(())
    }

    /// Closes all views and performs final host shutdown, consuming this runtime.
    ///
    /// Call at application exit while the owning thread, native surfaces and logging
    /// remain alive. Servo cannot initialize again after final shutdown. Ordinary
    /// drop closes the views but retains the process engine for subsequent host creation.
    /// All views and the host receive a cleanup attempt even if one fails; the first
    /// error is returned after final cleanup.
    pub fn shutdown(mut self) -> Result<(), RuntimeError> {
        let views = self.live_views();
        let mut first_error = None;
        for view in views {
            if self.webview_state(view, |_| ()).is_err() {
                continue;
            }
            if let Err(error) = self.destroy_webview(view) {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = self.call_host(|host| host.shutdown()) {
            first_error.get_or_insert(error);
        }
        first_error.map_or(Ok(()), Err)
    }

    pub fn attach_surface(
        &mut self,
        webview: WebViewHandle,
        surface: HostSurface,
        size: SurfaceSize,
    ) -> Result<(), RuntimeError> {
        if self.webview_state(webview, |state| state.surface.is_some())? {
            return Err(RuntimeError::SurfaceAlreadyAttached(webview));
        }
        let host_events = self.call_host(|host| host.attach_surface(webview, &surface, size))?;
        let updated = self
            .update_webview(webview, |state| {
                state.surface = Some(AttachedSurface {
                    surface,
                    viewport: SurfaceViewport::new(size, 1.0),
                });
            })
            .is_some();
        let mut events = if updated {
            vec![HostEvent::SurfaceAttached { size }]
        } else {
            Vec::new()
        };
        events.extend(host_events);
        self.record_events(webview, events)
    }

    pub fn resize_surface(
        &mut self,
        webview: WebViewHandle,
        size: SurfaceSize,
    ) -> Result<(), RuntimeError> {
        self.surface_viewport(webview)?;
        let host_events = self.call_host(|host| host.resize_surface(webview, size))?;
        let updated = self
            .update_webview(webview, |state| {
                state
                    .surface
                    .as_mut()
                    .expect("surface presence was checked")
                    .viewport
                    .size = size;
            })
            .is_some();
        let mut events = if updated {
            vec![HostEvent::SurfaceResized { size }]
        } else {
            Vec::new()
        };
        events.extend(host_events);
        self.record_events(webview, events)
    }

    pub fn set_surface_scale_factor(
        &mut self,
        webview: WebViewHandle,
        scale_factor: f32,
    ) -> Result<(), RuntimeError> {
        self.surface_viewport(webview)?;
        let events = self.call_host(|host| host.set_surface_scale_factor(webview, scale_factor))?;
        self.update_webview(webview, |state| {
            state
                .surface
                .as_mut()
                .expect("surface presence was checked")
                .viewport
                .scale_factor = scale_factor;
        });
        self.record_events(webview, events)
    }

    pub fn attach_surface_with_viewport(
        &mut self,
        webview: WebViewHandle,
        surface: HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<(), RuntimeError> {
        if self.webview_state(webview, |state| state.surface.is_some())? {
            return Err(RuntimeError::SurfaceAlreadyAttached(webview));
        }
        let host_events =
            self.call_host(|host| host.attach_surface_with_viewport(webview, &surface, viewport))?;
        let updated = self
            .update_webview(webview, |state| {
                state.surface = Some(AttachedSurface { surface, viewport });
            })
            .is_some();
        let mut events = if updated {
            vec![HostEvent::SurfaceAttached {
                size: viewport.size,
            }]
        } else {
            Vec::new()
        };
        events.extend(host_events);
        self.record_events(webview, events)
    }

    pub fn update_surface_viewport(
        &mut self,
        webview: WebViewHandle,
        viewport: SurfaceViewport,
    ) -> Result<(), RuntimeError> {
        let previous_viewport = self.surface_viewport(webview)?;
        let host_events = self
            .call_host(|host| host.update_surface_viewport(webview, viewport, previous_viewport))?;
        let updated = self
            .update_webview(webview, |state| {
                state
                    .surface
                    .as_mut()
                    .expect("surface presence was checked")
                    .viewport = viewport;
            })
            .is_some();
        let mut events = Vec::new();
        if updated && previous_viewport.size != viewport.size {
            events.push(HostEvent::SurfaceResized {
                size: viewport.size,
            });
        }
        events.extend(host_events);
        self.record_events(webview, events)
    }

    pub fn detach_surface(&mut self, webview: WebViewHandle) -> Result<(), RuntimeError> {
        self.surface_viewport(webview)?;
        let host_events = self.call_host(|host| host.detach_surface(webview))?;
        let updated = self
            .update_webview(webview, |state| state.surface = None)
            .is_some();
        let mut events = if updated {
            vec![HostEvent::SurfaceDetached]
        } else {
            Vec::new()
        };
        events.extend(host_events);
        self.record_events(webview, events)
    }

    pub fn set_popup_policy(
        &mut self,
        webview: WebViewHandle,
        popup_policy: PopupRequestPolicy,
    ) -> Result<(), RuntimeError> {
        self.webview_state(webview, |_| ())?;
        let events = self.call_host(|host| host.set_popup_policy(webview, popup_policy))?;
        self.record_events(webview, events)
    }

    pub fn attach_managed_child_surface(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: impl Into<String>,
        surface: HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<(), RuntimeError> {
        let child_webview_id = child_webview_id.into();
        if let Some(child) = self.registered_child(webview, &child_webview_id)? {
            return self.attach_surface_with_viewport(child, surface, viewport);
        }
        if self.webview_state(webview, |state| {
            state.managed_child_surfaces.contains_key(&child_webview_id)
        })? {
            return Err(RuntimeError::ManagedChildSurfaceAlreadyAttached(
                webview,
                child_webview_id,
            ));
        }
        let host_events = self.call_host(|host| {
            host.attach_managed_child_surface(webview, &child_webview_id, &surface, viewport)
        })?;
        let updated = self
            .update_webview(webview, |state| {
                state.managed_child_surfaces.insert(
                    child_webview_id.clone(),
                    AttachedSurface { surface, viewport },
                );
            })
            .is_some();
        let mut events = if updated {
            vec![HostEvent::SurfaceAttached {
                size: viewport.size,
            }]
        } else {
            Vec::new()
        };
        events.extend(host_events);
        self.record_managed_child_events(webview, child_webview_id.to_owned(), events)
    }

    pub fn resize_managed_child_surface(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: impl AsRef<str>,
        size: SurfaceSize,
    ) -> Result<(), RuntimeError> {
        let child_webview_id = child_webview_id.as_ref();
        if let Some(child) = self.registered_child(webview, child_webview_id)? {
            return self.resize_surface(child, size);
        }
        self.managed_child_viewport(webview, child_webview_id)?;
        let host_events = self
            .call_host(|host| host.resize_managed_child_surface(webview, child_webview_id, size))?;
        let updated = self
            .update_webview(webview, |state| {
                state
                    .managed_child_surfaces
                    .get_mut(child_webview_id)
                    .expect("child surface presence was checked")
                    .viewport
                    .size = size;
            })
            .is_some();
        let mut events = if updated {
            vec![HostEvent::SurfaceResized { size }]
        } else {
            Vec::new()
        };
        events.extend(host_events);
        self.record_managed_child_events(webview, child_webview_id.to_owned(), events)
    }

    pub fn update_managed_child_surface_viewport(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: impl AsRef<str>,
        viewport: SurfaceViewport,
    ) -> Result<(), RuntimeError> {
        let child_webview_id = child_webview_id.as_ref();
        if let Some(child) = self.registered_child(webview, child_webview_id)? {
            return self.update_surface_viewport(child, viewport);
        }
        let previous_viewport = self.managed_child_viewport(webview, child_webview_id)?;
        let host_events = self.call_host(|host| {
            host.update_managed_child_surface_viewport(
                webview,
                child_webview_id,
                viewport,
                previous_viewport,
            )
        })?;
        let updated = self
            .update_webview(webview, |state| {
                state
                    .managed_child_surfaces
                    .get_mut(child_webview_id)
                    .expect("child surface presence was checked")
                    .viewport = viewport;
            })
            .is_some();
        let mut events = if updated {
            if previous_viewport.size != viewport.size {
                vec![HostEvent::SurfaceResized {
                    size: viewport.size,
                }]
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        events.extend(host_events);
        self.record_managed_child_events(webview, child_webview_id.to_owned(), events)
    }

    pub fn detach_managed_child_surface(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: impl AsRef<str>,
    ) -> Result<(), RuntimeError> {
        let child_webview_id = child_webview_id.as_ref();
        if let Some(child) = self.registered_child(webview, child_webview_id)? {
            return self.detach_surface(child);
        }
        self.managed_child_viewport(webview, child_webview_id)?;
        let host_events =
            self.call_host(|host| host.detach_managed_child_surface(webview, child_webview_id))?;
        let updated = self
            .update_webview(webview, |state| {
                state.managed_child_surfaces.remove(child_webview_id);
            })
            .is_some();
        let mut events = if updated {
            vec![HostEvent::SurfaceDetached]
        } else {
            Vec::new()
        };
        events.extend(host_events);
        self.record_managed_child_events(webview, child_webview_id.to_owned(), events)
    }

    pub fn perform_managed_child_updates(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: impl AsRef<str>,
    ) -> Result<(), RuntimeError> {
        let child_webview_id = child_webview_id.as_ref();
        if let Some(child) = self.registered_child(webview, child_webview_id)? {
            return self.perform_updates(child);
        }
        self.managed_child_viewport(webview, child_webview_id)?;
        let events =
            self.call_host(|host| host.perform_managed_child_updates(webview, child_webview_id))?;
        self.record_managed_child_events(webview, child_webview_id.to_owned(), events)
    }

    pub fn load_url(
        &mut self,
        webview: WebViewHandle,
        url: impl AsRef<str>,
    ) -> Result<(), RuntimeError> {
        self.registry
            .lock()
            .expect("Runtime registry poisoned")
            .drain_journal();
        let request = NavigationRequest::new(url.as_ref())?;
        self.dispatch_webview_command(webview, WebViewCommand::LoadUrl(request))
    }

    pub fn reload(&mut self, webview: WebViewHandle) -> Result<(), RuntimeError> {
        self.dispatch_webview_command(webview, WebViewCommand::Reload)
    }

    /// Submit the native stop-loading operation for a live view.
    ///
    /// Currently supported by native macOS system WebViews. Success means submission,
    /// not completed cancellation or successful navigation. Servo returns an error.
    /// The native committed document, history and view identity are retained.
    pub fn stop_loading(&mut self, webview: WebViewHandle) -> Result<(), RuntimeError> {
        self.dispatch_webview_command(webview, WebViewCommand::StopLoading)
    }

    /// Sample the current native system-WebView content-loading flag.
    ///
    /// Currently maps to macOS `WKWebView::isLoading` on the owning thread. False
    /// identifies neither a navigation outcome nor cancellation cause or network
    /// quiescence. Query after processing commands/events; do not reuse a sample
    /// across a later navigation. Unsupported or uninstantiated views return an error.
    pub fn system_webview_is_loading(
        &mut self,
        webview: WebViewHandle,
    ) -> Result<bool, RuntimeError> {
        self.webview_state(webview, |_| ())?;
        self.call_host(|host| host.system_webview_is_loading(webview))
    }

    pub fn go_back(&mut self, webview: WebViewHandle) -> Result<(), RuntimeError> {
        self.dispatch_webview_command(webview, WebViewCommand::GoBack)
    }

    pub fn go_forward(&mut self, webview: WebViewHandle) -> Result<(), RuntimeError> {
        self.dispatch_webview_command(webview, WebViewCommand::GoForward)
    }

    pub fn focus(&mut self, webview: WebViewHandle) -> Result<(), RuntimeError> {
        self.dispatch_webview_command(webview, WebViewCommand::Focus)
    }

    pub fn blur(&mut self, webview: WebViewHandle) -> Result<(), RuntimeError> {
        self.dispatch_webview_command(webview, WebViewCommand::Blur)
    }

    pub fn dispatch_webview_command(
        &mut self,
        webview: WebViewHandle,
        command: WebViewCommand,
    ) -> Result<(), RuntimeError> {
        self.webview_state(webview, |_| ())?;
        let synthesize_events = self.host.synthesize_webview_command_events();
        let mut events = self.call_host(|host| host.dispatch_webview_command(webview, &command))?;
        if synthesize_events {
            if let Some(synthesized) =
                self.update_webview(webview, |state| dispatch_embedder_command(state, command))
            {
                events.extend(synthesized);
            }
        }
        self.record_events(webview, events)
    }

    /// Resolve a Servo `WebViewDelegate::request_navigation` policy request.
    ///
    /// The `navigation_id` comes from ExplorerKit's `NavigationRequested` host event.
    /// `allow=true` calls Servo's retained request `allow`; `allow=false` calls `deny`.
    pub fn resolve_navigation_request(
        &mut self,
        webview: WebViewHandle,
        navigation_id: impl AsRef<str>,
        allow: bool,
    ) -> Result<(), RuntimeError> {
        self.webview_state(webview, |_| ())?;
        let navigation_id = navigation_id.as_ref();
        if navigation_id.trim().is_empty() {
            return Err(RuntimeError::Host(HostError::new(
                "navigation request id must not be empty",
            )));
        }

        let events =
            self.call_host(|host| host.resolve_navigation_request(webview, navigation_id, allow))?;
        self.record_events(webview, events)
    }

    /// Resolve a retained Servo simple-dialog request through [`Host`].
    ///
    /// Provenance is `WebViewDelegate::show_embedder_control` with
    /// `EmbedderControl::SimpleDialog`. Completion uses the retained dialog's
    /// confirm/dismiss operations and optional prompt `set_current_value`; this
    /// method adds no capability or policy beyond that existing Servo path.
    pub fn resolve_simple_dialog(
        &mut self,
        webview: WebViewHandle,
        dialog_id: impl AsRef<str>,
        confirmed: bool,
        prompt_value: Option<&str>,
    ) -> Result<(), RuntimeError> {
        self.webview_state(webview, |_| ())?;
        let dialog_id = dialog_id.as_ref();
        if dialog_id.trim().is_empty() {
            return Err(RuntimeError::Host(HostError::new(
                "simple dialog id must not be empty",
            )));
        }

        let events = self.call_host(|host| {
            host.resolve_simple_dialog(webview, dialog_id, confirmed, prompt_value)
        })?;
        self.record_events(webview, events)
    }

    /// Resolve a retained Servo context-menu request through [`Host`].
    ///
    /// Provenance is `WebViewDelegate::show_embedder_control` with
    /// `EmbedderControl::ContextMenu`, completed through its retained `select`
    /// operation. This method adds no capability or policy beyond that existing
    /// Servo path.
    pub fn resolve_context_menu(
        &mut self,
        webview: WebViewHandle,
        context_menu_id: impl AsRef<str>,
        action: ContextMenuAction,
    ) -> Result<(), RuntimeError> {
        self.webview_state(webview, |_| ())?;
        let context_menu_id = context_menu_id.as_ref();
        if context_menu_id.trim().is_empty() {
            return Err(RuntimeError::Host(HostError::new(
                "context menu id must not be empty",
            )));
        }

        let events =
            self.call_host(|host| host.resolve_context_menu(webview, context_menu_id, action))?;
        self.record_events(webview, events)
    }

    /// Dismiss a retained Servo context-menu request through [`Host`].
    ///
    /// Provenance is `WebViewDelegate::show_embedder_control` with
    /// `EmbedderControl::ContextMenu`, completed through its retained `dismiss`
    /// operation. This method adds no capability or policy beyond that existing
    /// Servo path.
    pub fn dismiss_context_menu(
        &mut self,
        webview: WebViewHandle,
        context_menu_id: impl AsRef<str>,
    ) -> Result<(), RuntimeError> {
        self.webview_state(webview, |_| ())?;
        let context_menu_id = context_menu_id.as_ref();
        if context_menu_id.trim().is_empty() {
            return Err(RuntimeError::Host(HostError::new(
                "context menu id must not be empty",
            )));
        }

        let events = self.call_host(|host| host.dismiss_context_menu(webview, context_menu_id))?;
        self.record_events(webview, events)
    }

    /// Evaluate JavaScript through the view's owning engine.
    ///
    /// The asynchronous result is emitted as `HostEvent::JavaScriptEvaluationResult` with the
    /// caller-provided evaluation identifier. This ExplorerKit-owned operation maps to
    /// Servo's `WebView::evaluate_javascript` on Servo views and native WKWebView
    /// evaluation on macOS system views.
    ///
    /// On macOS system views, scripts run synchronously in the current main document's
    /// page world; returned Promises are not awaited. Eligibility begins at the first
    /// observed main-document commit, including for a retained hidden/detached view.
    /// An ineligible live view queues `WebViewNotReady` without saving the script.
    /// Caller IDs must remain distinct while outstanding within each view. Duplicate
    /// IDs still pending in the system adapter fail immediately.
    ///
    /// System results use recursive string, boolean, finite-number, null, array and
    /// string-keyed-object tags. Native nil normalizes to null. JavaScript exceptions
    /// (including syntax errors) are `EvaluationFailure`; native unsupported-result
    /// errors and unsupported values are `SerializationError`. Observed commit
    /// retires pending work with `DocumentNotFound`; observed process termination
    /// retires it with `InternalError`. WebKit can report an unsupported-result
    /// error before reporting termination; already finalized results are not rewritten.
    ///
    /// A system result finalized before a commit remains a historical snapshot ordered
    /// before load-start. Hosts must preserve request provenance, advance their own
    /// per-view generation when processing lifecycle events, and recheck it when
    /// deferred work applies a result. URL equality cannot detect same-URL reloads.
    /// Results may complete out of request order, with no hard deadline. Caller timeout
    /// does not cancel native execution. Destruction discards queued results; callers
    /// cancel their own waiters and must not transfer them to replacement views.
    pub fn evaluate_javascript(
        &mut self,
        webview: WebViewHandle,
        evaluation_id: impl AsRef<str>,
        script: impl AsRef<str>,
    ) -> Result<(), RuntimeError> {
        self.webview_state(webview, |_| ())?;
        let evaluation_id = evaluation_id.as_ref();
        if evaluation_id.trim().is_empty() {
            return Err(RuntimeError::Host(HostError::new(
                "JavaScript evaluation id must not be empty",
            )));
        }
        let events = self
            .call_host(|host| host.evaluate_javascript(webview, evaluation_id, script.as_ref()))?;
        self.record_events(webview, events)
    }

    pub fn dispatch_input_event(
        &mut self,
        webview: WebViewHandle,
        event: HostInputEvent,
    ) -> Result<(), RuntimeError> {
        self.webview_state(webview, |_| ())?;
        let events = self.call_host(|host| host.dispatch_input_event(webview, &event))?;
        self.record_events(webview, events)
    }

    pub fn perform_updates(&mut self, webview: WebViewHandle) -> Result<(), RuntimeError> {
        self.webview_state(webview, |_| ())?;
        let events = self.call_host(|host| host.perform_updates(webview))?;
        self.record_events(webview, events)
    }

    /// Pumps all live views and records their events using their own handles.
    ///
    /// Shared Servo hosts spin the engine once, then present each attached view. A failed
    /// view does not prevent siblings from updating. Their events remain available even
    /// when this method returns the first view-scoped error. Host-wide errors have no
    /// view handle and are returned as `RuntimeError::Host`.
    pub fn perform_all_updates(&mut self) -> Result<(), RuntimeError> {
        let views = self.live_views();
        let results = self.call_host(|host| host.perform_all_updates(&views))?;
        let mut first_error = None;
        for (webview, result) in results {
            let result = match result {
                Ok(events) => self.record_events(webview, events),
                Err(error) => Err(RuntimeError::WebView { webview, error }),
            };
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub fn drain_events(&mut self) -> Vec<ExplorerKitEvent> {
        let mut registry = self.registry.lock().expect("Runtime registry poisoned");
        registry.drain_journal();
        registry.events.drain(..).collect()
    }

    fn webview_state<R>(
        &mut self,
        webview: WebViewHandle,
        read: impl FnOnce(&WebViewState) -> R,
    ) -> Result<R, RuntimeError> {
        let mut registry = self.registry.lock().expect("Runtime registry poisoned");
        registry.drain_journal();
        registry
            .live_view(webview)
            .map(|view| read(&view.state))
            .ok_or(RuntimeError::UnknownWebView(webview))
    }

    fn record_events(
        &mut self,
        webview: WebViewHandle,
        events: Vec<HostEvent>,
    ) -> Result<(), RuntimeError> {
        self.record_events_with_target(webview, None, events)
    }

    fn record_managed_child_events(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: String,
        events: Vec<HostEvent>,
    ) -> Result<(), RuntimeError> {
        self.record_events_with_target(webview, Some(child_webview_id), events)
    }

    fn record_events_with_target(
        &mut self,
        webview: WebViewHandle,
        managed_child_webview_id: Option<String>,
        events: Vec<HostEvent>,
    ) -> Result<(), RuntimeError> {
        if events.is_empty() {
            return Ok(());
        }
        {
            let mut registry = self.registry.lock().expect("Runtime registry poisoned");
            registry.drain_journal();
            for event in &events {
                if managed_child_webview_id.is_none() && matches!(event, HostEvent::Closed) {
                    registry.webviews.remove(&webview);
                }
                registry.events.push_back(ExplorerKitEvent {
                    webview,
                    created_webview: None,
                    managed_child_webview_id: managed_child_webview_id.clone(),
                    event: event.clone(),
                });
            }
        }
        self.call_host(|host| {
            if let Some(child_webview_id) = managed_child_webview_id.as_deref() {
                host.observe_managed_child_webview_events(webview, child_webview_id, &events)
            } else {
                host.observe_webview_events(webview, &events)
            }
        })
    }

    /// Installs this view's immediate native child-creation decision handler.
    ///
    /// No handler denies. A created child copies the source's current handler and
    /// then owns it independently. The handler runs synchronously on the native
    /// owning thread and must not reenter this Runtime or present application UI.
    #[allow(
        clippy::type_complexity,
        reason = "Keep the approved callback signature explicit."
    )]
    pub fn set_new_webview_handler(
        &mut self,
        webview: WebViewHandle,
        handler: Option<Rc<dyn Fn(&NewWebViewRequest) -> NewWebViewDecision>>,
    ) -> Result<(), RuntimeError> {
        self.webview_state(webview, |_| ())?;
        self.call_host(|host| host.set_new_webview_handler(webview, handler))?;
        if let Some(view) = self
            .registry
            .lock()
            .expect("Runtime registry poisoned")
            .webviews
            .get_mut(&webview)
        {
            view.registers_children = true;
        }
        Ok(())
    }

    fn call_host<R>(
        &mut self,
        call: impl FnOnce(&mut H) -> Result<R, HostError>,
    ) -> Result<R, RuntimeError> {
        self.registry
            .lock()
            .expect("Runtime registry poisoned")
            .drain_journal();
        let result = call(&mut self.host);
        self.registry
            .lock()
            .expect("Runtime registry poisoned")
            .drain_journal();
        result.map_err(RuntimeError::Host)
    }

    fn register_root(&mut self, session: SessionHandle) -> Result<WebViewHandle, RuntimeError> {
        let mut registry = self.registry.lock().expect("Runtime registry poisoned");
        registry.drain_journal();
        if !registry.sessions.contains(&session) {
            return Err(RuntimeError::UnknownSession(session));
        }
        let webview = registry.allocate_webview()?;
        registry
            .webviews
            .insert(webview, RegisteredWebView::new(session));
        Ok(webview)
    }

    fn update_webview<R>(
        &mut self,
        webview: WebViewHandle,
        update: impl FnOnce(&mut WebViewState) -> R,
    ) -> Option<R> {
        self.registry
            .lock()
            .expect("Runtime registry poisoned")
            .webviews
            .get_mut(&webview)
            .filter(|view| view.reservation.is_none())
            .map(|view| update(&mut view.state))
    }

    fn live_views(&mut self) -> Vec<WebViewHandle> {
        let mut registry = self.registry.lock().expect("Runtime registry poisoned");
        registry.drain_journal();
        let mut views: Vec<_> = registry
            .webviews
            .iter()
            .filter(|(_, view)| view.reservation.is_none())
            .map(|(&handle, _)| handle)
            .collect();
        views.sort_by_key(|view| view.raw());
        views
    }

    fn surface_viewport(
        &mut self,
        webview: WebViewHandle,
    ) -> Result<SurfaceViewport, RuntimeError> {
        self.webview_state(webview, |state| {
            state.surface.as_ref().map(|surface| surface.viewport)
        })?
        .ok_or(RuntimeError::SurfaceNotAttached(webview))
    }

    fn managed_child_viewport(
        &mut self,
        webview: WebViewHandle,
        child: &str,
    ) -> Result<SurfaceViewport, RuntimeError> {
        self.webview_state(webview, |state| {
            state
                .managed_child_surfaces
                .get(child)
                .map(|surface| surface.viewport)
        })?
        .ok_or_else(|| RuntimeError::ManagedChildSurfaceNotAttached(webview, child.to_owned()))
    }

    fn registered_child(
        &mut self,
        root: WebViewHandle,
        native_id: &str,
    ) -> Result<Option<WebViewHandle>, RuntimeError> {
        let mut registry = self.registry.lock().expect("Runtime registry poisoned");
        registry.drain_journal();
        let child = registry.webviews.iter().find_map(|(&handle, view)| {
            view.lineage
                .as_ref()
                .filter(|lineage| {
                    view.reservation.is_none()
                        && lineage.originating_root == root
                        && lineage.native_id == native_id
                })
                .map(|_| handle)
        });
        if child.is_some() {
            return Ok(child);
        }
        if registry
            .live_view(root)
            .is_some_and(|view| !view.registers_children)
        {
            return Ok(None);
        }
        Err(HostError::new(format!(
            "registered child {native_id} for webview {} is absent",
            root.raw()
        ))
        .into())
    }
}

fn dispatch_embedder_command(state: &mut WebViewState, command: WebViewCommand) -> Vec<HostEvent> {
    match command {
        WebViewCommand::LoadUrl(request) => {
            let url = request.url;
            state.push_history_url(url.clone());
            emit_navigation_events(state, &url, true);
        }
        WebViewCommand::Reload => {
            if state.current_history_url().is_some() {
                state
                    .adapter
                    .notify_load_status_changed(LoadStatusKind::Started);
                state
                    .adapter
                    .notify_load_status_changed(LoadStatusKind::Complete);
            }
        }
        WebViewCommand::StopLoading => {}
        WebViewCommand::GoBack => {
            if let Some(current) = state.history_index.filter(|current| *current > 0) {
                let target = current - 1;
                state.history_index = Some(target);
                let url = state.history[target].clone();
                emit_navigation_events(state, &url, true);
            }
        }
        WebViewCommand::GoForward => {
            if let Some(current) = state
                .history_index
                .filter(|current| current + 1 < state.history.len())
            {
                let target = current + 1;
                state.history_index = Some(target);
                let url = state.history[target].clone();
                emit_navigation_events(state, &url, true);
            }
        }
        WebViewCommand::Focus => {
            state.is_focused = true;
            state.adapter.notify_focus_changed(true);
        }
        WebViewCommand::Blur => {
            state.is_focused = false;
            state.adapter.notify_focus_changed(false);
        }
    }

    state.adapter.drain_events()
}

fn emit_navigation_events(state: &mut WebViewState, url: &str, include_history: bool) {
    state
        .adapter
        .notify_load_status_changed(LoadStatusKind::Started);
    state.adapter.notify_url_changed(url);
    if include_history {
        state.notify_history_changed();
    }
    state
        .adapter
        .notify_load_status_changed(LoadStatusKind::Complete);
}

#[derive(Debug, Default)]
pub struct PlaceholderHost {
    created_webviews: HashMap<WebViewHandle, SessionHandle>,
    attached_surfaces: HashMap<WebViewHandle, AttachedSurface>,
    managed_child_surfaces: HashMap<(WebViewHandle, String), AttachedSurface>,
    popup_policies: HashMap<WebViewHandle, PopupRequestPolicy>,
    calls: Vec<HostCall>,
    observed_events: Vec<ExplorerKitEvent>,
    queued_update_events: VecDeque<HostEvent>,
    queued_managed_child_update_events: HashMap<(WebViewHandle, String), VecDeque<HostEvent>>,
}

pub type MockHost = PlaceholderHost;

impl PlaceholderHost {
    pub fn with_update_events(events: impl IntoIterator<Item = HostEvent>) -> Self {
        Self {
            queued_update_events: events.into_iter().collect(),
            ..Self::default()
        }
    }

    pub fn calls(&self) -> &[HostCall] {
        &self.calls
    }

    pub fn observed_events(&self) -> &[ExplorerKitEvent] {
        &self.observed_events
    }

    pub fn queue_update_event(&mut self, event: HostEvent) {
        self.queued_update_events.push_back(event);
    }

    pub fn queue_managed_child_update_event(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: impl Into<String>,
        event: HostEvent,
    ) {
        self.queued_managed_child_update_events
            .entry((webview, child_webview_id.into()))
            .or_default()
            .push_back(event);
    }

    fn ensure_created_webview(&self, webview: WebViewHandle) -> Result<(), HostError> {
        self.created_webviews
            .contains_key(&webview)
            .then_some(())
            .ok_or_else(|| HostError::new(format!("webview {} is not registered", webview.raw())))
    }

    fn ensure_managed_child_surface(
        &self,
        webview: WebViewHandle,
        child_webview_id: &str,
    ) -> Result<(), HostError> {
        self.ensure_created_webview(webview)?;
        self.managed_child_surfaces
            .contains_key(&(webview, child_webview_id.to_owned()))
            .then_some(())
            .ok_or_else(|| {
                HostError::new(format!(
                    "managed child webview {child_webview_id} for webview {} does not have an attached surface",
                    webview.raw()
                ))
            })
    }
}

impl Host for PlaceholderHost {
    fn destroy_webview(&mut self, webview: WebViewHandle) -> Result<(), HostError> {
        self.ensure_created_webview(webview)?;
        self.created_webviews.remove(&webview);
        self.attached_surfaces.remove(&webview);
        self.managed_child_surfaces
            .retain(|(owner, _), _| *owner != webview);
        self.queued_managed_child_update_events
            .retain(|(owner, _), _| *owner != webview);
        self.calls.push(HostCall::DestroyWebView { webview });
        Ok(())
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
        self.ensure_created_webview(webview)?;
        if self.attached_surfaces.contains_key(&webview) {
            return Err(HostError::new(format!(
                "webview {} already has an attached surface",
                webview.raw()
            )));
        }

        self.attached_surfaces.insert(
            webview,
            AttachedSurface {
                surface: surface.clone(),
                viewport: SurfaceViewport::new(size, 1.0),
            },
        );
        self.calls.push(HostCall::AttachSurface {
            webview,
            surface: surface.clone(),
            size,
        });
        Ok(Vec::new())
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
        Ok(Vec::new())
    }

    fn resize_surface(
        &mut self,
        webview: WebViewHandle,
        size: SurfaceSize,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        let Some(surface) = self.attached_surfaces.get_mut(&webview) else {
            return Err(HostError::new(format!(
                "webview {} does not have an attached surface",
                webview.raw()
            )));
        };

        surface.viewport.size = size;
        self.calls.push(HostCall::ResizeSurface { webview, size });
        Ok(Vec::new())
    }

    fn set_surface_scale_factor(
        &mut self,
        webview: WebViewHandle,
        scale_factor: f32,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        let Some(surface) = self.attached_surfaces.get_mut(&webview) else {
            return Err(HostError::new(format!(
                "webview {} does not have an attached surface",
                webview.raw()
            )));
        };

        surface.viewport.scale_factor = scale_factor;
        self.calls.push(HostCall::SetSurfaceScaleFactor {
            webview,
            scale_factor,
        });
        Ok(Vec::new())
    }

    fn update_surface_viewport(
        &mut self,
        webview: WebViewHandle,
        viewport: SurfaceViewport,
        _previous_viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        let Some(surface) = self.attached_surfaces.get_mut(&webview) else {
            return Err(HostError::new(format!(
                "webview {} does not have an attached surface",
                webview.raw()
            )));
        };

        surface.viewport = viewport;
        self.calls
            .push(HostCall::UpdateSurfaceViewport { webview, viewport });
        Ok(Vec::new())
    }

    fn detach_surface(&mut self, webview: WebViewHandle) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        if self.attached_surfaces.remove(&webview).is_none() {
            return Err(HostError::new(format!(
                "webview {} does not have an attached surface",
                webview.raw()
            )));
        }

        self.calls.push(HostCall::DetachSurface { webview });
        Ok(Vec::new())
    }

    fn set_popup_policy(
        &mut self,
        webview: WebViewHandle,
        popup_policy: PopupRequestPolicy,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        self.popup_policies.insert(webview, popup_policy);
        self.calls.push(HostCall::SetPopupPolicy {
            webview,
            popup_policy,
        });
        Ok(Vec::new())
    }

    fn attach_managed_child_surface(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
        surface: &HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        let key = (webview, child_webview_id.to_owned());
        if self.managed_child_surfaces.contains_key(&key) {
            return Err(HostError::new(format!(
                "managed child webview {child_webview_id} for webview {} already has an attached surface",
                webview.raw()
            )));
        }

        self.managed_child_surfaces.insert(
            key,
            AttachedSurface {
                surface: surface.clone(),
                viewport,
            },
        );
        self.calls.push(HostCall::AttachManagedChildSurface {
            webview,
            child_webview_id: child_webview_id.to_owned(),
            surface: surface.clone(),
            viewport,
        });
        Ok(Vec::new())
    }

    fn resize_managed_child_surface(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
        size: SurfaceSize,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_managed_child_surface(webview, child_webview_id)?;
        self.managed_child_surfaces
            .get_mut(&(webview, child_webview_id.to_owned()))
            .expect("managed child surface presence was checked")
            .viewport
            .size = size;
        self.calls.push(HostCall::ResizeManagedChildSurface {
            webview,
            child_webview_id: child_webview_id.to_owned(),
            size,
        });
        Ok(Vec::new())
    }

    fn update_managed_child_surface_viewport(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
        viewport: SurfaceViewport,
        _previous_viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_managed_child_surface(webview, child_webview_id)?;
        self.managed_child_surfaces
            .get_mut(&(webview, child_webview_id.to_owned()))
            .expect("managed child surface presence was checked")
            .viewport = viewport;
        self.calls
            .push(HostCall::UpdateManagedChildSurfaceViewport {
                webview,
                child_webview_id: child_webview_id.to_owned(),
                viewport,
            });
        Ok(Vec::new())
    }

    fn detach_managed_child_surface(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_managed_child_surface(webview, child_webview_id)?;
        self.managed_child_surfaces
            .remove(&(webview, child_webview_id.to_owned()));
        self.calls.push(HostCall::DetachManagedChildSurface {
            webview,
            child_webview_id: child_webview_id.to_owned(),
        });
        Ok(Vec::new())
    }

    fn perform_managed_child_updates(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_managed_child_surface(webview, child_webview_id)?;
        self.calls.push(HostCall::PerformManagedChildUpdates {
            webview,
            child_webview_id: child_webview_id.to_owned(),
        });
        Ok(self
            .queued_managed_child_update_events
            .entry((webview, child_webview_id.to_owned()))
            .or_default()
            .drain(..)
            .collect())
    }

    fn dispatch_webview_command(
        &mut self,
        webview: WebViewHandle,
        command: &WebViewCommand,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        self.calls.push(HostCall::DispatchWebViewCommand {
            webview,
            command: command.clone(),
        });
        Ok(Vec::new())
    }

    fn perform_updates(&mut self, webview: WebViewHandle) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        self.calls.push(HostCall::PerformUpdates { webview });
        Ok(self.queued_update_events.drain(..).collect())
    }

    fn resolve_navigation_request(
        &mut self,
        webview: WebViewHandle,
        navigation_id: &str,
        allow: bool,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.ensure_created_webview(webview)?;
        self.calls.push(HostCall::ResolveNavigationRequest {
            webview,
            navigation_id: navigation_id.to_owned(),
            allow,
        });
        Ok(Vec::new())
    }

    fn observe_webview_events(
        &mut self,
        webview: WebViewHandle,
        events: &[HostEvent],
    ) -> Result<(), HostError> {
        self.ensure_created_webview(webview)?;
        self.observed_events
            .extend(events.iter().cloned().map(|event| ExplorerKitEvent {
                webview,
                created_webview: None,
                managed_child_webview_id: None,
                event,
            }));
        Ok(())
    }

    fn observe_managed_child_webview_events(
        &mut self,
        webview: WebViewHandle,
        child_webview_id: &str,
        events: &[HostEvent],
    ) -> Result<(), HostError> {
        self.ensure_created_webview(webview)?;
        self.observed_events
            .extend(events.iter().cloned().map(|event| ExplorerKitEvent {
                webview,
                created_webview: None,
                managed_child_webview_id: Some(child_webview_id.to_owned()),
                event,
            }));
        Ok(())
    }
}

#[cfg(test)]
impl<H> Runtime<H> {
    fn host(&self) -> &H {
        &self.host
    }

    fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        KeyboardInputEvent, KeyboardInputKey, KeyboardInputState, PointerInputEvent,
        PopupRequestPolicy,
    };

    type JournalUpdate = Box<dyn FnOnce(&WebViewRegistrar) -> Result<Vec<HostEvent>, HostError>>;

    #[derive(Default)]
    struct JournalHost {
        registrar: Option<WebViewRegistrar>,
        update: Option<JournalUpdate>,
        commands: Vec<WebViewHandle>,
        observed: usize,
        fail_observer: bool,
        fail_destroy: bool,
        supports_children: bool,
    }

    impl Host for JournalHost {
        fn set_webview_registrar(&mut self, registrar: WebViewRegistrar) {
            self.registrar = Some(registrar);
        }
        fn create_webview(&mut self, _: SessionHandle, _: WebViewHandle) -> Result<(), HostError> {
            Ok(())
        }
        fn set_new_webview_handler(
            &mut self,
            _: WebViewHandle,
            _: Option<Rc<dyn Fn(&NewWebViewRequest) -> NewWebViewDecision>>,
        ) -> Result<(), HostError> {
            if self.supports_children {
                Ok(())
            } else {
                Err(HostError::new("native child creation is unsupported"))
            }
        }
        fn destroy_webview(&mut self, _: WebViewHandle) -> Result<(), HostError> {
            if self.fail_destroy {
                Err(HostError::new("outstanding frame"))
            } else {
                Ok(())
            }
        }
        fn dispatch_webview_command(
            &mut self,
            webview: WebViewHandle,
            _: &WebViewCommand,
        ) -> Result<Vec<HostEvent>, HostError> {
            self.commands.push(webview);
            Ok(Vec::new())
        }
        fn synthesize_webview_command_events(&self) -> bool {
            false
        }
        fn perform_updates(&mut self, _: WebViewHandle) -> Result<Vec<HostEvent>, HostError> {
            match self.update.take() {
                Some(update) => update(self.registrar.as_ref().unwrap()),
                None => Ok(Vec::new()),
            }
        }
        fn observe_webview_events(
            &mut self,
            _: WebViewHandle,
            _: &[HostEvent],
        ) -> Result<(), HostError> {
            self.observed += 1;
            if self.fail_observer {
                Err(HostError::new("observer failed"))
            } else {
                Ok(())
            }
        }
    }

    fn creation(request_id: &str, child_id: &str) -> PopupCreated {
        PopupCreated {
            request_id: request_id.into(),
            parent_webview_id: "native-source".into(),
            child_webview_id: child_id.into(),
            parent_url: None,
            target_url: None,
            window_features: None,
            wants_popup: None,
        }
    }

    #[test]
    fn registered_child_receipt_preserves_engine_popup_preference() {
        let mut runtime = Runtime::new(JournalHost::default());
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let registrar = runtime.host().registrar.clone().unwrap();
        runtime.drain_events();
        for wants_popup in [Some(true), Some(false), None] {
            let reservation = registrar.reserve_child(source, "attempt".into()).unwrap();
            let mut metadata = creation("attempt", "native-child");
            metadata.wants_popup = wants_popup;
            reservation.commit(metadata);
            assert!(runtime.drain_events().iter().any(|event| matches!(
                event.event, HostEvent::PopupCreated { wants_popup: actual, .. } if actual == wants_popup
            )));
        }
    }

    #[test]
    fn child_commit_precedes_staged_callbacks_and_close_without_resurrecting_either_view() {
        let mut runtime = Runtime::new(JournalHost::default());
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let registrar = runtime.host().registrar.clone().unwrap();
        let reservation = registrar.reserve_child(source, "attempt-1".into()).unwrap();
        let child = reservation.webview();
        assert_eq!(
            runtime.reload(child),
            Err(RuntimeError::UnknownWebView(child))
        );
        let title = HostEvent::PageTitleChanged {
            title: Some("original child".into()),
        };
        registrar.record_event(child, title.clone());
        registrar.retire_webview(child);
        registrar.retire_webview(child);
        registrar.record_event(child, HostEvent::UrlChanged { url: "late".into() });
        registrar.retire_webview(source);
        reservation.commit(creation("attempt-1", "native-child"));
        let events = runtime.drain_events();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0], event(source, HostEvent::Closed));
        assert_eq!(events[1].webview, source);
        assert_eq!(events[1].created_webview, Some(child));
        assert!(
            matches!(&events[1].event, HostEvent::PopupCreated { request_id, .. } if request_id == "attempt-1")
        );
        assert_eq!(events[2], event(child, title));
        assert_eq!(events[3], event(child, HostEvent::Closed));
        assert_eq!(
            runtime.reload(child),
            Err(RuntimeError::UnknownWebView(child))
        );
        assert_eq!(
            runtime.reload(source),
            Err(RuntimeError::UnknownWebView(source))
        );
        assert_eq!(
            runtime.host().observed,
            0,
            "journal records bypass legacy observation"
        );
        assert!(runtime.host().commands.is_empty());
    }

    #[test]
    fn journal_child_and_sibling_events_survive_host_failure_and_source_destruction() {
        use std::cell::Cell;
        let mut runtime = Runtime::new(JournalHost::default());
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let sibling = runtime.create_webview(session).unwrap();
        let child_sink = Rc::new(Cell::new(None));
        let child_result = child_sink.clone();
        runtime.host_mut().update = Some(Box::new(move |registrar| {
            let child = registrar.reserve_child(source, "attempt-2".into())?;
            let handle = child.webview();
            registrar.record_event(
                handle,
                HostEvent::UrlChanged {
                    url: "https://child.test/".into(),
                },
            );
            child.commit(creation("attempt-2", "native-child-2"));
            child_result.set(Some(handle));
            registrar.record_event(
                sibling,
                HostEvent::PageTitleChanged {
                    title: Some("sibling".into()),
                },
            );
            Err(HostError::new("presentation failed after creation"))
        }));
        assert!(matches!(
            runtime.perform_updates(source),
            Err(RuntimeError::Host(_))
        ));
        let child = child_sink.get().unwrap();
        runtime.destroy_webview(source).unwrap();
        runtime.reload(child).unwrap();
        runtime.reload(sibling).unwrap();
        let events = runtime.drain_events();
        assert_eq!(events.len(), 3);
        assert_eq!(
            (events[0].webview, events[0].created_webview),
            (source, Some(child))
        );
        assert_eq!(events[1].webview, child);
        assert_eq!(events[2].webview, sibling);
        assert_eq!(runtime.host().commands, vec![child, sibling]);
        assert_eq!(runtime.host().observed, 0);
    }

    #[test]
    fn reservation_rollback_never_reuses_a_handle_or_retains_the_runtime() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Runtime<PlaceholderHost>>();
        assert_send_sync::<WebViewRegistrar>();
        let mut runtime = Runtime::new(JournalHost::default());
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let registrar = runtime.host().registrar.clone().unwrap();
        let abandoned = registrar.reserve_child(source, "abandoned".into()).unwrap();
        let old = abandoned.webview();
        registrar.record_event(
            old,
            HostEvent::PageTitleChanged {
                title: Some("never published".into()),
            },
        );
        drop(abandoned);
        let reserved = registrar
            .reserve_child(source, "replacement".into())
            .unwrap();
        let replacement = reserved.webview();
        assert_ne!(old, replacement);
        assert_eq!(runtime.reload(old), Err(RuntimeError::UnknownWebView(old)));
        assert_eq!(
            runtime.reload(replacement),
            Err(RuntimeError::UnknownWebView(replacement))
        );
        assert!(runtime.drain_events().is_empty());
        assert_eq!(
            runtime.registry.lock().unwrap().webviews[&replacement].session,
            session
        );
        drop(runtime);
        assert!(registrar.reserve_child(source, "too late".into()).is_err());
        drop(reserved);
    }

    #[test]
    fn native_child_initial_surface_is_live_before_receipts_and_detaches_normally() {
        let mut runtime = Runtime::new(JournalHost::default());
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let registrar = runtime.host().registrar.clone().unwrap();
        let reserved = registrar.reserve_child(source, "attached".into()).unwrap();
        let child = reserved.webview();
        let viewport = SurfaceViewport::new(SurfaceSize::new(120, 90), 2.0);
        registrar.record_event(
            child,
            HostEvent::PageTitleChanged {
                title: Some("ready".into()),
            },
        );
        reserved.commit_with_surface(
            creation("attached", "native-attached"),
            HostSurface::new("initial-slot"),
            viewport,
        );

        assert_eq!(runtime.surface_viewport(child).unwrap(), viewport);
        assert_eq!(
            runtime.attach_surface(child, HostSurface::new("duplicate"), viewport.size),
            Err(RuntimeError::SurfaceAlreadyAttached(child))
        );
        let events = runtime.drain_events();
        assert_eq!(events[0].created_webview, Some(child));
        assert!(
            matches!(events[1].event, HostEvent::SurfaceAttached { size } if size == viewport.size)
        );
        assert!(matches!(
            events[2].event,
            HostEvent::PageTitleChanged { .. }
        ));
        assert!(events[1..].iter().all(|event| event.webview == child));
        runtime.destroy_webview(source).unwrap();
        runtime.detach_surface(child).unwrap();
        assert_eq!(
            runtime.detach_surface(child),
            Err(RuntimeError::SurfaceNotAttached(child))
        );
        runtime
            .attach_surface(child, HostSurface::new("replacement"), viewport.size)
            .unwrap();
        runtime.destroy_webview(child).unwrap();
    }

    #[test]
    fn initially_attached_child_early_close_keeps_ordered_receipts() {
        let mut runtime = Runtime::new(JournalHost::default());
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let registrar = runtime.host().registrar.clone().unwrap();
        let reserved = registrar.reserve_child(source, "closed".into()).unwrap();
        let child = reserved.webview();
        registrar.retire_webview(child);
        let viewport = SurfaceViewport::new(SurfaceSize::new(120, 90), 1.0);
        reserved.commit_with_surface(
            creation("closed", "native-closed"),
            HostSurface::new("slot"),
            viewport,
        );
        assert_eq!(
            runtime.reload(child),
            Err(RuntimeError::UnknownWebView(child))
        );
        let events = runtime.drain_events();
        assert_eq!(events[0].created_webview, Some(child));
        assert!(matches!(events[1].event, HostEvent::SurfaceAttached { .. }));
        assert!(matches!(events[2].event, HostEvent::Closed));
    }

    #[test]
    fn retained_legacy_alias_reuses_child_surface_after_source_closes() {
        let mut runtime = Runtime::new(JournalHost::default());
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let registrar = runtime.host().registrar.clone().unwrap();
        let reserved = registrar
            .reserve_child(source, "surface-child".into())
            .unwrap();
        let child = reserved.webview();
        reserved.commit(creation("surface-child", "native-surface-child"));
        runtime.destroy_webview(source).unwrap();
        let viewport = SurfaceViewport::new(SurfaceSize::new(120, 90), 2.0);
        runtime
            .attach_managed_child_surface(
                source,
                "native-surface-child",
                HostSurface::new("canvas-slot"),
                viewport,
            )
            .unwrap();
        assert_eq!(
            runtime.attach_surface(child, HostSurface::new("second-slot"), viewport.size),
            Err(RuntimeError::SurfaceAlreadyAttached(child))
        );
        runtime
            .resize_managed_child_surface(
                source,
                "native-surface-child",
                SurfaceSize::new(150, 100),
            )
            .unwrap();
        runtime.detach_surface(child).unwrap();
        assert_eq!(
            runtime.detach_managed_child_surface(source, "native-surface-child"),
            Err(RuntimeError::SurfaceNotAttached(child))
        );
        let events = runtime.drain_events();
        assert!(events
            .iter()
            .skip(1)
            .all(|event| event.webview == child && event.managed_child_webview_id.is_none()));
        runtime.destroy_webview(child).unwrap();
        assert!(matches!(
            runtime.perform_managed_child_updates(source, "native-surface-child"),
            Err(RuntimeError::Host(_))
        ));
    }

    #[test]
    fn registered_routes_reject_missing_legacy_aliases_before_their_first_request() {
        let mut runtime = Runtime::new(JournalHost {
            supports_children: true,
            ..JournalHost::default()
        });
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        runtime.set_new_webview_handler(source, None).unwrap();
        assert!(matches!(
            runtime.attach_managed_child_surface(
                source,
                "absent-child",
                HostSurface::new("slot"),
                SurfaceViewport::new(SurfaceSize::new(100, 100), 1.0),
            ),
            Err(RuntimeError::Host(_))
        ));
        let registrar = runtime.host().registrar.clone().unwrap();
        let reserved = registrar.reserve_child(source, "child".into()).unwrap();
        let child = reserved.webview();
        reserved.commit(creation("child", "native-child"));
        for view in [source, child] {
            assert!(matches!(
                runtime.attach_managed_child_surface(
                    view,
                    "absent-child",
                    HostSurface::new("slot"),
                    SurfaceViewport::new(SurfaceSize::new(100, 100), 1.0),
                ),
                Err(RuntimeError::Host(_))
            ));
        }
    }

    #[test]
    fn successful_app_retirement_discards_callbacks_racing_the_last_host_drain() {
        let mut runtime = Runtime::new(JournalHost::default());
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let registrar = runtime.host().registrar.clone().unwrap();
        runtime
            .call_host(|host| host.destroy_webview(source))
            .unwrap();
        let reserved = registrar
            .reserve_child(source, "racing-child".into())
            .unwrap();
        let child = reserved.webview();
        reserved.commit(creation("racing-child", "native-child"));
        registrar.record_event(source, HostEvent::PageTitleChanged { title: None });
        runtime.registry.lock().unwrap().destroy_webview(source);
        registrar.record_event(source, HostEvent::PageTitleChanged { title: None });
        let events = runtime.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].created_webview, Some(child));
        runtime.reload(child).unwrap();
    }

    #[test]
    fn failed_destroy_is_retryable_and_native_retirement_drops_late_callbacks() {
        let mut runtime = Runtime::new(JournalHost::default());
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        runtime.host_mut().fail_destroy = true;
        assert!(runtime.destroy_webview(source).is_err());
        runtime.reload(source).unwrap();
        runtime.host_mut().fail_destroy = false;
        let registrar = runtime.host().registrar.clone().unwrap();
        registrar.retire_webview(source);
        registrar.retire_webview(source);
        registrar.record_event(source, HostEvent::UrlChanged { url: "late".into() });
        assert_eq!(
            runtime.reload(source),
            Err(RuntimeError::UnknownWebView(source))
        );
        assert_eq!(
            runtime.drain_events(),
            vec![event(source, HostEvent::Closed)]
        );
        assert!(runtime.drain_events().is_empty());
    }

    #[test]
    fn legacy_observer_failure_preserves_events_once_and_unsupported_handler_is_explicit() {
        let mut runtime = Runtime::new(JournalHost::default());
        let session = runtime.create_session();
        let source = runtime.create_webview(session).unwrap();
        let title = HostEvent::PageTitleChanged {
            title: Some("historical title".into()),
        };
        let returned = title.clone();
        runtime.host_mut().update = Some(Box::new(move |_| Ok(vec![returned])));
        runtime.host_mut().fail_observer = true;
        assert!(runtime.perform_updates(source).is_err());
        assert_eq!(runtime.drain_events(), vec![event(source, title)]);
        assert!(runtime.drain_events().is_empty());
        assert_eq!(runtime.host().observed, 1);
        assert!(matches!(
            runtime.set_new_webview_handler(source, Some(Rc::new(|_| NewWebViewDecision::Create))),
            Err(RuntimeError::Host(_))
        ));
    }

    #[test]
    fn destroying_either_view_preserves_sibling_routing_and_allows_replacement() {
        for close_first in [true, false] {
            let mut runtime = Runtime::new(PlaceholderHost::default());
            let session = runtime.create_session();
            let first = runtime.create_webview(session).unwrap();
            let second = runtime.create_webview(session).unwrap();
            let (closed, survivor) = if close_first {
                (first, second)
            } else {
                (second, first)
            };
            runtime.load_url(closed, "https://closed.test/").unwrap();
            runtime
                .load_url(survivor, "https://survivor.test/")
                .unwrap();
            runtime.destroy_webview(closed).unwrap();
            assert_eq!(
                runtime.reload(closed),
                Err(RuntimeError::UnknownWebView(closed))
            );
            let events = runtime.drain_events();
            assert!(events.iter().all(|event| event.webview == survivor));
            assert!(events.iter().any(|event|
                matches!(&event.event, HostEvent::UrlChanged { url } if url == "https://survivor.test/")));
            runtime.reload(survivor).unwrap();
            let replacement = runtime.create_webview(session).unwrap();
            assert_ne!(replacement, closed);
            runtime.reload(replacement).unwrap();
            runtime.destroy_webview(survivor).unwrap();
            runtime.destroy_webview(replacement).unwrap();
            runtime.perform_all_updates().unwrap();
            assert!(runtime.drain_events().is_empty());
        }
    }

    #[test]
    fn batch_updates_deliver_sibling_events_when_one_view_fails() {
        struct FailingViewHost {
            failed_view: Option<WebViewHandle>,
        }
        impl Host for FailingViewHost {
            fn observe_webview_events(
                &mut self,
                _: WebViewHandle,
                _: &[HostEvent],
            ) -> Result<(), HostError> {
                Ok(())
            }

            fn create_webview(
                &mut self,
                _: SessionHandle,
                view: WebViewHandle,
            ) -> Result<(), HostError> {
                self.failed_view.get_or_insert(view);
                Ok(())
            }
            fn perform_updates(
                &mut self,
                view: WebViewHandle,
            ) -> Result<Vec<HostEvent>, HostError> {
                if Some(view) == self.failed_view {
                    return Err(HostError::new("view failed"));
                }
                Ok(vec![HostEvent::PageTitleChanged {
                    title: Some("survivor".into()),
                }])
            }
        }
        let mut runtime = Runtime::new(FailingViewHost { failed_view: None });
        let session = runtime.create_session();
        let first = runtime.create_webview(session).unwrap();
        let second = runtime.create_webview(session).unwrap();
        assert_eq!(
            runtime.perform_all_updates(),
            Err(RuntimeError::WebView {
                webview: first,
                error: HostError::new("view failed"),
            })
        );
        assert_eq!(
            runtime.drain_events(),
            vec![event(
                second,
                HostEvent::PageTitleChanged {
                    title: Some("survivor".into())
                }
            )]
        );
    }

    fn event(webview: WebViewHandle, event: HostEvent) -> ExplorerKitEvent {
        ExplorerKitEvent {
            webview,
            created_webview: None,
            managed_child_webview_id: None,
            event,
        }
    }

    fn child_event(
        webview: WebViewHandle,
        child_webview_id: impl Into<String>,
        event: HostEvent,
    ) -> ExplorerKitEvent {
        ExplorerKitEvent {
            webview,
            created_webview: None,
            managed_child_webview_id: Some(child_webview_id.into()),
            event,
        }
    }

    fn load_events(
        webview: WebViewHandle,
        url: &str,
        history: Vec<String>,
        current: usize,
    ) -> Vec<ExplorerKitEvent> {
        vec![
            event(
                webview,
                HostEvent::LoadStatusChanged {
                    status: LoadStatusKind::Started,
                },
            ),
            event(
                webview,
                HostEvent::UrlChanged {
                    url: url.to_owned(),
                },
            ),
            event(
                webview,
                HostEvent::HistoryChanged {
                    can_go_back: current > 0,
                    can_go_forward: current + 1 < history.len(),
                    entries: history,
                    current,
                },
            ),
            event(
                webview,
                HostEvent::LoadStatusChanged {
                    status: LoadStatusKind::Complete,
                },
            ),
        ]
    }

    #[derive(Default)]
    struct InputRecordingHost {
        created_webviews: HashSet<WebViewHandle>,
        inputs: Vec<(WebViewHandle, HostInputEvent)>,
        observed_events: Vec<ExplorerKitEvent>,
    }

    impl Host for InputRecordingHost {
        fn create_webview(
            &mut self,
            _session: SessionHandle,
            webview: WebViewHandle,
        ) -> Result<(), HostError> {
            self.created_webviews.insert(webview);
            Ok(())
        }

        fn dispatch_input_event(
            &mut self,
            webview: WebViewHandle,
            input: &HostInputEvent,
        ) -> Result<Vec<HostEvent>, HostError> {
            if !self.created_webviews.contains(&webview) {
                return Err(HostError::new(format!(
                    "webview {} is not registered",
                    webview.raw()
                )));
            }

            self.inputs.push((webview, input.clone()));
            match input {
                HostInputEvent::Focus { is_focused } => Ok(vec![HostEvent::FocusChanged {
                    is_focused: *is_focused,
                }]),
                _ => Ok(Vec::new()),
            }
        }

        fn observe_webview_events(
            &mut self,
            webview: WebViewHandle,
            events: &[HostEvent],
        ) -> Result<(), HostError> {
            self.observed_events
                .extend(events.iter().cloned().map(|event| ExplorerKitEvent {
                    webview,
                    created_webview: None,
                    managed_child_webview_id: None,
                    event,
                }));
            Ok(())
        }
    }

    impl ConfigurableHost for InputRecordingHost {
        type WebViewOptions = bool;

        fn create_webview_with_options(
            &mut self,
            session: SessionHandle,
            webview: WebViewHandle,
            allowed: Self::WebViewOptions,
        ) -> Result<(), HostError> {
            if !allowed {
                return Err(HostError::new("create option rejected"));
            }
            self.create_webview(session, webview)
        }
    }

    #[test]
    fn configurable_host_owns_create_options_without_changing_runtime_handles() {
        let mut runtime = Runtime::new(InputRecordingHost::default());
        let unknown_session = SessionHandle(99);
        assert_eq!(
            runtime.create_webview_with_options(unknown_session, true),
            Err(RuntimeError::UnknownSession(unknown_session))
        );

        let session = runtime.create_session();
        assert!(matches!(
            runtime.create_webview_with_options(session, false),
            Err(RuntimeError::Host(_))
        ));
        let webview = runtime.create_webview_with_options(session, true).unwrap();
        assert_eq!(webview.raw(), 2);
        runtime
            .dispatch_input_event(webview, HostInputEvent::Focus { is_focused: true })
            .unwrap();
    }

    #[test]
    fn native_caller_can_create_handles_load_url_and_receive_servo_events() {
        let mut runtime = Runtime::new(PlaceholderHost::default());

        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        runtime.load_url(webview, "example.com").unwrap();

        assert_eq!(session.raw(), 1);
        assert_eq!(webview.raw(), 1);

        let expected_events = load_events(
            webview,
            "https://example.com/",
            vec!["https://example.com/".to_owned()],
            0,
        );

        assert_eq!(runtime.host().observed_events(), expected_events.as_slice());
        assert_eq!(runtime.drain_events(), expected_events);
        assert!(runtime.drain_events().is_empty());
    }

    #[test]
    fn stop_and_current_loading_validate_lifetimes_and_default_to_unsupported() {
        struct DefaultHost;
        impl Host for DefaultHost {
            fn destroy_webview(&mut self, _: WebViewHandle) -> Result<(), HostError> {
                Ok(())
            }
            fn create_webview(
                &mut self,
                _: SessionHandle,
                _: WebViewHandle,
            ) -> Result<(), HostError> {
                Ok(())
            }
            fn observe_webview_events(
                &mut self,
                _: WebViewHandle,
                _: &[HostEvent],
            ) -> Result<(), HostError> {
                Ok(())
            }
        }
        let mut runtime = Runtime::new(DefaultHost);
        let session = runtime.create_session();
        let view = runtime.create_webview(session).unwrap();
        assert!(runtime
            .stop_loading(view)
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
        assert!(runtime
            .system_webview_is_loading(view)
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
        assert!(runtime.drain_events().is_empty());
        runtime.destroy_webview(view).unwrap();
        assert_eq!(
            runtime.stop_loading(view),
            Err(RuntimeError::UnknownWebView(view))
        );
        assert_eq!(
            runtime.system_webview_is_loading(view),
            Err(RuntimeError::UnknownWebView(view))
        );
    }

    #[test]
    fn loading_query_reads_current_host_state_despite_queued_old_completion() {
        #[derive(Default)]
        struct LoadingHost {
            loading: HashMap<WebViewHandle, bool>,
        }
        impl Host for LoadingHost {
            fn observe_webview_events(
                &mut self,
                _: WebViewHandle,
                _: &[HostEvent],
            ) -> Result<(), HostError> {
                Ok(())
            }
            fn create_webview(
                &mut self,
                _: SessionHandle,
                view: WebViewHandle,
            ) -> Result<(), HostError> {
                self.loading.insert(view, true);
                Ok(())
            }
            fn dispatch_webview_command(
                &mut self,
                view: WebViewHandle,
                command: &WebViewCommand,
            ) -> Result<Vec<HostEvent>, HostError> {
                *self.loading.get_mut(&view).unwrap() =
                    !matches!(command, WebViewCommand::StopLoading);
                Ok(Vec::new())
            }
            fn system_webview_is_loading(
                &mut self,
                view: WebViewHandle,
            ) -> Result<bool, HostError> {
                Ok(self.loading[&view])
            }
        }
        let mut runtime = Runtime::new(LoadingHost::default());
        let session = runtime.create_session();
        let stopped = runtime.create_webview(session).unwrap();
        let sibling = runtime.create_webview(session).unwrap();
        runtime.stop_loading(stopped).unwrap();
        assert!(!runtime.system_webview_is_loading(stopped).unwrap());
        assert!(runtime.system_webview_is_loading(sibling).unwrap());
        assert!(runtime.drain_events().is_empty());
        runtime
            .record_events(
                stopped,
                vec![HostEvent::LoadStatusChanged {
                    status: LoadStatusKind::Complete,
                }],
            )
            .unwrap();
        runtime.reload(stopped).unwrap();
        runtime.drain_events();
        assert!(runtime.system_webview_is_loading(stopped).unwrap());
        runtime.stop_loading(stopped).unwrap();
        assert!(!runtime.system_webview_is_loading(stopped).unwrap());
    }

    #[test]
    fn runtime_validates_session_webview_url_and_surface_state_before_host_dispatch() {
        let mut runtime = Runtime::new(PlaceholderHost::default());
        let unknown_session = SessionHandle(99);

        assert_eq!(
            runtime.create_webview(unknown_session),
            Err(RuntimeError::UnknownSession(unknown_session))
        );
        assert!(runtime.host().calls().is_empty());
        assert!(runtime.host().observed_events().is_empty());

        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let calls_after_create = runtime.host().calls().len();

        assert!(matches!(
            runtime.load_url(webview, "   "),
            Err(RuntimeError::InvalidUrl(NavigationError::Empty))
        ));
        assert!(matches!(
            runtime.load_url(webview, "http://[::1"),
            Err(RuntimeError::InvalidUrl(NavigationError::Invalid(_)))
        ));
        assert_eq!(runtime.host().calls().len(), calls_after_create);

        let unknown_webview = WebViewHandle(42);
        assert_eq!(
            runtime.load_url(unknown_webview, "example.com"),
            Err(RuntimeError::UnknownWebView(unknown_webview))
        );
        assert_eq!(
            runtime.dispatch_webview_command(unknown_webview, WebViewCommand::Reload),
            Err(RuntimeError::UnknownWebView(unknown_webview))
        );
        assert_eq!(
            runtime.attach_surface(
                unknown_webview,
                HostSurface::new("missing-surface"),
                SurfaceSize::new(1, 1),
            ),
            Err(RuntimeError::UnknownWebView(unknown_webview))
        );
        assert_eq!(
            runtime.resize_surface(unknown_webview, SurfaceSize::new(640, 480)),
            Err(RuntimeError::UnknownWebView(unknown_webview))
        );
        assert_eq!(
            runtime.set_surface_scale_factor(unknown_webview, 2.0),
            Err(RuntimeError::UnknownWebView(unknown_webview))
        );
        assert_eq!(
            runtime.update_surface_viewport(
                unknown_webview,
                SurfaceViewport::new(SurfaceSize::new(640, 480), 2.0),
            ),
            Err(RuntimeError::UnknownWebView(unknown_webview))
        );
        assert_eq!(
            runtime.detach_surface(unknown_webview),
            Err(RuntimeError::UnknownWebView(unknown_webview))
        );
        assert_eq!(
            runtime.perform_updates(unknown_webview),
            Err(RuntimeError::UnknownWebView(unknown_webview))
        );
        assert_eq!(
            runtime.resize_surface(webview, SurfaceSize::new(640, 480)),
            Err(RuntimeError::SurfaceNotAttached(webview))
        );
        assert_eq!(
            runtime.set_surface_scale_factor(webview, 2.0),
            Err(RuntimeError::SurfaceNotAttached(webview))
        );
        assert_eq!(
            runtime.update_surface_viewport(
                webview,
                SurfaceViewport::new(SurfaceSize::new(640, 480), 2.0),
            ),
            Err(RuntimeError::SurfaceNotAttached(webview))
        );
        assert_eq!(
            runtime.detach_surface(webview),
            Err(RuntimeError::SurfaceNotAttached(webview))
        );
        assert_eq!(runtime.host().calls().len(), calls_after_create);
        assert!(runtime.host().observed_events().is_empty());
        assert!(runtime.drain_events().is_empty());
    }

    #[test]
    fn runtime_dispatches_host_input_events_to_host_and_records_events() {
        let mut runtime = Runtime::new(InputRecordingHost::default());
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let pointer = HostInputEvent::Pointer(PointerInputEvent::moved(12.0, 34.0));
        let keyboard = HostInputEvent::Keyboard(KeyboardInputEvent::new(
            KeyboardInputKey::Character("a".to_owned()),
            KeyboardInputState::Pressed,
        ));
        let focus = HostInputEvent::Focus { is_focused: true };

        runtime
            .dispatch_input_event(webview, pointer.clone())
            .unwrap();
        runtime
            .dispatch_input_event(webview, keyboard.clone())
            .unwrap();
        runtime
            .dispatch_input_event(webview, focus.clone())
            .unwrap();

        assert_eq!(
            runtime.host().inputs,
            vec![(webview, pointer), (webview, keyboard), (webview, focus)]
        );
        let expected_events = vec![event(webview, HostEvent::FocusChanged { is_focused: true })];
        assert_eq!(runtime.host().observed_events, expected_events);
        assert_eq!(runtime.drain_events(), expected_events);
    }

    #[test]
    fn runtime_resolves_navigation_policy_requests_through_host() {
        let mut runtime = Runtime::new(PlaceholderHost::default());
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();

        runtime
            .resolve_navigation_request(webview, "navigation-1", true)
            .unwrap();

        assert_eq!(
            runtime.host().calls(),
            &[
                HostCall::CreateWebView { session, webview },
                HostCall::ResolveNavigationRequest {
                    webview,
                    navigation_id: "navigation-1".to_owned(),
                    allow: true,
                },
            ]
        );
        assert!(runtime.drain_events().is_empty());
    }

    #[test]
    fn surface_lifecycle_crosses_the_host_boundary_and_emits_shared_events() {
        let mut runtime = Runtime::new(PlaceholderHost::default());
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let surface = HostSurface::new("mock-surface");
        let initial_size = SurfaceSize::new(640, 480);
        let resized_size = SurfaceSize::new(800, 600);
        let reattached_size = SurfaceSize::new(1024, 768);
        let initial_viewport = SurfaceViewport::new(initial_size, 2.0);

        runtime
            .attach_surface_with_viewport(webview, surface.clone(), initial_viewport)
            .unwrap();
        assert_eq!(
            runtime.attach_surface(webview, surface.clone(), initial_size),
            Err(RuntimeError::SurfaceAlreadyAttached(webview))
        );
        runtime
            .update_surface_viewport(webview, SurfaceViewport::new(resized_size, 2.5))
            .unwrap();
        runtime.detach_surface(webview).unwrap();

        let calls_after_detach = runtime.host().calls().len();
        let observed_events_after_detach = runtime.host().observed_events().len();
        assert_eq!(
            runtime.resize_surface(webview, SurfaceSize::new(320, 240)),
            Err(RuntimeError::SurfaceNotAttached(webview))
        );
        assert_eq!(
            runtime.detach_surface(webview),
            Err(RuntimeError::SurfaceNotAttached(webview))
        );
        assert_eq!(runtime.host().calls().len(), calls_after_detach);
        assert_eq!(
            runtime.host().observed_events().len(),
            observed_events_after_detach
        );

        runtime
            .attach_surface(webview, surface.clone(), reattached_size)
            .unwrap();

        assert_eq!(
            runtime.host().calls(),
            &[
                HostCall::CreateWebView { session, webview },
                HostCall::AttachSurfaceWithViewport {
                    webview,
                    surface: surface.clone(),
                    viewport: initial_viewport,
                },
                HostCall::UpdateSurfaceViewport {
                    webview,
                    viewport: SurfaceViewport::new(resized_size, 2.5),
                },
                HostCall::DetachSurface { webview },
                HostCall::AttachSurface {
                    webview,
                    surface,
                    size: reattached_size,
                },
            ]
        );

        let expected_events = vec![
            event(webview, HostEvent::SurfaceAttached { size: initial_size }),
            event(webview, HostEvent::SurfaceResized { size: resized_size }),
            event(webview, HostEvent::SurfaceDetached),
            event(
                webview,
                HostEvent::SurfaceAttached {
                    size: reattached_size,
                },
            ),
        ];
        assert_eq!(runtime.host().observed_events(), expected_events.as_slice());
        assert_eq!(runtime.drain_events(), expected_events);
    }

    #[test]
    fn core_browser_commands_are_dispatched_and_emit_ordered_events() {
        let mut runtime = Runtime::new(PlaceholderHost::default());
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let first = "https://servo.org/".to_owned();
        let second = "https://example.org/page".to_owned();

        runtime.load_url(webview, "servo.org").unwrap();
        runtime.load_url(webview, &second).unwrap();
        runtime.reload(webview).unwrap();
        runtime.go_back(webview).unwrap();
        runtime.go_forward(webview).unwrap();
        runtime.focus(webview).unwrap();
        runtime.blur(webview).unwrap();

        let command_calls: Vec<_> = runtime
            .host()
            .calls()
            .iter()
            .filter_map(|call| match call {
                HostCall::DispatchWebViewCommand { webview, command } => {
                    Some((*webview, command.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            command_calls,
            vec![
                (
                    webview,
                    WebViewCommand::LoadUrl(NavigationRequest::new("servo.org").unwrap()),
                ),
                (
                    webview,
                    WebViewCommand::LoadUrl(NavigationRequest::new(&second).unwrap()),
                ),
                (webview, WebViewCommand::Reload),
                (webview, WebViewCommand::GoBack),
                (webview, WebViewCommand::GoForward),
                (webview, WebViewCommand::Focus),
                (webview, WebViewCommand::Blur),
            ]
        );

        let mut expected_events = load_events(webview, &first, vec![first.clone()], 0);
        expected_events.extend(load_events(
            webview,
            &second,
            vec![first.clone(), second.clone()],
            1,
        ));
        expected_events.extend([
            event(
                webview,
                HostEvent::LoadStatusChanged {
                    status: LoadStatusKind::Started,
                },
            ),
            event(
                webview,
                HostEvent::LoadStatusChanged {
                    status: LoadStatusKind::Complete,
                },
            ),
        ]);
        expected_events.extend(load_events(
            webview,
            &first,
            vec![first.clone(), second.clone()],
            0,
        ));
        expected_events.extend(load_events(
            webview,
            &second,
            vec![first.clone(), second.clone()],
            1,
        ));
        expected_events.extend([
            event(webview, HostEvent::FocusChanged { is_focused: true }),
            event(webview, HostEvent::FocusChanged { is_focused: false }),
        ]);

        assert_eq!(runtime.drain_events(), expected_events);
    }

    #[derive(Default)]
    struct HostDrivenEventsHost {
        created_webviews: HashSet<WebViewHandle>,
    }

    impl Host for HostDrivenEventsHost {
        fn create_webview(
            &mut self,
            _session: SessionHandle,
            webview: WebViewHandle,
        ) -> Result<(), HostError> {
            self.created_webviews.insert(webview);
            Ok(())
        }

        fn attach_surface(
            &mut self,
            webview: WebViewHandle,
            _surface: &HostSurface,
            _size: SurfaceSize,
        ) -> Result<Vec<HostEvent>, HostError> {
            self.ensure_webview(webview)?;
            Ok(vec![HostEvent::CursorChanged {
                cursor: "pointer".to_owned(),
            }])
        }

        fn dispatch_webview_command(
            &mut self,
            webview: WebViewHandle,
            command: &WebViewCommand,
        ) -> Result<Vec<HostEvent>, HostError> {
            self.ensure_webview(webview)?;
            match command {
                WebViewCommand::LoadUrl(request) => Ok(vec![HostEvent::UrlChanged {
                    url: request.url.clone(),
                }]),
                _ => Ok(Vec::new()),
            }
        }

        fn observe_webview_events(
            &mut self,
            webview: WebViewHandle,
            _events: &[HostEvent],
        ) -> Result<(), HostError> {
            self.ensure_webview(webview)
        }

        fn synthesize_webview_command_events(&self) -> bool {
            false
        }
    }

    impl HostDrivenEventsHost {
        fn ensure_webview(&self, webview: WebViewHandle) -> Result<(), HostError> {
            self.created_webviews
                .contains(&webview)
                .then_some(())
                .ok_or_else(|| HostError::new("webview was not created"))
        }
    }

    #[test]
    fn runtime_can_record_host_driven_events_without_synthesizing_command_events() {
        let mut runtime = Runtime::new(HostDrivenEventsHost::default());
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();

        runtime.load_url(webview, "example.com").unwrap();
        runtime
            .attach_surface(
                webview,
                HostSurface::new("host-surface"),
                SurfaceSize::new(320, 240),
            )
            .unwrap();

        assert_eq!(
            runtime.drain_events(),
            vec![
                event(
                    webview,
                    HostEvent::UrlChanged {
                        url: "https://example.com/".to_owned(),
                    },
                ),
                event(
                    webview,
                    HostEvent::SurfaceAttached {
                        size: SurfaceSize::new(320, 240),
                    },
                ),
                event(
                    webview,
                    HostEvent::CursorChanged {
                        cursor: "pointer".to_owned(),
                    },
                ),
            ]
        );
    }

    #[test]
    fn managed_popup_child_surface_lifecycle_is_child_scoped_and_preserves_parent_state() {
        let mut runtime = Runtime::new(PlaceholderHost::default());
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let child_webview_id = "servo-child-1".to_owned();
        let popup_created = HostEvent::PopupCreated {
            request_id: "request-1".to_owned(),
            parent_webview_id: "servo-parent-1".to_owned(),
            child_webview_id: child_webview_id.clone(),
            parent_url: Some("https://parent.test/".to_owned()),
            target_url: None,
            window_features: None,
            wants_popup: None,
            policy: PopupRequestPolicy::ManagedChild,
        };
        let surface = HostSurface::new("popup-child-surface");
        let initial_viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let resized_viewport = SurfaceViewport::new(SurfaceSize::new(480, 320), 2.5);

        runtime
            .set_popup_policy(webview, PopupRequestPolicy::ManagedChild)
            .unwrap();
        runtime.host_mut().queue_update_event(popup_created.clone());
        runtime.perform_updates(webview).unwrap();
        runtime
            .attach_managed_child_surface(
                webview,
                child_webview_id.clone(),
                surface.clone(),
                initial_viewport,
            )
            .unwrap();
        assert_eq!(
            runtime.attach_managed_child_surface(
                webview,
                child_webview_id.clone(),
                surface.clone(),
                initial_viewport,
            ),
            Err(RuntimeError::ManagedChildSurfaceAlreadyAttached(
                webview,
                child_webview_id.clone()
            ))
        );
        runtime.host_mut().queue_managed_child_update_event(
            webview,
            child_webview_id.clone(),
            HostEvent::PageTitleChanged {
                title: Some("Child title".to_owned()),
            },
        );
        runtime
            .perform_managed_child_updates(webview, &child_webview_id)
            .unwrap();
        runtime
            .update_managed_child_surface_viewport(webview, &child_webview_id, resized_viewport)
            .unwrap();
        runtime
            .detach_managed_child_surface(webview, &child_webview_id)
            .unwrap();
        assert_eq!(
            runtime.perform_managed_child_updates(webview, &child_webview_id),
            Err(RuntimeError::ManagedChildSurfaceNotAttached(
                webview,
                child_webview_id.clone()
            ))
        );

        runtime.load_url(webview, "example.com").unwrap();

        assert_eq!(
            runtime.host().calls(),
            &[
                HostCall::CreateWebView { session, webview },
                HostCall::SetPopupPolicy {
                    webview,
                    popup_policy: PopupRequestPolicy::ManagedChild,
                },
                HostCall::PerformUpdates { webview },
                HostCall::AttachManagedChildSurface {
                    webview,
                    child_webview_id: child_webview_id.clone(),
                    surface,
                    viewport: initial_viewport,
                },
                HostCall::PerformManagedChildUpdates {
                    webview,
                    child_webview_id: child_webview_id.clone(),
                },
                HostCall::UpdateManagedChildSurfaceViewport {
                    webview,
                    child_webview_id: child_webview_id.clone(),
                    viewport: resized_viewport,
                },
                HostCall::DetachManagedChildSurface {
                    webview,
                    child_webview_id: child_webview_id.clone(),
                },
                HostCall::DispatchWebViewCommand {
                    webview,
                    command: WebViewCommand::LoadUrl(
                        NavigationRequest::new("example.com").unwrap()
                    ),
                },
            ]
        );

        let mut expected_events = vec![
            event(webview, popup_created),
            child_event(
                webview,
                child_webview_id.clone(),
                HostEvent::SurfaceAttached {
                    size: initial_viewport.size,
                },
            ),
            child_event(
                webview,
                child_webview_id.clone(),
                HostEvent::PageTitleChanged {
                    title: Some("Child title".to_owned()),
                },
            ),
            child_event(
                webview,
                child_webview_id.clone(),
                HostEvent::SurfaceResized {
                    size: resized_viewport.size,
                },
            ),
            child_event(webview, child_webview_id, HostEvent::SurfaceDetached),
        ];
        expected_events.extend(load_events(
            webview,
            "https://example.com/",
            vec!["https://example.com/".to_owned()],
            0,
        ));

        assert_eq!(runtime.host().observed_events(), expected_events.as_slice());
        assert_eq!(runtime.drain_events(), expected_events);
    }

    #[test]
    fn default_denied_popup_events_flow_through_runtime_without_child_webview() {
        let popup_event = HostEvent::PopupRequested {
            request_id: "request-1".to_owned(),
            parent_webview_id: "servo-parent-1".to_owned(),
            parent_url: Some("https://parent.test/".to_owned()),
            target_url: None,
            window_features: None,
            wants_popup: None,
            policy: PopupRequestPolicy::DefaultDeny,
        };
        let host = PlaceholderHost::with_update_events([popup_event.clone()]);
        let mut runtime = Runtime::new(host);
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();

        runtime.perform_updates(webview).unwrap();

        assert_eq!(
            runtime.host().calls(),
            &[
                HostCall::CreateWebView { session, webview },
                HostCall::PerformUpdates { webview },
            ]
        );
        let expected_events = vec![event(webview, popup_event)];
        assert_eq!(runtime.host().observed_events(), expected_events.as_slice());
        assert_eq!(runtime.drain_events(), expected_events);
    }

    #[test]
    fn host_updates_are_drained_after_prior_facade_events() {
        let host = PlaceholderHost::with_update_events([
            HostEvent::PageTitleChanged {
                title: Some("ExplorerKit".to_owned()),
            },
            HostEvent::StatusTextChanged {
                status: Some("ready".to_owned()),
            },
        ]);
        let mut runtime = Runtime::new(host);
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();

        runtime.load_url(webview, "example.com").unwrap();
        runtime.perform_updates(webview).unwrap();

        let mut expected_events = load_events(
            webview,
            "https://example.com/",
            vec!["https://example.com/".to_owned()],
            0,
        );
        expected_events.extend([
            event(
                webview,
                HostEvent::PageTitleChanged {
                    title: Some("ExplorerKit".to_owned()),
                },
            ),
            event(
                webview,
                HostEvent::StatusTextChanged {
                    status: Some("ready".to_owned()),
                },
            ),
        ]);

        assert_eq!(runtime.host().observed_events(), expected_events.as_slice());
        assert_eq!(runtime.drain_events(), expected_events);
        assert!(matches!(
            runtime.host().calls().last(),
            Some(HostCall::PerformUpdates { webview: called }) if *called == webview
        ));

        let calls_after_first_drain = runtime.host().calls().len();
        let observed_events_after_first_drain = runtime.host().observed_events().to_vec();
        runtime.perform_updates(webview).unwrap();

        assert!(runtime.drain_events().is_empty());
        assert_eq!(
            runtime.host().observed_events(),
            observed_events_after_first_drain.as_slice()
        );
        assert_eq!(
            &runtime.host().calls()[calls_after_first_drain..],
            &[HostCall::PerformUpdates { webview }]
        );
    }
}
