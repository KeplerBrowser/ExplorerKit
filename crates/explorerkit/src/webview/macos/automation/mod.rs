//! Opt-in automation of this host's macOS system-WebKit views.
//!
//! This alpha adapter uses system WebKit SPI and a compatible externally launched
//! minidriver. It is not Servo automation. The host owns helper launch/reaping,
//! presentation and the AppKit event loop. Keep pumping Runtime updates while a
//! standard client runs on another thread. All operations are main-thread only.
//! Protocol-created views are prepared outside native callbacks. Eligibility also
//! opts into the existing `SimpleDialogRequested`/`resolve_simple_dialog` contract.

use super::{
    HostError, MacOsViewHost, MacOsViewHostOptions, SystemCallbackSink, SystemCallbacks,
    WebViewHandle,
};
use objc2::{
    rc::{Retained, Weak as NativeWeak},
    MainThreadMarker,
};
use objc2_web_kit::WKWebView;
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    os::fd::OwnedFd,
    rc::{Rc, Weak},
};

pub(super) mod dialogs;
pub(super) mod native;
mod transport;

/// A weak control handle. Clones never retain the host, Runtime, or native views.
///
/// Like the AppKit host, this handle is neither `Send` nor `Sync`. After host
/// destruction its methods return an error. Do not call it from a selection hook.
#[derive(Clone)]
pub struct WebKitAutomation {
    state: Weak<State>,
}
/// A native context identity valid only in the indicated automation session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebKitAutomationContext {
    /// Standard session identifier; verify it against the client's session.
    pub session_id: String,
    /// Native top-level handle for explicit standard context selection.
    pub handle: String,
}
/// The state of the adopted connection, independent of the host's helper process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebKitAutomationStatus {
    /// No connection is owned.
    Disconnected,
    /// The descriptor was adopted; the native session handshake is incomplete.
    AwaitingSession,
    /// Native target pairing completed.
    Active {
        /// Standard session identifier.
        session_id: String,
    },
    /// An asynchronous transport or handshake error ended the connection.
    Failed {
        /// The local failure description.
        message: String,
    },
}
/// Opaque, host- and session-bound preparation token; not a protocol identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WebKitAutomationNewViewRequestId {
    host: u64,
    epoch: u64,
    sequence: u64,
}
/// Fallible host preparation requested before entering native context creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebKitAutomationNewViewRequest {
    /// Resolve once with a fresh attached view, or `None` to decline.
    pub id: WebKitAutomationNewViewRequestId,
    /// Native presentation hint; the host still owns placement.
    pub prefer_new_tab: bool,
}

impl MacOsViewHostOptions {
    /// Exposes a host-managed view synchronously when native automation selects it.
    ///
    /// The handler must use independently retained presentation objects, must not
    /// call Runtime or the automation handle, and must not run nested event loops.
    /// It does not imply application activation. No hook is needed for views that
    /// are already presented. Native focus and continuation remain engine-owned.
    pub fn with_webkit_automation_view_selection_handler(
        mut self,
        handler: Rc<dyn Fn(WebViewHandle)>,
    ) -> Self {
        self.automation_selection = Some(handler);
        self
    }
}
impl<S> MacOsViewHost<S> {
    /// Obtains a weak automation control before moving this host into Runtime.
    pub fn webkit_automation(&self) -> WebKitAutomation {
        WebKitAutomation {
            state: Rc::downgrade(&self.automation),
        }
    }
}
impl WebKitAutomation {
    fn owner(&self) -> Result<Rc<State>, HostError> {
        MainThreadMarker::new()
            .ok_or_else(|| HostError::new("WebKit automation requires AppKit's main thread"))?;
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| HostError::new("automation host has been destroyed"))?;
        if state.dispatching.get() {
            return Err(HostError::new(
                "automation control cannot reenter a native callback",
            ));
        }
        Ok(state)
    }
    /// Adopts a connected AF_UNIX stream descriptor, consuming it on every result.
    ///
    /// Requires an instantiated eligible view, available SPI, and no connection.
    /// Success means adoption, not native pairing. SDK never starts a helper.
    pub fn connect(&self, connection: OwnedFd) -> Result<(), HostError> {
        self.owner()?.connect(connection)
    }
    /// Ends the connection and invalidates preparation tokens and native mappings.
    /// Existing views and captured dialogs remain owned by the host.
    pub fn disconnect(&self) -> Result<(), HostError> {
        self.owner()?.disconnect(None);
        Ok(())
    }
    /// Reads connection status without polling or launching a helper.
    pub fn status(&self) -> Result<WebKitAutomationStatus, HostError> {
        Ok(self.owner()?.data.borrow().status.clone())
    }
    /// Returns the current native mapping, or `None` before pairing/attachment.
    ///
    /// Unknown, retired, Servo and ineligible handles fail. A handle must belong
    /// to this Runtime; numerically colliding handles from another Runtime cannot
    /// be distinguished. Native identifiers must not be persisted across sessions.
    pub fn context(
        &self,
        webview: WebViewHandle,
    ) -> Result<Option<WebKitAutomationContext>, HostError> {
        let owner = self.owner()?;
        let target = owner.target(webview)?;
        let registry = owner.callbacks.borrow();
        let view = &registry.views[&target].automation;
        if !view.eligible {
            return Err(HostError::new("view is not enabled for WebKit automation"));
        }
        if !matches!(
            owner.data.borrow().status,
            WebKitAutomationStatus::Active { .. }
        ) {
            return Ok(None);
        }
        Ok(view.context.clone())
    }
    /// Enables an existing system view in place, or stores eligibility until attach.
    ///
    /// Opting out requires no connection and no captured dialog. Eligibility is
    /// enumeration policy, not isolation from related native references or storage.
    pub fn set_view_automation(
        &self,
        webview: WebViewHandle,
        enabled: bool,
    ) -> Result<(), HostError> {
        let owner = self.owner()?;
        native::check_available()?;
        let target = owner.target(webview)?;
        let connected = owner.data.borrow().transport.is_some();
        let native = {
            let mut registry = owner.callbacks.borrow_mut();
            let view = &mut registry.views.get_mut(&target).unwrap().automation;
            if !enabled && (connected || view.dialog.is_some()) {
                return Err(HostError::new(
                    "cannot disable automation while connected or a dialog is pending",
                ));
            }
            view.eligible = enabled;
            view.owner = Rc::downgrade(&owner);
            view.native.as_ref().and_then(NativeWeak::load)
        };
        if let Some(view) = native {
            native::set_eligible(&view, enabled);
            if enabled {
                owner.register(target, &view);
            }
        }
        Ok(())
    }
    /// Drains newly queued host preparation requests without entering native code.
    /// The host must resolve every request or end its connection; no timeout or
    /// implicit window is created. Draining does not transfer candidate ownership.
    pub fn drain_new_view_requests(
        &self,
    ) -> Result<Vec<WebKitAutomationNewViewRequest>, HostError> {
        Ok(self.owner()?.data.borrow_mut().requests.drain(..).collect())
    }
    /// Settles a preparation request before native creation dispatch.
    ///
    /// `Some(view)` asserts this host created that fresh, attached, eligible view
    /// for this request. It must have been created after the request was queued
    /// and must never have been claimed by another request. `None` lets native
    /// WebKit return its creation error without entering a completion with nil.
    ///
    /// `Err` is pre-adoption: both the pending request and supplied view remain
    /// host-owned, including for stale tokens. `Ok` adopts candidate settlement,
    /// not protocol success: native consumption leaves a normal live view; native
    /// rejection retires the unused candidate completely before later dispatch.
    /// While native owners await the ordinary Runtime update, another resolution
    /// returns a retryable error before adoption. Retry the same request and view
    /// after that update; disconnect instead invalidates the request immediately.
    /// Temporary candidate lifecycle events can be observable even after rejection.
    pub fn resolve_new_view_request(
        &self,
        request: WebKitAutomationNewViewRequestId,
        webview: Option<WebViewHandle>,
    ) -> Result<(), HostError> {
        self.owner()?.resolve(request, webview)
    }
}

#[derive(Default)]
pub(super) struct ViewState {
    pub(super) born: u64,
    pub(super) eligible: bool,
    pub(super) attached: bool,
    pub(super) claimed: bool,
    pub(super) handle: Option<WebViewHandle>,
    pub(super) native: Option<NativeWeak<WKWebView>>,
    pub(super) context: Option<WebKitAutomationContext>,
    pub(super) owner: Weak<State>,
    pub(super) dialog: Option<dialogs::PendingDialog>,
    pub(super) prompts: explorerkit_embedder::ServoAdapterState,
    pub(super) next_dialog: u64,
}
struct PendingCreate {
    original: String,
    born_after: u64,
}
struct Data {
    status: WebKitAutomationStatus,
    epoch: u64,
    sequence: u64,
    transport: Option<Rc<transport::Transport>>,
    session: Option<Rc<native::Session>>,
    incoming: VecDeque<Value>,
    pending: BTreeMap<WebKitAutomationNewViewRequestId, PendingCreate>,
    requests: VecDeque<WebKitAutomationNewViewRequest>,
}
pub(super) struct State {
    identity: u64,
    pub(super) callbacks: Rc<RefCell<SystemCallbacks>>,
    data: RefCell<Data>,
    pub(super) selection: Option<Rc<dyn Fn(WebViewHandle)>>,
    pub(super) dispatching: Cell<bool>,
    // Owner retirement outlives the protocol epoch; disconnect must not reset it.
    pub(super) retirement_pending: Cell<bool>,
    #[cfg(test)]
    dispatch_witness: RefCell<Option<Rc<dyn Fn()>>>,
}
impl State {
    pub(super) fn new(
        callbacks: Rc<RefCell<SystemCallbacks>>,
        selection: Option<Rc<dyn Fn(WebViewHandle)>>,
    ) -> Rc<Self> {
        static NEXT_HOST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let identity = NEXT_HOST
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |id| id.checked_add(1),
            )
            .expect("automation host identity space exhausted");
        Rc::new(Self {
            identity,
            callbacks,
            selection,
            dispatching: Cell::new(false),
            retirement_pending: Cell::new(false),
            #[cfg(test)]
            dispatch_witness: RefCell::new(None),
            data: RefCell::new(Data {
                status: WebKitAutomationStatus::Disconnected,
                epoch: 0,
                sequence: 0,
                transport: None,
                session: None,
                incoming: VecDeque::new(),
                pending: BTreeMap::new(),
                requests: VecDeque::new(),
            }),
        })
    }
    fn target(&self, handle: WebViewHandle) -> Result<super::CallbackTarget, HostError> {
        self.callbacks
            .borrow()
            .views
            .iter()
            .find_map(|(&key, state)| (state.automation.handle == Some(handle)).then_some(key))
            .ok_or_else(|| HostError::new("unknown, retired, or non-WebKit view"))
    }
    pub(super) fn sink_for_native(&self, native: &WKWebView) -> Option<SystemCallbackSink> {
        let registry = self.callbacks.borrow();
        let target = registry.views.iter().find_map(|(&target, state)| {
            state
                .automation
                .native
                .as_ref()
                .and_then(NativeWeak::load)
                .filter(|view| std::ptr::eq(&**view, native))
                .map(|_| target)
        })?;
        Some(SystemCallbackSink {
            target,
            callbacks: self.callbacks.clone(),
        })
    }
    pub(super) fn initialize_view(self: &Rc<Self>, sink: &SystemCallbackSink, enabled: bool) {
        if let Some(state) = self.callbacks.borrow_mut().views.get_mut(&sink.target) {
            state.automation.eligible = enabled;
            state.automation.owner = Rc::downgrade(self);
        }
    }
    pub(super) fn attached(
        self: &Rc<Self>,
        sink: &SystemCallbackSink,
        handle: WebViewHandle,
        native: &WKWebView,
    ) {
        {
            let mut registry = self.callbacks.borrow_mut();
            let view = &mut registry.views.get_mut(&sink.target).unwrap().automation;
            view.handle = Some(handle);
            view.attached = true;
            view.native = Some(NativeWeak::new(native));
        }
        self.register(sink.target, native);
    }
    fn register(&self, target: super::CallbackTarget, view: &WKWebView) {
        let session = self.data.borrow().session.clone();
        let eligible = self
            .callbacks
            .borrow()
            .views
            .get(&target)
            .is_some_and(|state| state.automation.eligible);
        if let Some(session) = session.filter(|_| eligible) {
            let context = session.register(view);
            if let Some(state) = self.callbacks.borrow_mut().views.get_mut(&target) {
                state.automation.context = Some(context);
            }
        }
    }
    fn connect(self: &Rc<Self>, fd: OwnedFd) -> Result<(), HostError> {
        if self.data.borrow().transport.is_some() {
            return Err(HostError::new("an automation connection is already owned"));
        }
        native::check_available()?;
        if self.eligible_views().is_empty() {
            return Err(HostError::new(
                "automation needs an instantiated eligible WebKit view",
            ));
        }
        let epoch = {
            let mut data = self.data.borrow_mut();
            data.epoch += 1;
            data.epoch
        };
        let transport = transport::Transport::open(fd, self, epoch)?;
        let mut data = self.data.borrow_mut();
        data.transport = Some(Rc::new(transport));
        data.status = WebKitAutomationStatus::AwaitingSession;
        Ok(())
    }
    fn eligible_views(&self) -> Vec<(super::CallbackTarget, Retained<WKWebView>)> {
        self.callbacks
            .borrow()
            .views
            .iter()
            .filter_map(|(&key, state)| {
                state
                    .automation
                    .eligible
                    .then(|| state.automation.native.as_ref().and_then(NativeWeak::load))
                    .flatten()
                    .map(|view| (key, view))
            })
            .collect()
    }
    pub(super) fn disconnect(&self, failure: Option<String>) {
        let (transport, session) = {
            let mut data = self.data.borrow_mut();
            data.epoch += 1;
            data.pending.clear();
            data.requests.clear();
            data.incoming.clear();
            data.status = failure
                .map(|message| WebKitAutomationStatus::Failed { message })
                .unwrap_or(WebKitAutomationStatus::Disconnected);
            (data.transport.take(), data.session.take())
        };
        for view in self.callbacks.borrow_mut().views.values_mut() {
            view.automation.context = None;
        }
        // Native teardown and descriptor cleanup can invoke callbacks. No Rust
        // registry/data borrow survives this boundary. Live dialogs stay pending.
        drop(session);
        drop(transport);
        self.wake();
    }
    pub(super) fn native_disconnected(&self) {
        // Clearing the frontend handler during SDK teardown synchronously reports
        // native disconnection too. Preserve the original I/O failure in that case.
        let owns_session = self.data.borrow().session.is_some();
        if owns_session {
            self.disconnect(None);
        }
    }

    pub(super) fn is_epoch(&self, epoch: u64) -> bool {
        self.data.borrow().epoch == epoch
    }
    pub(super) fn enqueue(&self, epoch: u64, event: Value) {
        if self.is_epoch(epoch) {
            self.data.borrow_mut().incoming.push_back(event);
            self.wake();
        }
    }
    pub(super) fn wake(&self) {
        let wake = self.callbacks.borrow().wake.clone();
        wake();
    }
    pub(super) fn send(self: &Rc<Self>, event: Value) {
        let (transport, epoch) = {
            let data = self.data.borrow();
            (data.transport.clone(), data.epoch)
        };
        if let Some(transport) = transport {
            transport.send(&event, self, epoch);
        }
    }
    /// One message per host retirement boundary. A queued preparation does not
    /// block unrelated opaque requests, but native entry itself is serialized.
    pub(super) fn pump_one(self: &Rc<Self>) -> bool {
        if self.retirement_pending.get() {
            return false;
        }
        let event = self.data.borrow_mut().incoming.pop_front();
        let Some(event) = event else {
            return false;
        };
        if let Err(error) = self.receive(event) {
            self.disconnect(Some(error.to_string()));
        }
        true
    }
    fn receive(self: &Rc<Self>, event: Value) -> Result<(), HostError> {
        match event["event"].as_str() {
            Some("StartAutomationSession") => {
                if self.data.borrow().session.is_some() {
                    return Err(HostError::new("duplicate native automation session"));
                }
                let message: Value = serde_json::from_str(
                    event["message"]
                        .as_str()
                        .ok_or_else(|| HostError::new("missing session handshake"))?,
                )
                .map_err(|e| HostError::new(e.to_string()))?;
                let id = message["sessionID"]
                    .as_str()
                    .ok_or_else(|| HostError::new("missing native session identifier"))?;
                let views = self.eligible_views();
                let first = views
                    .first()
                    .ok_or_else(|| HostError::new("no eligible native views remain"))?;
                let session = Rc::new(native::Session::new(
                    self,
                    &first.1,
                    id,
                    message["capabilities"]["acceptInsecureCerts"]
                        .as_bool()
                        .unwrap_or(false),
                )?);
                self.data.borrow_mut().session = Some(session);
                for (target, view) in views {
                    self.register(target, &view);
                }
                self.send(json!({"event":"StartAutomationSession_Return", "message":json!({"browserName":"ExplorerKit", "browserVersion":native::version()}).to_string()}));
                self.send(json!({"event":"SetTargetList", "connectionID":1, "message":json!([{"type":"automation", "name":id,"targetID":1,"isPaired":false}]).to_string()}));
            }
            Some("Setup") => {
                let session = self
                    .data
                    .borrow()
                    .session
                    .clone()
                    .ok_or_else(|| HostError::new("native Setup preceded session creation"))?;
                session.pair(self);
                self.data.borrow_mut().status = WebKitAutomationStatus::Active {
                    session_id: session.id.clone(),
                };
                self.wake();
            }
            Some("SendMessageToBackend") => {
                let original = event["message"]
                    .as_str()
                    .ok_or_else(|| HostError::new("missing native message"))?
                    .to_owned();
                if let Some(prefer_new_tab) = creation_intent(&original) {
                    let born_after = self.callbacks.borrow().lifetime_sequence;
                    let mut data = self.data.borrow_mut();
                    data.sequence += 1;
                    let id = WebKitAutomationNewViewRequestId {
                        host: self.identity,
                        epoch: data.epoch,
                        sequence: data.sequence,
                    };
                    data.pending.insert(
                        id,
                        PendingCreate {
                            original,
                            born_after,
                        },
                    );
                    data.requests
                        .push_back(WebKitAutomationNewViewRequest { id, prefer_new_tab });
                    drop(data);
                    self.wake();
                } else {
                    self.dispatch(&original, None);
                }
            }
            Some("FrontendDidClose") => self.disconnect(None),
            _ => return Err(HostError::new("unknown native automation transport event")),
        }
        Ok(())
    }
    fn dispatch(&self, original: &str, candidate: Option<&WKWebView>) -> bool {
        #[cfg(test)]
        if let Some(witness) = self.dispatch_witness.borrow().clone() {
            witness();
        }
        let session = self.data.borrow().session.clone();
        let Some(session) = session else {
            return false;
        };
        self.dispatching.set(true);
        let consumed = session.dispatch(original, candidate);
        self.dispatching.set(false);
        consumed
    }
    fn resolve(
        self: &Rc<Self>,
        request: WebKitAutomationNewViewRequestId,
        webview: Option<WebViewHandle>,
    ) -> Result<(), HostError> {
        let born_after = self
            .data
            .borrow()
            .pending
            .get(&request)
            .ok_or_else(|| {
                HostError::new("automation creation request is stale or already resolved")
            })?
            .born_after;
        if self.retirement_pending.get() {
            return Err(HostError::new(
                "automation creation is waiting for native view retirement; retry after a Runtime update",
            ));
        }
        let candidate = if let Some(handle) = webview {
            let target = self.target(handle)?;
            let registry = self.callbacks.borrow();
            let view = &registry.views[&target].automation;
            if view.born <= born_after || view.claimed || !view.attached || !view.eligible {
                return Err(HostError::new(
                    "creation candidate must be fresh, attached, eligible and unclaimed",
                ));
            }
            let native = view
                .native
                .as_ref()
                .and_then(NativeWeak::load)
                .ok_or_else(|| HostError::new("creation candidate has no live native view"))?;
            let session = self
                .data
                .borrow()
                .session
                .clone()
                .ok_or_else(|| HostError::new("automation creation session ended"))?;
            if !session.same_pool(&native) {
                return Err(HostError::new(
                    "creation candidate belongs to a different native pool",
                ));
            }
            Some((target, native))
        } else {
            None
        };
        // Adoption boundary: everything fallible and local is complete. All
        // subsequent outcomes settle ownership internally and return Ok.
        let pending = self.data.borrow_mut().pending.remove(&request).unwrap();
        if let Some((target, _)) = &candidate {
            self.callbacks
                .borrow_mut()
                .views
                .get_mut(target)
                .unwrap()
                .automation
                .claimed = true;
        }
        let consumed = self.dispatch(
            &pending.original,
            candidate.as_ref().map(|(_, native)| &**native),
        );
        if let Some((_, view)) = candidate.filter(|_| !consumed) {
            self.retirement_pending.set(true);
            native::retire(&view);
        }
        self.wake();
        Ok(())
    }
}

/// Intent only. Do not duplicate the engine's option or parameter validation.
fn creation_intent(message: &str) -> Option<bool> {
    let message: Value = serde_json::from_str(message).ok()?;
    match message["method"].as_str()? {
        "Automation.createBrowsingContext" => {
            Some(message["params"]["preferNewTab"].as_bool().unwrap_or(false))
        }
        "Automation.processBidiMessage" => {
            let inner: Value = serde_json::from_str(message["params"]["message"].as_str()?).ok()?;
            (inner["method"] == "browsingContext.create").then(|| inner["params"]["type"] == "tab")
        }
        _ => None,
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    fn state() -> Rc<State> {
        State::new(
            Rc::new(RefCell::new(SystemCallbacks::new(std::sync::Arc::new(
                || {},
            )))),
            None,
        )
    }

    // This checks owning-layer ordering with seeded connection state. The public
    // native suite separately exercises a real session/transport during shutdown.
    pub(in crate::webview::macos) fn pending_shutdown_probe(owner: &Rc<State>) -> impl Fn() {
        let id = WebKitAutomationNewViewRequestId {
            host: owner.identity,
            epoch: 0,
            sequence: 1,
        };
        owner.data.borrow_mut().status = WebKitAutomationStatus::Active {
            session_id: "ordering-probe".into(),
        };
        owner.data.borrow_mut().pending.insert(
            id,
            PendingCreate {
                original: "not dispatched".into(),
                born_after: 0,
            },
        );
        let owner = Rc::downgrade(owner);
        move || {
            let owner = owner.upgrade().unwrap();
            let data = owner
                .data
                .try_borrow()
                .expect("no state guard crosses native owner retirement");
            assert_eq!(data.status, WebKitAutomationStatus::Disconnected);
            assert!(data.session.is_none());
            assert!(data.transport.is_none());
            assert!(data.pending.is_empty());
            drop(data);
            assert!(owner.resolve(id, None).is_err());
        }
    }

    pub(in crate::webview::macos) fn resolve_probe(
        owner: &Rc<State>,
        token: WebKitAutomationNewViewRequestId,
        view: Option<WebViewHandle>,
    ) -> Result<(), HostError> {
        owner.resolve(token, view)
    }

    pub(in crate::webview::macos) fn seed_retirement_probe(
        owner: &Rc<State>,
        events: Rc<RefCell<Vec<&'static str>>>,
    ) -> WebKitAutomationNewViewRequestId {
        let id = WebKitAutomationNewViewRequestId {
            host: owner.identity,
            epoch: 0,
            sequence: 1,
        };
        owner.data.borrow_mut().pending.insert(
            id,
            PendingCreate {
                original: "unchanged creation bytes".into(),
                born_after: 0,
            },
        );
        owner.data.borrow_mut().incoming.push_back(json!({"event":"SendMessageToBackend","message":r#"{"method":"Automation.getBrowsingContexts"}"#}));
        *owner.dispatch_witness.borrow_mut() =
            Some(Rc::new(move || events.borrow_mut().push("dispatch")));
        owner.retirement_pending.set(true);
        id
    }

    #[test]
    fn creation_gate_preserves_native_validation_responsibility() {
        assert_eq!(
            creation_intent(
                r#"{"method":"Automation.createBrowsingContext","params":{"preferNewTab":true,"unknown":1}}"#
            ),
            Some(true)
        );
        assert_eq!(
            creation_intent(
                r#"{"method":"Automation.createBrowsingContext","params":{"preferNewTab":"invalid"}}"#
            ),
            Some(false)
        );
        let nested = json!({"method":"Automation.processBidiMessage", "params":{"message":json!({"method":"browsingContext.create","params":{"type":"tab","userContext":"invalid"}}).to_string()}});
        assert_eq!(creation_intent(&nested.to_string()), Some(true));
        assert_eq!(
            creation_intent(
                r#"{"method":"Automation.processBidiMessage","params":{"message":"invalid JSON"}}"#
            ),
            None
        );
        assert_eq!(
            creation_intent(r#"{"method":"Automation.evaluateJavaScriptFunction"}"#),
            None
        );
    }

    #[test]
    fn new_host_cannot_reuse_old_request_tokens() {
        let first = state();
        let old = WebKitAutomationNewViewRequestId {
            host: first.identity,
            epoch: 1,
            sequence: 1,
        };
        drop(first);
        let second = state();
        let new = WebKitAutomationNewViewRequestId {
            host: second.identity,
            epoch: 1,
            sequence: 1,
        };
        second.data.borrow_mut().pending.insert(
            new,
            PendingCreate {
                original: "original bytes".into(),
                born_after: 0,
            },
        );
        assert_ne!(old, new);
        assert!(second.resolve(old, None).is_err());
        assert!(second.data.borrow().pending.contains_key(&new));
    }

    #[test]
    fn native_teardown_notification_preserves_transport_failure() {
        let owner = state();
        owner.disconnect(Some("stream ended".into()));
        owner.native_disconnected();
        assert_eq!(
            owner.data.borrow().status,
            WebKitAutomationStatus::Failed {
                message: "stream ended".into()
            }
        );
    }

    #[test]
    fn disconnect_invalidates_preparation_without_native_completion() {
        let state = state();
        let id = WebKitAutomationNewViewRequestId {
            host: state.identity,
            epoch: 0,
            sequence: 1,
        };
        state.data.borrow_mut().pending.insert(
            id,
            PendingCreate {
                original: "unforwarded original".into(),
                born_after: 0,
            },
        );
        state
            .data
            .borrow_mut()
            .requests
            .push_back(WebKitAutomationNewViewRequest {
                id,
                prefer_new_tab: true,
            });
        state.disconnect(None);
        assert!(state.data.borrow().pending.is_empty());
        assert!(state.data.borrow().requests.is_empty());
        assert!(state.resolve(id, None).is_err());
        assert_eq!(
            state.data.borrow().status,
            WebKitAutomationStatus::Disconnected
        );
    }
}
