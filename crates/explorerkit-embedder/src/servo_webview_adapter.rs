use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use dpi::PhysicalSize;
use euclid::Scale;

use crate::{
    host_event_from_javascript_evaluation_result, ContextMenuAction, ContextMenuBounds,
    ContextMenuElementInformation, ContextMenuItem, ContextMenuRequest, FaviconImage,
    FaviconPixelFormat, FilePickerRequest, HiddenEmbedderControl, HostEvent, InputMethodKind,
    InputMethodRequest, LoadStatusKind, NewWebViewDecision, NewWebViewRequest, PopupCreated,
    PopupRequest, PopupRequestPolicy, SelectElementRequest, ServoAdapterState, ServoRuntime,
    ServoWebView, SimpleDialogKind, SimpleDialogRequest, WebViewHandle, WebViewRegistrar,
};
use explorerkit_host::{HostSurface, SurfaceSize, SurfaceViewport};
use servo::{
    DeviceIndependentPixel, DevicePixel, InputMethodType, LoadStatus, RenderingContext,
    SimpleDialog, WebView, WebViewDelegate,
};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServoWebViewEvent {
    pub webview_id: String,
    pub event: HostEvent,
}

pub type ManagedChildRenderingContextFactory =
    Rc<dyn Fn(Rc<dyn RenderingContext>) -> Rc<dyn RenderingContext>>;

/// Resources prepared before accepting Servo's original auxiliary-view request.
pub struct PreparedManagedChild {
    pub rendering_context: Rc<dyn RenderingContext>,
    pub delegate: Rc<dyn WebViewDelegate>,
    pub commit: Box<dyn FnOnce(WebView)>,
}

/// Fallible preparation; the returned commit has no recoverable adoption step.
pub type ManagedChildWebViewFactory =
    Rc<dyn Fn(&WebView, &str) -> Result<PreparedManagedChild, String>>;

#[derive(Clone)]
pub struct ServoWebViewAdapter {
    shared: Rc<SharedState>,
    key: String,
}

impl ServoWebViewAdapter {
    pub(crate) fn close(&self) {
        self.shared.cleanup_pending_for_webview(&self.key);
        let removed = self.shared.manager.borrow_mut().views.remove(&self.key);
        drop(removed);
    }

    pub fn new(
        popup_policy: PopupRequestPolicy,
        rendering_context: Rc<dyn RenderingContext>,
    ) -> Self {
        Self::with_factory(
            popup_policy,
            rendering_context,
            Rc::new(|context| context),
            false,
        )
    }

    pub fn with_managed_child_rendering_context_factory(
        popup_policy: PopupRequestPolicy,
        rendering_context: Rc<dyn RenderingContext>,
        factory: ManagedChildRenderingContextFactory,
    ) -> Self {
        Self::with_factory(popup_policy, rendering_context, factory, true)
    }

    fn with_factory(
        popup_policy: PopupRequestPolicy,
        rendering_context: Rc<dyn RenderingContext>,
        factory: ManagedChildRenderingContextFactory,
        explicit: bool,
    ) -> Self {
        let key = "view-0".to_owned();
        Self {
            shared: Rc::new(SharedState::new(
                key.clone(),
                popup_policy,
                rendering_context,
                factory,
                explicit,
            )),
            key,
        }
    }

    pub(crate) fn set_runtime(&self, runtime: ServoRuntime) {
        *self.shared.runtime.borrow_mut() = Some(runtime);
    }

    pub(crate) fn runtime(&self) -> ServoRuntime {
        self.shared
            .runtime
            .borrow()
            .as_ref()
            .expect("native owner has a runtime lease")
            .clone()
    }

    pub(crate) fn native_webview(&self) -> Result<WebView, String> {
        self.shared
            .manager
            .borrow()
            .view(&self.key)?
            .webview
            .clone()
            .ok_or_else(|| "native webview has not been committed".to_owned())
    }

    pub(crate) fn is_live(&self) -> bool {
        self.shared.manager.borrow().views.contains_key(&self.key)
    }

    pub fn webview_delegate(&self) -> Rc<dyn WebViewDelegate> {
        Rc::new(ServoWebViewDelegate {
            shared: Rc::downgrade(&self.shared),
            key: self.key.clone(),
        })
    }

    pub fn register_root_webview(&self, webview: &WebView) {
        if let Some(view) = self.shared.manager.borrow_mut().views.get_mut(&self.key) {
            view.native_id = webview_key(webview);
            view.webview = Some(webview.clone());
        }
    }

    pub(crate) fn bind_runtime(
        &self,
        webview: WebViewHandle,
        registrar: WebViewRegistrar,
        navigation_state_events: bool,
    ) {
        let mut manager = self.shared.manager.borrow_mut();
        let view = manager
            .views
            .get_mut(&self.key)
            .expect("prepared view is live");
        view.registration = Some(RuntimeBinding {
            webview,
            registrar,
            navigation_state_events,
        });
        // Construction can already have emitted events before the facade binds the root.
        let queued = manager.events.drain(..).collect::<Vec<_>>();
        for (key, event) in queued {
            if key == self.key {
                manager.emit(&key, event.event);
            } else {
                manager.events.push_back((key, event));
            }
        }
    }

    #[allow(
        clippy::type_complexity,
        reason = "Keep the native child callback signature explicit."
    )]
    pub(crate) fn set_new_webview_handler(
        &self,
        handler: Option<Rc<dyn Fn(&NewWebViewRequest) -> NewWebViewDecision>>,
    ) -> Result<(), String> {
        let old = {
            let mut manager = self.shared.manager.borrow_mut();
            let view = manager.view_mut(&self.key)?;
            if view.factory.is_none() || view.registration.is_none() {
                return Err("registered native child creation is unavailable".to_owned());
            }
            std::mem::replace(&mut view.handler, handler)
        };
        drop(old);
        Ok(())
    }

    pub(crate) fn set_managed_child_webview_factory(
        &self,
        factory: ManagedChildWebViewFactory,
    ) -> Result<(), String> {
        let mut manager = self.shared.manager.borrow_mut();
        if self.shared.managed_child_rendering_context_is_child_scoped || manager.views.len() != 1 {
            return Err("prepared child creation requires a fresh view without a legacy factory or children".to_owned());
        }
        if manager.legacy_popup_policy != PopupRequestPolicy::DefaultDeny
            || manager.view(&self.key)?.factory.is_some()
        {
            return Err(
                "prepared child creation must be configured once while creation is denied"
                    .to_owned(),
            );
        }
        manager.view_mut(&self.key)?.factory = Some(factory);
        Ok(())
    }

    #[allow(
        clippy::type_complexity,
        reason = "Keep the prepared native child callback signature explicit."
    )]
    pub(crate) fn child_factory(
        &self,
        prepare: Rc<
            dyn Fn(
                ServoWebView,
                WebViewHandle,
                &WebView,
                &str,
            ) -> Result<PreparedManagedChild, String>,
        >,
    ) -> ManagedChildWebViewFactory {
        let owner = Rc::downgrade(&self.shared);
        Rc::new(move |parent, request_id| {
            let shared = owner
                .upgrade()
                .ok_or_else(|| "native child owner has been dropped".to_owned())?;
            let (source, key) = {
                let mut manager = shared.manager.borrow_mut();
                let source = manager
                    .views
                    .values()
                    .find(|view| view.native_id == webview_key(parent))
                    .and_then(|view| view.registration.as_ref())
                    .map(|binding| binding.webview)
                    .ok_or_else(|| {
                        "native child source is not registered with a Runtime".to_owned()
                    })?;
                (source, manager.reserve_child(&webview_key(parent))?)
            };
            let child = ServoWebView::from_adapter(Self { shared, key });
            prepare(child, source, parent, request_id)
        })
    }

    pub(crate) fn set_rendering_context(&self, context: Rc<dyn RenderingContext>) {
        self.shared
            .manager
            .borrow_mut()
            .view_mut(&self.key)
            .expect("prepared child is live")
            .rendering_context = context;
    }

    pub fn request_paint(&self) {
        self.shared
            .manager
            .borrow_mut()
            .update_state_for_webview(&self.key, ServoAdapterState::request_paint);
    }

    pub fn take_needs_paint(&self) -> bool {
        self.shared
            .manager
            .borrow_mut()
            .views
            .get_mut(&self.key)
            .is_some_and(|view| view.state.take_needs_paint())
    }

    pub fn drain_events(&self) -> Vec<HostEvent> {
        self.drain_webview_events()
            .into_iter()
            .map(|event| event.event)
            .collect()
    }

    pub fn drain_webview_events(&self) -> Vec<ServoWebViewEvent> {
        self.shared.manager.borrow_mut().drain_events()
    }

    pub fn set_popup_policy(&self, popup_policy: PopupRequestPolicy) {
        let old = {
            let mut manager = self.shared.manager.borrow_mut();
            let Ok(view) = manager.view_mut(&self.key) else {
                return;
            };
            if view.factory.is_none() {
                manager.legacy_popup_policy = popup_policy;
                None
            } else if popup_policy == PopupRequestPolicy::DefaultDeny {
                view.handler.take()
            } else {
                None
            }
        };
        drop(old);
    }

    pub fn managed_child_webview_ids(&self) -> Vec<String> {
        self.shared.manager.borrow().managed_child_webview_ids()
    }

    pub fn opener_webview_id(&self, child_webview_id: &str) -> Option<String> {
        self.shared
            .manager
            .borrow()
            .child(child_webview_id)
            .ok()
            .and_then(|view| view.parent_webview_id.clone())
    }

    pub fn destroy_managed_child_webview(&self, child_webview_id: &str) -> Result<(), String> {
        let key = self.shared.manager.borrow().child_key(child_webview_id)?;
        self.shared.cleanup_pending_for_webview(&key);
        let removed = self.shared.manager.borrow_mut().views.remove(&key);
        drop(removed);
        Ok(())
    }

    pub fn attach_managed_child_surface(
        &self,
        child_webview_id: &str,
        surface: HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<(), String> {
        let child = self
            .shared
            .manager
            .borrow_mut()
            .attach_managed_child_surface(child_webview_id, surface, viewport)?;
        child.set_hidpi_scale_factor(servo_hidpi_scale_factor(viewport.scale_factor));
        child.resize(servo_physical_size(viewport.size));
        Ok(())
    }

    pub fn update_managed_child_surface_viewport(
        &self,
        child_webview_id: &str,
        viewport: SurfaceViewport,
    ) -> Result<(), String> {
        let child = self
            .shared
            .manager
            .borrow_mut()
            .update_managed_child_surface_viewport(child_webview_id, viewport)?;
        child.set_hidpi_scale_factor(servo_hidpi_scale_factor(viewport.scale_factor));
        child.resize(servo_physical_size(viewport.size));
        Ok(())
    }

    pub fn detach_managed_child_surface(&self, child_webview_id: &str) -> Result<(), String> {
        self.shared
            .manager
            .borrow_mut()
            .detach_managed_child_surface(child_webview_id)
    }

    pub fn managed_child_surface_viewport(
        &self,
        child_webview_id: &str,
    ) -> Option<SurfaceViewport> {
        self.shared
            .manager
            .borrow()
            .child(child_webview_id)
            .ok()
            .and_then(|view| view.surface.as_ref())
            .map(|surface| surface.viewport)
    }

    pub fn managed_child_surface_attached(&self, child_webview_id: &str) -> Result<bool, String> {
        Ok(self
            .shared
            .manager
            .borrow()
            .child(child_webview_id)?
            .surface
            .is_some())
    }

    pub fn take_managed_child_needs_paint(&self, child_webview_id: &str) -> Result<bool, String> {
        let mut manager = self.shared.manager.borrow_mut();
        let key = manager.child_key(child_webview_id)?;
        Ok(manager.view_mut(&key)?.state.take_needs_paint())
    }

    pub fn paint_managed_child(&self, child_webview_id: &str) -> Result<(), String> {
        let child = self
            .shared
            .manager
            .borrow()
            .child(child_webview_id)?
            .webview
            .clone()
            .expect("committed native child");
        child.paint();
        Ok(())
    }

    pub fn ensure_managed_child_webview(&self, child_webview_id: &str) -> Result<(), String> {
        self.shared
            .manager
            .borrow()
            .child(child_webview_id)
            .map(|_| ())
    }

    pub(crate) fn evaluate_javascript(
        &self,
        evaluation_id: &str,
        script: &str,
    ) -> Result<(), String> {
        let native = self.native_webview()?;
        let token = self
            .shared
            .manager
            .borrow_mut()
            .view_mut(&self.key)?
            .evaluations
            .submit(evaluation_id)?;
        let owner = Rc::downgrade(&self.shared);
        let key = self.key.clone();
        let evaluation_id = evaluation_id.to_owned();
        native.evaluate_javascript(script, move |result| {
            let Some(owner) = owner.upgrade() else { return };
            let mut manager = owner.manager.borrow_mut();
            let Some(view) = manager.views.get_mut(&key) else {
                return;
            };
            if !view.evaluations.complete(&evaluation_id, token) {
                return;
            }
            manager.emit(
                &key,
                host_event_from_javascript_evaluation_result(evaluation_id, result),
            );
        });
        Ok(())
    }

    pub fn notify_javascript_evaluation_result(
        &self,
        evaluation_id: String,
        result: Result<servo::JSValue, servo::JavaScriptEvaluationError>,
    ) {
        self.shared.manager.borrow_mut().emit(
            &self.key,
            host_event_from_javascript_evaluation_result(evaluation_id, result),
        );
    }

    pub fn resolve_select_element(
        &self,
        select_element_id: &str,
        selected_options: Vec<usize>,
    ) -> Result<(), String> {
        let mut pending_select_element = self.take_pending(
            &self.shared.pending_select_elements,
            select_element_id,
            "select element",
        )?;
        self.shared
            .manager
            .borrow_mut()
            .with_state_for_webview(&pending_select_element.webview_id, |state| {
                state.resolve_select_element(select_element_id)
            })?;

        pending_select_element
            .select_element
            .select(selected_options);
        pending_select_element.select_element.submit();

        Ok(())
    }

    pub fn resolve_file_picker(
        &self,
        file_picker_id: &str,
        selected_paths: Vec<String>,
    ) -> Result<(), String> {
        let mut pending_file_picker = self.take_pending(
            &self.shared.pending_file_pickers,
            file_picker_id,
            "file picker",
        )?;
        self.shared
            .manager
            .borrow_mut()
            .with_state_for_webview(&pending_file_picker.webview_id, |state| {
                state.resolve_file_picker(file_picker_id)
            })?;

        let selected_paths = selected_paths
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        pending_file_picker.file_picker.select(&selected_paths);
        pending_file_picker.file_picker.submit();

        Ok(())
    }

    pub fn dismiss_file_picker(&self, file_picker_id: &str) -> Result<(), String> {
        let pending_file_picker = self.take_pending(
            &self.shared.pending_file_pickers,
            file_picker_id,
            "file picker",
        )?;
        self.shared
            .manager
            .borrow_mut()
            .with_state_for_webview(&pending_file_picker.webview_id, |state| {
                state.resolve_file_picker(file_picker_id)
            })?;

        pending_file_picker.file_picker.dismiss();

        Ok(())
    }

    pub fn resolve_context_menu(
        &self,
        context_menu_id: &str,
        action: ContextMenuAction,
    ) -> Result<(), String> {
        let pending_context_menu = self.take_pending(
            &self.shared.pending_context_menus,
            context_menu_id,
            "context menu",
        )?;
        self.shared
            .manager
            .borrow_mut()
            .with_state_for_webview(&pending_context_menu.webview_id, |state| {
                state.resolve_context_menu(context_menu_id)
            })?;

        pending_context_menu
            .context_menu
            .select(convert_context_menu_action(action));

        Ok(())
    }

    pub fn dismiss_context_menu(&self, context_menu_id: &str) -> Result<(), String> {
        let pending_context_menu = self.take_pending(
            &self.shared.pending_context_menus,
            context_menu_id,
            "context menu",
        )?;
        self.shared
            .manager
            .borrow_mut()
            .with_state_for_webview(&pending_context_menu.webview_id, |state| {
                state.resolve_context_menu(context_menu_id)
            })?;

        pending_context_menu.context_menu.dismiss();

        Ok(())
    }

    pub fn has_pending_permission_request(&self) -> bool {
        self.pending_permission_request().is_some()
    }

    pub fn pending_permission_request(&self) -> Option<(String, String)> {
        let key = self.pending_permission_key()?;
        self.shared
            .manager
            .borrow()
            .pending_permission_request(&key)
            .map(|request| (request.permission.clone(), request.origin.clone()))
    }

    pub fn resolve_permission(&self, allow: bool) -> Result<(), String> {
        let key = self
            .pending_permission_key()
            .ok_or_else(|| "permission request is no longer pending".to_owned())?;
        let request = self
            .shared
            .pending_permission_requests
            .borrow_mut()
            .remove(&key)
            .ok_or_else(|| "permission request is no longer pending".to_owned())?;
        self.shared
            .manager
            .borrow_mut()
            .resolve_permission(&request.webview_id)?;

        if allow {
            request.request.allow();
        } else {
            request.request.deny();
        }

        Ok(())
    }

    pub fn resolve_navigation_request(
        &self,
        navigation_id: &str,
        allow: bool,
    ) -> Result<(), String> {
        let navigation_request = self.take_pending(
            &self.shared.pending_navigation_requests,
            navigation_id,
            "navigation request",
        )?;
        self.shared
            .manager
            .borrow_mut()
            .resolve_navigation_request(&navigation_request.webview_id, navigation_id)?;

        if allow {
            navigation_request.request.allow();
        } else {
            navigation_request.request.deny();
        }

        Ok(())
    }

    pub fn resolve_simple_dialog(
        &self,
        dialog_id: &str,
        confirmed: bool,
        prompt_value: Option<&str>,
    ) -> Result<(), String> {
        let pending_dialog = self.take_pending(
            &self.shared.pending_simple_dialogs,
            dialog_id,
            "simple dialog",
        )?;
        self.shared
            .manager
            .borrow_mut()
            .with_state_for_webview(&pending_dialog.webview_id, |state| {
                state.resolve_simple_dialog(dialog_id)
            })?;

        match pending_dialog.dialog {
            SimpleDialog::Alert(alert_dialog) => alert_dialog.confirm(),
            SimpleDialog::Confirm(confirm_dialog) => {
                if confirmed {
                    confirm_dialog.confirm();
                } else {
                    confirm_dialog.dismiss();
                }
            }
            SimpleDialog::Prompt(mut prompt_dialog) => {
                if confirmed {
                    if let Some(prompt_value) = prompt_value {
                        prompt_dialog.set_current_value(prompt_value);
                    }
                    prompt_dialog.confirm();
                } else {
                    prompt_dialog.dismiss();
                }
            }
        }

        Ok(())
    }

    fn take_pending<T: PendingWebViewScoped>(
        &self,
        pending: &RefCell<HashMap<String, T>>,
        id: &str,
        kind: &str,
    ) -> Result<T, String> {
        let manager = self.shared.manager.borrow();
        let view = manager.view(&self.key)?;
        // Standalone legacy children are controlled through their sole root wrapper.
        let scope = view.registration.as_ref().map(|_| self.key.as_str());
        let mut pending = pending.borrow_mut();
        let request = pending
            .get(id)
            .ok_or_else(|| format!("{kind} {id} is no longer pending"))?;
        if scope.is_some_and(|key| request.webview_id() != key) {
            return Err(format!("{kind} {id} is not pending on this view"));
        }
        Ok(pending.remove(id).expect("validated pending response"))
    }

    fn pending_permission_key(&self) -> Option<String> {
        let manager = self.shared.manager.borrow();
        let view = manager.view(&self.key).ok()?;
        let pending = self.shared.pending_permission_requests.borrow();
        if view.registration.is_some() {
            pending.contains_key(&self.key).then(|| self.key.clone())
        } else {
            pending.keys().next().cloned()
        }
    }
}

struct SharedState {
    manager: RefCell<WebViewManager>,
    runtime: RefCell<Option<ServoRuntime>>,
    managed_child_rendering_context_factory: ManagedChildRenderingContextFactory,
    managed_child_rendering_context_is_child_scoped: bool,
    pending_navigation_requests: RefCell<HashMap<String, PendingNavigationRequest>>,
    pending_simple_dialogs: RefCell<HashMap<String, PendingSimpleDialog>>,
    pending_select_elements: RefCell<HashMap<String, PendingSelectElement>>,
    pending_context_menus: RefCell<HashMap<String, PendingContextMenu>>,
    pending_file_pickers: RefCell<HashMap<String, PendingFilePicker>>,
    pending_permission_requests: RefCell<HashMap<String, PendingPermissionRequest>>,
}

impl SharedState {
    fn new(
        key: String,
        popup_policy: PopupRequestPolicy,
        rendering_context: Rc<dyn RenderingContext>,
        factory: ManagedChildRenderingContextFactory,
        explicit: bool,
    ) -> Self {
        Self {
            manager: RefCell::new(WebViewManager::new(key, popup_policy, rendering_context)),
            runtime: RefCell::default(),
            managed_child_rendering_context_factory: factory,
            managed_child_rendering_context_is_child_scoped: explicit,
            pending_navigation_requests: RefCell::default(),
            pending_simple_dialogs: RefCell::default(),
            pending_select_elements: RefCell::default(),
            pending_context_menus: RefCell::default(),
            pending_file_pickers: RefCell::default(),
            pending_permission_requests: RefCell::default(),
        }
    }

    fn notify_closed(&self, key: &str) {
        self.manager.borrow_mut().emit(key, HostEvent::Closed);
        self.cleanup_pending_for_webview(key);
        let removed = self.manager.borrow_mut().views.remove(key);
        // The callback's WebView argument keeps the native view alive until it unwinds.
        drop(removed);
    }

    fn cleanup_pending_for_webview(&self, webview_id: &str) {
        // Managed child handles may be destroyed by the host or closed by Servo while
        // request-style callbacks are still pending. Clear ExplorerKit state and answer
        // retained Servo requests synchronously here so stale child controls cannot be
        // resolved later against a removed child. Servo's SelectElement has no dismiss
        // API; dropping it sends Servo's default current-selection response.
        for (navigation_id, navigation_request) in
            self.remove_pending_navigation_requests_for_webview(webview_id)
        {
            let _ = self
                .manager
                .borrow_mut()
                .resolve_navigation_request(&navigation_request.webview_id, &navigation_id);
            navigation_request.request.deny();
        }

        if let Some(permission_request) =
            self.remove_pending_permission_request_for_webview(webview_id)
        {
            let _ = self
                .manager
                .borrow_mut()
                .resolve_permission(&permission_request.webview_id);
            permission_request.request.deny();
        }

        for (dialog_id, simple_dialog) in self.remove_pending_simple_dialogs_for_webview(webview_id)
        {
            let _ = self
                .manager
                .borrow_mut()
                .with_state_for_webview(&simple_dialog.webview_id, |state| {
                    state.resolve_simple_dialog(&dialog_id)
                });
            simple_dialog.dialog.dismiss();
        }

        for (select_element_id, select_element) in
            self.remove_pending_select_elements_for_webview(webview_id)
        {
            let _ = self
                .manager
                .borrow_mut()
                .with_state_for_webview(&select_element.webview_id, |state| {
                    state.resolve_select_element(&select_element_id)
                });
            drop(select_element.select_element);
        }

        for (context_menu_id, context_menu) in
            self.remove_pending_context_menus_for_webview(webview_id)
        {
            let _ = self
                .manager
                .borrow_mut()
                .with_state_for_webview(&context_menu.webview_id, |state| {
                    state.resolve_context_menu(&context_menu_id)
                });
            context_menu.context_menu.dismiss();
        }

        for (file_picker_id, file_picker) in
            self.remove_pending_file_pickers_for_webview(webview_id)
        {
            let _ = self
                .manager
                .borrow_mut()
                .with_state_for_webview(&file_picker.webview_id, |state| {
                    state.resolve_file_picker(&file_picker_id)
                });
            file_picker.file_picker.dismiss();
        }
    }

    fn remove_pending_navigation_requests_for_webview(
        &self,
        webview_id: &str,
    ) -> Vec<(String, PendingNavigationRequest)> {
        let ids = pending_ids_for_webview(&self.pending_navigation_requests.borrow(), webview_id);
        remove_pending_by_ids(&mut *self.pending_navigation_requests.borrow_mut(), ids)
    }

    fn remove_pending_permission_request_for_webview(
        &self,
        webview_id: &str,
    ) -> Option<PendingPermissionRequest> {
        self.pending_permission_requests
            .borrow_mut()
            .remove(webview_id)
    }

    fn remove_pending_simple_dialogs_for_webview(
        &self,
        webview_id: &str,
    ) -> Vec<(String, PendingSimpleDialog)> {
        let ids = pending_ids_for_webview(&self.pending_simple_dialogs.borrow(), webview_id);
        remove_pending_by_ids(&mut *self.pending_simple_dialogs.borrow_mut(), ids)
    }

    fn remove_pending_select_elements_for_webview(
        &self,
        webview_id: &str,
    ) -> Vec<(String, PendingSelectElement)> {
        let ids = pending_ids_for_webview(&self.pending_select_elements.borrow(), webview_id);
        remove_pending_by_ids(&mut *self.pending_select_elements.borrow_mut(), ids)
    }

    fn remove_pending_context_menus_for_webview(
        &self,
        webview_id: &str,
    ) -> Vec<(String, PendingContextMenu)> {
        let ids = pending_ids_for_webview(&self.pending_context_menus.borrow(), webview_id);
        remove_pending_by_ids(&mut *self.pending_context_menus.borrow_mut(), ids)
    }

    fn remove_pending_file_pickers_for_webview(
        &self,
        webview_id: &str,
    ) -> Vec<(String, PendingFilePicker)> {
        let ids = pending_ids_for_webview(&self.pending_file_pickers.borrow(), webview_id);
        remove_pending_by_ids(&mut *self.pending_file_pickers.borrow_mut(), ids)
    }
}

#[derive(Clone)]
struct RuntimeBinding {
    webview: WebViewHandle,
    registrar: WebViewRegistrar,
    navigation_state_events: bool,
}

#[derive(Default)]
struct PendingEvaluations {
    generation: u64,
    next_submission: u64,
    pending: HashMap<String, (u64, u64)>,
}

impl PendingEvaluations {
    fn submit(&mut self, id: &str) -> Result<(u64, u64), String> {
        if id.is_empty() || self.pending.contains_key(id) {
            return Err(
                "JavaScript evaluation ID must be nonempty and not already pending on this view"
                    .to_owned(),
            );
        }
        self.next_submission = self
            .next_submission
            .checked_add(1)
            .ok_or_else(|| "JavaScript evaluation identity capacity exhausted".to_owned())?;
        let token = (self.generation, self.next_submission);
        self.pending.insert(id.to_owned(), token);
        Ok(token)
    }

    fn complete(&mut self, id: &str, token: (u64, u64)) -> bool {
        if self.pending.get(id) != Some(&token) {
            return false;
        }
        self.pending.remove(id);
        true
    }

    fn retire(&mut self) -> Vec<String> {
        // Submission tokens remain unique even if an observed generation reaches its limit.
        self.generation = self.generation.saturating_add(1);
        let mut ids = self.pending.drain().map(|(id, _)| id).collect::<Vec<_>>();
        ids.sort();
        ids
    }
}

struct ManagedWebView {
    webview: Option<WebView>,
    native_id: String,
    parent_webview_id: Option<String>,
    state: ServoAdapterState,
    rendering_context: Rc<dyn RenderingContext>,
    surface: Option<AttachedManagedChildSurface>,
    surface_adoptable: bool,
    factory: Option<ManagedChildWebViewFactory>,
    #[allow(
        clippy::type_complexity,
        reason = "Keep the native child callback signature explicit."
    )]
    handler: Option<Rc<dyn Fn(&NewWebViewRequest) -> NewWebViewDecision>>,
    registration: Option<RuntimeBinding>,
    next_request: Option<u64>,
    evaluations: PendingEvaluations,
}

impl ManagedWebView {
    fn new(rendering_context: Rc<dyn RenderingContext>) -> Self {
        Self {
            webview: None,
            native_id: String::new(),
            parent_webview_id: None,
            state: ServoAdapterState::default(),
            rendering_context,
            surface: None,
            surface_adoptable: false,
            factory: None,
            handler: None,
            registration: None,
            next_request: Some(1),
            evaluations: PendingEvaluations::default(),
        }
    }

    fn next_request_id(&mut self) -> Option<String> {
        let sequence = self.next_request?;
        self.next_request = sequence.checked_add(1);
        let source = self
            .registration
            .as_ref()
            .map(|binding| binding.webview.raw().to_string())
            .unwrap_or_else(|| self.native_id.clone());
        Some(format!("{source}:{sequence}"))
    }
}

struct WebViewManager {
    views: HashMap<String, ManagedWebView>,
    legacy_popup_policy: PopupRequestPolicy,
    events: VecDeque<(String, ServoWebViewEvent)>,
    next_view_id: u64,
    next_navigation_request_id: u64,
}

impl WebViewManager {
    fn new(
        key: String,
        popup_policy: PopupRequestPolicy,
        rendering_context: Rc<dyn RenderingContext>,
    ) -> Self {
        Self {
            views: HashMap::from([(key, ManagedWebView::new(rendering_context))]),
            legacy_popup_policy: popup_policy,
            events: VecDeque::new(),
            next_view_id: 0,
            next_navigation_request_id: 0,
        }
    }

    fn view(&self, key: &str) -> Result<&ManagedWebView, String> {
        self.views
            .get(key)
            .ok_or_else(|| format!("webview {key} is not registered"))
    }

    fn view_mut(&mut self, key: &str) -> Result<&mut ManagedWebView, String> {
        self.views
            .get_mut(key)
            .ok_or_else(|| format!("webview {key} is not registered"))
    }

    fn child_key(&self, native_id: &str) -> Result<String, String> {
        self.views
            .iter()
            .find(|(_, view)| view.native_id == native_id && view.parent_webview_id.is_some())
            .map(|(key, _)| key.clone())
            .ok_or_else(|| format!("managed child webview {native_id} is not registered"))
    }

    fn child(&self, native_id: &str) -> Result<&ManagedWebView, String> {
        self.view(&self.child_key(native_id)?)
    }

    fn reserve_child(&mut self, parent_id: &str) -> Result<String, String> {
        let parent = self
            .views
            .values()
            .find(|view| view.native_id == parent_id)
            .ok_or_else(|| "native child source is no longer live".to_owned())?;
        let mut child = ManagedWebView::new(parent.rendering_context.clone());
        child.parent_webview_id = Some(parent_id.to_owned());
        child.factory = parent.factory.clone();
        child.handler = parent.handler.clone();
        self.next_view_id = self
            .next_view_id
            .checked_add(1)
            .ok_or_else(|| "native child identity capacity exhausted".to_owned())?;
        let key = format!("view-{}", self.next_view_id);
        self.views.insert(key.clone(), child);
        Ok(key)
    }

    fn current_url(&self, key: &str) -> Option<String> {
        self.views
            .get(key)
            .and_then(|view| view.state.current_url().map(str::to_owned))
    }

    fn managed_child_webview_ids(&self) -> Vec<String> {
        let mut ids = self
            .views
            .values()
            .filter(|view| view.parent_webview_id.is_some() && view.webview.is_some())
            .map(|view| view.native_id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    fn attach_managed_child_surface(
        &mut self,
        native_id: &str,
        surface: HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<WebView, String> {
        let key = self.child_key(native_id)?;
        let child = self.view_mut(&key)?;
        if !child.surface_adoptable {
            return Err(format!(
                "managed child webview {native_id} does not have a child-scoped rendering context"
            ));
        }
        if child.surface.is_some() {
            return Err(format!(
                "managed child webview {native_id} already has an attached surface"
            ));
        }
        child.surface = Some(AttachedManagedChildSurface {
            _surface: surface,
            viewport,
        });
        let native = child.webview.clone().expect("committed native child");
        self.update_state_for_webview(&key, |state| {
            state.notify_surface_attached(viewport.size);
            state.request_paint();
        });
        Ok(native)
    }

    fn update_managed_child_surface_viewport(
        &mut self,
        native_id: &str,
        viewport: SurfaceViewport,
    ) -> Result<WebView, String> {
        let key = self.child_key(native_id)?;
        let child = self.view_mut(&key)?;
        child
            .surface
            .as_mut()
            .ok_or_else(|| {
                format!("managed child webview {native_id} does not have an attached surface")
            })?
            .viewport = viewport;
        let native = child.webview.clone().expect("committed native child");
        self.update_state_for_webview(&key, |state| {
            state.notify_surface_resized(viewport.size);
            state.request_paint();
        });
        Ok(native)
    }

    fn detach_managed_child_surface(&mut self, native_id: &str) -> Result<(), String> {
        let key = self.child_key(native_id)?;
        self.view_mut(&key)?.surface.take().ok_or_else(|| {
            format!("managed child webview {native_id} does not have an attached surface")
        })?;
        self.update_state_for_webview(&key, ServoAdapterState::notify_surface_detached);
        Ok(())
    }

    fn request_popup(
        &mut self,
        key: &str,
        request_id: String,
        parent_webview_id: String,
        parent_url: Option<String>,
    ) {
        self.update_state_for_webview(key, |state| {
            state.request_popup(PopupRequest {
                request_id,
                parent_webview_id,
                parent_url,
                target_url: None,
                window_features: None,
                wants_popup: None,
            })
        });
    }

    fn request_navigation(&mut self, key: &str, url: String) -> String {
        self.next_navigation_request_id = self
            .next_navigation_request_id
            .checked_add(1)
            .expect("native navigation identity capacity exhausted");
        let id = format!("navigation-{}", self.next_navigation_request_id);
        self.update_state_for_webview(key, |state| {
            state.request_navigation_with_id(id.clone(), url);
        });
        id
    }

    fn resolve_navigation_request(&mut self, key: &str, id: &str) -> Result<(), String> {
        self.with_state_for_webview(key, |state| state.resolve_navigation_request(id))
    }

    fn hide_embedder_control(&mut self, key: &str, id: &str) -> HiddenEmbedderControl {
        let mut hidden = HiddenEmbedderControl::Unknown;
        self.update_state_for_webview(key, |state| hidden = state.hide_embedder_control(id));
        hidden
    }

    fn resolve_permission(&mut self, key: &str) -> Result<(), String> {
        self.with_state_for_webview(key, ServoAdapterState::resolve_permission)
    }

    fn pending_permission_request(&self, key: &str) -> Option<crate::PendingPermissionRequestInfo> {
        self.views
            .get(key)
            .and_then(|view| view.state.pending_permission_request())
    }

    fn update_state_for_webview(&mut self, key: &str, update: impl FnOnce(&mut ServoAdapterState)) {
        let Some(view) = self.views.get_mut(key) else {
            return;
        };
        update(&mut view.state);
        let events = view.state.drain_events();
        for event in events {
            self.emit(key, event);
        }
    }

    fn with_state_for_webview(
        &mut self,
        key: &str,
        update: impl FnOnce(&mut ServoAdapterState) -> Result<(), String>,
    ) -> Result<(), String> {
        let result = update(&mut self.view_mut(key)?.state);
        let events = self.view_mut(key)?.state.drain_events();
        for event in events {
            self.emit(key, event);
        }
        result
    }

    fn retire_evaluations(&mut self, key: &str, error: servo::JavaScriptEvaluationError) {
        let Some(view) = self.views.get_mut(key) else {
            return;
        };
        for id in view.evaluations.retire() {
            self.emit(
                key,
                host_event_from_javascript_evaluation_result(id, Err(error.clone())),
            );
        }
    }

    fn emit(&mut self, key: &str, event: HostEvent) {
        let Some(view) = self.views.get(key) else {
            return;
        };
        if let Some(binding) = &view.registration {
            let navigation = match &event {
                HostEvent::HistoryChanged {
                    can_go_back,
                    can_go_forward,
                    ..
                } if binding.navigation_state_events => Some((*can_go_back, *can_go_forward)),
                _ => None,
            };
            binding.registrar.record_event(binding.webview, event);
            if let Some((can_go_back, can_go_forward)) = navigation {
                binding.registrar.record_event(
                    binding.webview,
                    HostEvent::NavigationStateChanged {
                        can_go_back,
                        can_go_forward,
                    },
                );
            }
        } else {
            self.events.push_back((
                key.to_owned(),
                ServoWebViewEvent {
                    webview_id: view.native_id.clone(),
                    event,
                },
            ));
        }
    }

    fn drain_events(&mut self) -> Vec<ServoWebViewEvent> {
        self.events.drain(..).map(|(_, event)| event).collect()
    }
}

struct AttachedManagedChildSurface {
    _surface: HostSurface,
    viewport: SurfaceViewport,
}

trait PendingWebViewScoped {
    fn webview_id(&self) -> &str;
}

struct PendingNavigationRequest {
    webview_id: String,
    request: servo::NavigationRequest,
}

impl PendingWebViewScoped for PendingNavigationRequest {
    fn webview_id(&self) -> &str {
        &self.webview_id
    }
}

struct PendingSimpleDialog {
    webview_id: String,
    dialog: SimpleDialog,
}

impl PendingWebViewScoped for PendingSimpleDialog {
    fn webview_id(&self) -> &str {
        &self.webview_id
    }
}

struct PendingSelectElement {
    webview_id: String,
    select_element: servo::SelectElement,
}

impl PendingWebViewScoped for PendingSelectElement {
    fn webview_id(&self) -> &str {
        &self.webview_id
    }
}

struct PendingContextMenu {
    webview_id: String,
    context_menu: servo::ContextMenu,
}

impl PendingWebViewScoped for PendingContextMenu {
    fn webview_id(&self) -> &str {
        &self.webview_id
    }
}

struct PendingFilePicker {
    webview_id: String,
    file_picker: servo::FilePicker,
}

impl PendingWebViewScoped for PendingFilePicker {
    fn webview_id(&self) -> &str {
        &self.webview_id
    }
}

fn pending_ids_for_webview<T: PendingWebViewScoped>(
    pending: &HashMap<String, T>,
    webview_id: &str,
) -> Vec<String> {
    pending
        .iter()
        .filter(|(_, request)| request.webview_id() == webview_id)
        .map(|(id, _)| id.clone())
        .collect()
}

fn remove_pending_by_ids<T>(
    pending: &mut HashMap<String, T>,
    ids: Vec<String>,
) -> Vec<(String, T)> {
    ids.into_iter()
        .filter_map(|id| pending.remove(&id).map(|request| (id, request)))
        .collect()
}

fn servo_physical_size(size: SurfaceSize) -> PhysicalSize<u32> {
    PhysicalSize::new(size.width, size.height)
}

fn servo_hidpi_scale_factor(density: f32) -> Scale<f32, DeviceIndependentPixel, DevicePixel> {
    Scale::new(density)
}

struct ServoWebViewDelegate {
    shared: Weak<SharedState>,
    key: String,
}

impl ServoWebViewDelegate {
    fn owner(&self, webview: &WebView) -> Option<Rc<SharedState>> {
        let shared = self.shared.upgrade()?;
        {
            let mut manager = shared.manager.borrow_mut();
            let view = manager.views.get_mut(&self.key)?;
            if view.native_id.is_empty() {
                view.native_id = webview_key(webview);
            }
        }
        Some(shared)
    }
}

struct PendingPermissionRequest {
    webview_id: String,
    request: servo::PermissionRequest,
}

impl PendingWebViewScoped for PendingPermissionRequest {
    fn webview_id(&self) -> &str {
        &self.webview_id
    }
}

impl servo::WebViewDelegate for ServoWebViewDelegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        shared
            .manager
            .borrow_mut()
            .update_state_for_webview(&webview_id, |state| state.request_paint());
    }

    fn notify_url_changed(&self, webview: WebView, url: Url) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        shared
            .manager
            .borrow_mut()
            .update_state_for_webview(&webview_id, |state| state.notify_url_changed(url.as_str()));
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        shared
            .manager
            .borrow_mut()
            .update_state_for_webview(&webview_id, |state| state.notify_page_title_changed(title));
    }

    fn notify_favicon_changed(&self, webview: WebView) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        let favicon = webview.favicon().as_deref().map(favicon_image);
        shared
            .manager
            .borrow_mut()
            .update_state_for_webview(&webview_id, |state| state.notify_favicon_changed(favicon));
    }

    fn notify_status_text_changed(&self, webview: WebView, status: Option<String>) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        shared
            .manager
            .borrow_mut()
            .update_state_for_webview(&webview_id, |state| {
                state.notify_status_text_changed(status)
            });
    }

    fn notify_focus_changed(&self, webview: WebView, focused: bool) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        shared
            .manager
            .borrow_mut()
            .update_state_for_webview(&webview_id, |state| state.notify_focus_changed(focused));
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        let status = match status {
            LoadStatus::Started => LoadStatusKind::Started,
            LoadStatus::HeadParsed => LoadStatusKind::HeadParsed,
            LoadStatus::Complete => LoadStatusKind::Complete,
        };
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        let mut manager = shared.manager.borrow_mut();
        if status == LoadStatusKind::Started {
            manager.retire_evaluations(
                &webview_id,
                servo::JavaScriptEvaluationError::DocumentNotFound,
            );
        }
        manager.update_state_for_webview(&webview_id, |state| {
            state.notify_load_status_changed(status)
        });
    }

    fn notify_cursor_changed(&self, webview: WebView, cursor: servo::Cursor) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        shared
            .manager
            .borrow_mut()
            .update_state_for_webview(&webview_id, |state| {
                state.notify_cursor_changed(cursor_value(cursor))
            });
    }

    fn notify_history_changed(&self, webview: WebView, entries: Vec<Url>, current: usize) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        let can_go_back = webview.can_go_back();
        let can_go_forward = webview.can_go_forward();
        let entries = entries.into_iter().map(|entry| entry.to_string()).collect();
        shared
            .manager
            .borrow_mut()
            .update_state_for_webview(&webview_id, |state| {
                state.notify_history_changed(entries, current, can_go_back, can_go_forward)
            });
    }

    fn notify_closed(&self, webview: WebView) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        shared.notify_closed(&webview_id);
    }

    fn notify_fullscreen_state_changed(&self, webview: WebView, is_fullscreen: bool) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        shared
            .manager
            .borrow_mut()
            .update_state_for_webview(&webview_id, |state| {
                state.notify_fullscreen_state_changed(is_fullscreen)
            });
    }

    fn notify_crashed(&self, webview: WebView, reason: String, backtrace: Option<String>) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        let webview_url = webview.url().map(|url| url.to_string());
        shared
            .manager
            .borrow_mut()
            .update_state_for_webview(&webview_id, |state| {
                state.notify_crashed(webview_url.as_deref(), reason, backtrace)
            });
    }

    fn request_navigation(&self, webview: WebView, request: servo::NavigationRequest) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        let url = request.url.to_string();
        let navigation_id = shared
            .manager
            .borrow_mut()
            .request_navigation(&webview_id, url);

        shared.pending_navigation_requests.borrow_mut().insert(
            navigation_id.clone(),
            PendingNavigationRequest {
                webview_id,
                request,
            },
        );
    }

    fn request_create_new(&self, parent: WebView, request: servo::CreateNewWebViewRequest) {
        let Some(shared) = self.owner(&parent) else {
            return;
        };
        let key = &self.key;
        let parent_id = webview_key(&parent);
        let parent_url = parent
            .url()
            .map(|url| url.to_string())
            .or_else(|| shared.manager.borrow().current_url(key));
        let (request_id, factory, handler, registration, policy) = {
            let mut manager = shared.manager.borrow_mut();
            let policy = manager.legacy_popup_policy;
            let Ok(view) = manager.view_mut(key) else {
                return;
            };
            let Some(id) = view.next_request_id() else {
                return;
            };
            (
                id,
                view.factory.clone(),
                view.handler.clone(),
                view.registration.clone(),
                policy,
            )
        };
        if let Some(factory) = factory {
            let decision = match (handler, registration) {
                (Some(handler), Some(binding)) => handler(&NewWebViewRequest {
                    source: binding.webview,
                    request_id: request_id.clone(),
                    parent_url: parent_url.clone(),
                    target_url: None,
                    window_features: None,
                    wants_popup: None,
                }),
                _ => NewWebViewDecision::Deny,
            };
            if decision == NewWebViewDecision::Deny {
                shared
                    .manager
                    .borrow_mut()
                    .request_popup(key, request_id, parent_id, parent_url);
                return;
            }
            let prepared = match factory(&parent, &request_id) {
                Ok(prepared) => prepared,
                Err(error_message) => {
                    shared.manager.borrow_mut().emit(
                        key,
                        HostEvent::PopupCreationFailed {
                            request_id,
                            error_message,
                        },
                    );
                    return;
                }
            };
            let child = request
                .builder(prepared.rendering_context)
                .hidpi_scale_factor(parent.hidpi_scale_factor())
                .clipboard_delegate(parent.clipboard_delegate())
                .delegate(prepared.delegate)
                .build();
            (prepared.commit)(child);
            return;
        }
        match policy {
            PopupRequestPolicy::DefaultDeny => {
                // Do not synthesize target URL/window-feature metadata, and do not retain the
                // request across an async host bridge. Dropping it applies Servo's default-deny
                // response (None) and creates no child WebView.
                shared
                    .manager
                    .borrow_mut()
                    .request_popup(key, request_id, parent_id, parent_url);
            }
            PopupRequestPolicy::ManagedChild => {
                let rendering_context =
                    (shared.managed_child_rendering_context_factory)(parent.rendering_context());
                let reservation = shared.manager.borrow_mut().reserve_child(&parent_id);
                let child_key = match reservation {
                    Ok(key) => key,
                    Err(error_message) => {
                        shared.manager.borrow_mut().emit(
                            key,
                            HostEvent::PopupCreationFailed {
                                request_id,
                                error_message,
                            },
                        );
                        return;
                    }
                };
                let adapter = ServoWebViewAdapter {
                    shared: shared.clone(),
                    key: child_key.clone(),
                };
                {
                    let mut manager = shared.manager.borrow_mut();
                    let child = manager.view_mut(&child_key).expect("reserved native child");
                    child.rendering_context = rendering_context.clone();
                    child.surface_adoptable =
                        shared.managed_child_rendering_context_is_child_scoped;
                }
                let child = request
                    .builder(rendering_context)
                    .hidpi_scale_factor(parent.hidpi_scale_factor())
                    .clipboard_delegate(parent.clipboard_delegate())
                    .delegate(adapter.webview_delegate())
                    .build();
                adapter.register_root_webview(&child);
                shared
                    .manager
                    .borrow_mut()
                    .update_state_for_webview(key, |state| {
                        state.notify_popup_created(PopupCreated {
                            request_id,
                            parent_webview_id: parent_id,
                            child_webview_id: webview_key(&child),
                            parent_url,
                            target_url: None,
                            window_features: None,
                            wants_popup: None,
                        });
                    });
            }
        }
    }

    fn request_permission(&self, webview: WebView, request: servo::PermissionRequest) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        let permission = permission_feature_name(request.feature());
        let origin = permission_request_origin(&webview)
            .or_else(|| shared.manager.borrow().current_url(&webview_id))
            .unwrap_or_default();

        // Legacy callers expose one permission response; registered views each own theirs.
        let registered = shared.manager.borrow().views[&webview_id]
            .registration
            .is_some();
        let old = {
            let mut pending = shared.pending_permission_requests.borrow_mut();
            if registered {
                pending.remove(&webview_id).into_iter().collect::<Vec<_>>()
            } else {
                pending.drain().map(|(_, request)| request).collect()
            }
        };
        for old in old {
            let _ = shared
                .manager
                .borrow_mut()
                .resolve_permission(&old.webview_id);
            old.request.deny();
        }

        shared.pending_permission_requests.borrow_mut().insert(
            webview_id.clone(),
            PendingPermissionRequest {
                webview_id: webview_id.clone(),
                request,
            },
        );
        shared
            .manager
            .borrow_mut()
            .update_state_for_webview(&webview_id, |state| {
                state.request_permission(permission, origin);
            });
    }

    fn show_embedder_control(&self, webview: WebView, embedder_control: servo::EmbedderControl) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        let control_id = embedder_control_key(embedder_control.id());

        match embedder_control {
            servo::EmbedderControl::InputMethod(input_method) => {
                shared
                    .manager
                    .borrow_mut()
                    .update_state_for_webview(&webview_id, |state| {
                        state.show_input_method(InputMethodRequest {
                            input_method_id: control_id,
                            input_method_type: input_method_kind(input_method.input_method_type()),
                            text: input_method.text(),
                            insertion_point: input_method.insertion_point(),
                            multiline: input_method.multiline(),
                            allow_virtual_keyboard: input_method.allow_virtual_keyboard(),
                        });
                    });
            }
            servo::EmbedderControl::SelectElement(select_element) => {
                shared.pending_select_elements.borrow_mut().insert(
                    control_id.clone(),
                    PendingSelectElement {
                        webview_id: webview_id.clone(),
                        select_element,
                    },
                );

                let select_element_ref = shared.pending_select_elements.borrow();
                let select_element = &select_element_ref.get(&control_id).unwrap().select_element;

                let options = select_element
                    .options()
                    .iter()
                    .map(convert_select_element_option)
                    .collect();
                let selected_options = select_element.selected_options();
                let allow_select_multiple = select_element.allow_select_multiple();

                shared
                    .manager
                    .borrow_mut()
                    .update_state_for_webview(&webview_id, |state| {
                        state.show_select_element(SelectElementRequest {
                            select_element_id: control_id,
                            options,
                            selected_options,
                            allow_select_multiple,
                        });
                    });
            }
            servo::EmbedderControl::ContextMenu(context_menu) => {
                let position = context_menu.position();
                let element_info = convert_context_menu_element_info(context_menu.element_info());
                let items = context_menu
                    .items()
                    .iter()
                    .map(convert_context_menu_item)
                    .collect();
                let bounds = ContextMenuBounds::new(
                    position.min.x,
                    position.min.y,
                    position.size().width,
                    position.size().height,
                );

                shared.pending_context_menus.borrow_mut().insert(
                    control_id.clone(),
                    PendingContextMenu {
                        webview_id: webview_id.clone(),
                        context_menu,
                    },
                );

                shared
                    .manager
                    .borrow_mut()
                    .update_state_for_webview(&webview_id, |state| {
                        state.show_context_menu(ContextMenuRequest {
                            context_menu_id: control_id,
                            bounds,
                            element_info,
                            items,
                        });
                    });
            }
            servo::EmbedderControl::FilePicker(file_picker) => {
                shared.pending_file_pickers.borrow_mut().insert(
                    control_id.clone(),
                    PendingFilePicker {
                        webview_id: webview_id.clone(),
                        file_picker,
                    },
                );

                let file_picker_ref = shared.pending_file_pickers.borrow();
                let file_picker = &file_picker_ref.get(&control_id).unwrap().file_picker;
                let current_paths = file_picker
                    .current_paths()
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect();
                let filter_patterns = file_picker
                    .filter_patterns()
                    .iter()
                    .map(|pattern| pattern.0.clone())
                    .collect();
                let allow_select_multiple = file_picker.allow_select_multiple();

                shared
                    .manager
                    .borrow_mut()
                    .update_state_for_webview(&webview_id, |state| {
                        state.show_file_picker(FilePickerRequest {
                            file_picker_id: control_id,
                            current_paths,
                            filter_patterns,
                            allow_select_multiple,
                        });
                    });
            }
            servo::EmbedderControl::SimpleDialog(dialog) => {
                let kind = simple_dialog_kind(&dialog);
                let message = dialog.message().to_owned();
                let default_value = simple_dialog_default_value(&dialog);
                shared.pending_simple_dialogs.borrow_mut().insert(
                    control_id.clone(),
                    PendingSimpleDialog {
                        webview_id: webview_id.clone(),
                        dialog,
                    },
                );
                shared
                    .manager
                    .borrow_mut()
                    .update_state_for_webview(&webview_id, |state| {
                        state.show_simple_dialog(SimpleDialogRequest {
                            dialog_id: control_id,
                            kind,
                            message,
                            default_value,
                        });
                    });
            }
            _ => {}
        }
    }

    fn hide_embedder_control(&self, webview: WebView, control_id: servo::EmbedderControlId) {
        let Some(shared) = self.owner(&webview) else {
            return;
        };
        let webview_id = self.key.clone();
        let control_id = embedder_control_key(control_id);
        let hidden_control = shared
            .manager
            .borrow_mut()
            .hide_embedder_control(&webview_id, &control_id);
        match hidden_control {
            HiddenEmbedderControl::InputMethod => {}
            HiddenEmbedderControl::SelectElement => {
                let removed = shared
                    .pending_select_elements
                    .borrow_mut()
                    .remove(&control_id);
                drop(removed);
            }
            HiddenEmbedderControl::FilePicker => {
                let removed = shared.pending_file_pickers.borrow_mut().remove(&control_id);
                drop(removed);
            }
            HiddenEmbedderControl::ContextMenu => {
                let removed = shared
                    .pending_context_menus
                    .borrow_mut()
                    .remove(&control_id);
                drop(removed);
            }
            HiddenEmbedderControl::SimpleDialog => {
                let removed = shared
                    .pending_simple_dialogs
                    .borrow_mut()
                    .remove(&control_id);
                drop(removed);
            }
            HiddenEmbedderControl::Unknown => {}
        }
    }
}

fn webview_key(webview: &WebView) -> String {
    webview.id().to_string()
}

fn embedder_control_key(control_id: servo::EmbedderControlId) -> String {
    format!(
        "{:?}:{:?}:{:?}",
        control_id.webview_id, control_id.pipeline_id, control_id.index
    )
}

fn simple_dialog_kind(dialog: &SimpleDialog) -> SimpleDialogKind {
    match dialog {
        SimpleDialog::Alert(_) => SimpleDialogKind::Alert,
        SimpleDialog::Confirm(_) => SimpleDialogKind::Confirm,
        SimpleDialog::Prompt(_) => SimpleDialogKind::Prompt,
    }
}

fn simple_dialog_default_value(dialog: &SimpleDialog) -> Option<String> {
    match dialog {
        SimpleDialog::Prompt(prompt_dialog) => Some(prompt_dialog.current_value().to_owned()),
        SimpleDialog::Alert(_) | SimpleDialog::Confirm(_) => None,
    }
}

fn input_method_kind(input_method_type: InputMethodType) -> InputMethodKind {
    match input_method_type {
        InputMethodType::Color => InputMethodKind::Color,
        InputMethodType::Date => InputMethodKind::Date,
        InputMethodType::DatetimeLocal => InputMethodKind::DatetimeLocal,
        InputMethodType::Email => InputMethodKind::Email,
        InputMethodType::Month => InputMethodKind::Month,
        InputMethodType::Number => InputMethodKind::Number,
        InputMethodType::Password => InputMethodKind::Password,
        InputMethodType::Search => InputMethodKind::Search,
        InputMethodType::Tel => InputMethodKind::Tel,
        InputMethodType::Text => InputMethodKind::Text,
        InputMethodType::Time => InputMethodKind::Time,
        InputMethodType::Url => InputMethodKind::Url,
        InputMethodType::Week => InputMethodKind::Week,
    }
}

fn cursor_value(cursor: servo::Cursor) -> &'static str {
    match cursor {
        servo::Cursor::None => "none",
        servo::Cursor::Default => "default",
        servo::Cursor::Pointer => "pointer",
        servo::Cursor::ContextMenu => "context-menu",
        servo::Cursor::Help => "help",
        servo::Cursor::Progress => "progress",
        servo::Cursor::Wait => "wait",
        servo::Cursor::Cell => "cell",
        servo::Cursor::Crosshair => "crosshair",
        servo::Cursor::Text => "text",
        servo::Cursor::VerticalText => "vertical-text",
        servo::Cursor::Alias => "alias",
        servo::Cursor::Copy => "copy",
        servo::Cursor::Move => "move",
        servo::Cursor::NoDrop => "no-drop",
        servo::Cursor::NotAllowed => "not-allowed",
        servo::Cursor::Grab => "grab",
        servo::Cursor::Grabbing => "grabbing",
        servo::Cursor::EResize => "e-resize",
        servo::Cursor::NResize => "n-resize",
        servo::Cursor::NeResize => "ne-resize",
        servo::Cursor::NwResize => "nw-resize",
        servo::Cursor::SResize => "s-resize",
        servo::Cursor::SeResize => "se-resize",
        servo::Cursor::SwResize => "sw-resize",
        servo::Cursor::WResize => "w-resize",
        servo::Cursor::EwResize => "ew-resize",
        servo::Cursor::NsResize => "ns-resize",
        servo::Cursor::NeswResize => "nesw-resize",
        servo::Cursor::NwseResize => "nwse-resize",
        servo::Cursor::ColResize => "col-resize",
        servo::Cursor::RowResize => "row-resize",
        servo::Cursor::AllScroll => "all-scroll",
        servo::Cursor::ZoomIn => "zoom-in",
        servo::Cursor::ZoomOut => "zoom-out",
    }
}

fn favicon_image(image: &servo::Image) -> FaviconImage {
    FaviconImage {
        width: image.width,
        height: image.height,
        format: match image.format {
            servo::PixelFormat::K8 => FaviconPixelFormat::K8,
            servo::PixelFormat::KA8 => FaviconPixelFormat::KA8,
            servo::PixelFormat::RGB8 => FaviconPixelFormat::RGB8,
            servo::PixelFormat::RGBA8 => FaviconPixelFormat::RGBA8,
            servo::PixelFormat::BGRA8 => FaviconPixelFormat::BGRA8,
        },
        bytes: image.data().to_vec(),
    }
}

fn permission_feature_name(permission: servo::PermissionFeature) -> String {
    // Convert PermissionFeature to kebab-case web-facing permission name.
    // Uses Debug formatting with CamelCase->kebab-case normalization.
    // FUTURE: When Servo evolves PermissionFeature, consider switching to explicit match
    // arms with compile-time exhaustiveness checking, or a custom enum mapping layer.
    let raw = format!("{permission:?}");
    let mut normalized = String::with_capacity(raw.len() + 4);
    for (index, ch) in raw.chars().enumerate() {
        if ch.is_uppercase() {
            if index > 0 {
                normalized.push('-');
            }
            normalized.extend(ch.to_lowercase());
        } else {
            normalized.push(ch);
        }
    }
    normalized
}

fn permission_request_origin(webview: &WebView) -> Option<String> {
    let url = webview.url()?;
    let origin = url.origin().unicode_serialization();
    if origin == "null" {
        Some(url.to_string())
    } else {
        Some(origin)
    }
}

fn convert_select_element_option(
    option_or_optgroup: &servo::SelectElementOptionOrOptgroup,
) -> crate::SelectElementOptionOrOptgroup {
    use crate::{SelectElementOption, SelectElementOptionOrOptgroup};
    match option_or_optgroup {
        servo::SelectElementOptionOrOptgroup::Option(option) => {
            SelectElementOptionOrOptgroup::Option(SelectElementOption {
                id: option.id,
                label: option.label.clone(),
                is_disabled: option.is_disabled,
            })
        }
        servo::SelectElementOptionOrOptgroup::Optgroup { label, options } => {
            SelectElementOptionOrOptgroup::Optgroup {
                label: label.clone(),
                options: options
                    .iter()
                    .map(|opt| SelectElementOption {
                        id: opt.id,
                        label: opt.label.clone(),
                        is_disabled: opt.is_disabled,
                    })
                    .collect(),
            }
        }
    }
}

fn convert_context_menu_item(item: &servo::ContextMenuItem) -> ContextMenuItem {
    match item {
        servo::ContextMenuItem::Item {
            label,
            action,
            enabled,
        } => ContextMenuItem::Item {
            label: label.clone(),
            action: convert_servo_context_menu_action(*action),
            enabled: *enabled,
        },
        servo::ContextMenuItem::Separator => ContextMenuItem::Separator,
    }
}

fn convert_context_menu_element_info(
    element_info: &servo::ContextMenuElementInformation,
) -> ContextMenuElementInformation {
    let flags = element_info.flags;
    let context_type = if flags.contains(servo::ContextMenuElementInformationFlags::Link) {
        "link"
    } else if flags.contains(servo::ContextMenuElementInformationFlags::Image) {
        "image"
    } else if flags.contains(servo::ContextMenuElementInformationFlags::EditableText) {
        "input"
    } else if flags.contains(servo::ContextMenuElementInformationFlags::Selection) {
        "text"
    } else {
        "default"
    };
    ContextMenuElementInformation {
        is_link: flags.contains(servo::ContextMenuElementInformationFlags::Link),
        is_image: flags.contains(servo::ContextMenuElementInformationFlags::Image),
        is_editable_text: flags.contains(servo::ContextMenuElementInformationFlags::EditableText),
        has_selection: flags.contains(servo::ContextMenuElementInformationFlags::Selection),
        link_url: element_info.link_url.as_ref().map(ToString::to_string),
        image_url: element_info.image_url.as_ref().map(ToString::to_string),
        context_type: context_type.to_owned(),
    }
}

fn convert_servo_context_menu_action(action: servo::ContextMenuAction) -> ContextMenuAction {
    match action {
        servo::ContextMenuAction::GoBack => ContextMenuAction::GoBack,
        servo::ContextMenuAction::GoForward => ContextMenuAction::GoForward,
        servo::ContextMenuAction::Reload => ContextMenuAction::Reload,
        servo::ContextMenuAction::CopyLink => ContextMenuAction::CopyLink,
        servo::ContextMenuAction::OpenLinkInNewWebView => ContextMenuAction::OpenLinkInNewWebView,
        servo::ContextMenuAction::CopyImageLink => ContextMenuAction::CopyImageLink,
        servo::ContextMenuAction::OpenImageInNewView => ContextMenuAction::OpenImageInNewView,
        servo::ContextMenuAction::Cut => ContextMenuAction::Cut,
        servo::ContextMenuAction::Copy => ContextMenuAction::Copy,
        servo::ContextMenuAction::Paste => ContextMenuAction::Paste,
        servo::ContextMenuAction::SelectAll => ContextMenuAction::SelectAll,
    }
}

fn convert_context_menu_action(action: ContextMenuAction) -> servo::ContextMenuAction {
    match action {
        ContextMenuAction::GoBack => servo::ContextMenuAction::GoBack,
        ContextMenuAction::GoForward => servo::ContextMenuAction::GoForward,
        ContextMenuAction::Reload => servo::ContextMenuAction::Reload,
        ContextMenuAction::CopyLink => servo::ContextMenuAction::CopyLink,
        ContextMenuAction::OpenLinkInNewWebView => servo::ContextMenuAction::OpenLinkInNewWebView,
        ContextMenuAction::CopyImageLink => servo::ContextMenuAction::CopyImageLink,
        ContextMenuAction::OpenImageInNewView => servo::ContextMenuAction::OpenImageInNewView,
        ContextMenuAction::Cut => servo::ContextMenuAction::Cut,
        ContextMenuAction::Copy => servo::ContextMenuAction::Copy,
        ContextMenuAction::Paste => servo::ContextMenuAction::Paste,
        ContextMenuAction::SelectAll => servo::ContextMenuAction::SelectAll,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Host, HostError, Runtime, SessionHandle};
    use servo::SoftwareRenderingContext;
    use servo_base::generic_channel::GenericSharedMemory;
    use std::cell::Cell;
    use std::sync::Arc;

    #[derive(Default)]
    struct RegistrationHost(Rc<RefCell<Option<WebViewRegistrar>>>);

    impl Host for RegistrationHost {
        fn set_webview_registrar(&mut self, registrar: WebViewRegistrar) {
            *self.0.borrow_mut() = Some(registrar);
        }

        fn create_webview(&mut self, _: SessionHandle, _: WebViewHandle) -> Result<(), HostError> {
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

    fn test_adapter() -> ServoWebViewAdapter {
        ServoWebViewAdapter::new(
            PopupRequestPolicy::DefaultDeny,
            Rc::new(SoftwareRenderingContext::new(PhysicalSize::new(8, 8)).unwrap()),
        )
    }

    fn register(adapter: &ServoWebViewAdapter) -> Runtime<RegistrationHost> {
        let host = RegistrationHost::default();
        let registrar = host.0.clone();
        let mut runtime = Runtime::new(host);
        let session = runtime.create_session();
        let handle = runtime.create_webview(session).unwrap();
        adapter.bind_runtime(handle, registrar.borrow().clone().unwrap(), false);
        runtime
    }

    #[test]
    fn registered_responses_are_scoped_while_legacy_children_use_the_root() {
        struct Response(String);
        impl PendingWebViewScoped for Response {
            fn webview_id(&self) -> &str {
                &self.0
            }
        }
        let adapter = test_adapter();
        let pending = RefCell::new(HashMap::from([(
            "request".into(),
            Response("view-child".into()),
        )]));
        let response = adapter
            .take_pending(&pending, "request", "navigation request")
            .unwrap();
        assert_eq!(response.0, "view-child");
        pending.borrow_mut().insert("request".into(), response);
        let _runtime = register(&adapter);
        assert!(adapter
            .take_pending(&pending, "request", "navigation request")
            .is_err());
        assert!(pending.borrow().contains_key("request"));
        pending.borrow_mut().get_mut("request").unwrap().0 = adapter.key.clone();
        assert!(adapter
            .take_pending(&pending, "request", "navigation request")
            .is_ok());
    }

    #[test]
    fn replacing_handlers_releases_captures_after_the_owner_borrow() {
        struct Capture(Weak<SharedState>, Rc<Cell<usize>>);
        impl Drop for Capture {
            fn drop(&mut self) {
                self.0.upgrade().unwrap().manager.borrow_mut();
                self.1.set(self.1.get() + 1);
            }
        }
        let adapter = test_adapter();
        adapter
            .set_managed_child_webview_factory(Rc::new(|_, _| unreachable!()))
            .unwrap();
        let _runtime = register(&adapter);
        let released = Rc::new(Cell::new(0));
        for clear_by_policy in [false, true] {
            let capture = Capture(Rc::downgrade(&adapter.shared), released.clone());
            adapter
                .set_new_webview_handler(Some(Rc::new(move |_| {
                    let _ = &capture;
                    NewWebViewDecision::Create
                })))
                .unwrap();
            if clear_by_policy {
                adapter.set_popup_policy(PopupRequestPolicy::DefaultDeny);
            } else {
                adapter.set_new_webview_handler(None).unwrap();
            }
        }
        assert_eq!(released.get(), 2);
        let weak = Rc::downgrade(&adapter.shared);
        drop(adapter);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn children_copy_registered_handlers_and_legacy_policy_remains_shared() {
        let adapter = test_adapter();
        adapter
            .shared
            .manager
            .borrow_mut()
            .views
            .get_mut(&adapter.key)
            .unwrap()
            .native_id = "root".into();
        adapter.set_popup_policy(PopupRequestPolicy::ManagedChild);
        let child = adapter
            .shared
            .manager
            .borrow_mut()
            .reserve_child("root")
            .unwrap();
        adapter.set_popup_policy(PopupRequestPolicy::DefaultDeny);
        assert_eq!(
            adapter.shared.manager.borrow().legacy_popup_policy,
            PopupRequestPolicy::DefaultDeny
        );
        adapter.shared.manager.borrow_mut().views.remove(&child);
        adapter
            .set_managed_child_webview_factory(Rc::new(|_, _| unreachable!()))
            .unwrap();
        let _runtime = register(&adapter);
        let handle = adapter.shared.manager.borrow().views[&adapter.key]
            .registration
            .as_ref()
            .unwrap()
            .webview;
        adapter
            .set_new_webview_handler(Some(Rc::new(|_| NewWebViewDecision::Create)))
            .unwrap();
        let child = adapter
            .shared
            .manager
            .borrow_mut()
            .reserve_child("root")
            .unwrap();
        adapter.set_new_webview_handler(None).unwrap();
        let request = NewWebViewRequest {
            source: handle,
            request_id: "child:1".into(),
            parent_url: None,
            target_url: None,
            window_features: None,
            wants_popup: None,
        };
        let handler = adapter.shared.manager.borrow().views[&child]
            .handler
            .clone()
            .unwrap();
        assert_eq!(handler(&request), NewWebViewDecision::Create);
        assert!(adapter.shared.manager.borrow().views[&adapter.key]
            .handler
            .is_none());
    }

    #[test]
    fn evaluation_completion_cannot_consume_a_reused_id_or_a_sibling() {
        let mut first = PendingEvaluations::default();
        let mut sibling = PendingEvaluations::default();
        assert!(first.submit("").is_err());
        let old = first.submit("same").unwrap();
        assert!(first.submit("same").is_err());
        let sibling_token = sibling.submit("same").unwrap();
        assert_eq!(first.retire(), vec!["same"]);
        let current = first.submit("same").unwrap();
        assert!(!first.complete("same", old));
        assert!(first.complete("same", current));
        assert!(!first.complete("same", current));
        assert!(sibling.complete("same", sibling_token));
    }

    #[test]
    fn favicon_copy_preserves_every_servo_format_and_owns_the_bytes() {
        for (servo_format, format) in [
            (servo::PixelFormat::K8, FaviconPixelFormat::K8),
            (servo::PixelFormat::KA8, FaviconPixelFormat::KA8),
            (servo::PixelFormat::RGB8, FaviconPixelFormat::RGB8),
            (servo::PixelFormat::RGBA8, FaviconPixelFormat::RGBA8),
            (servo::PixelFormat::BGRA8, FaviconPixelFormat::BGRA8),
        ] {
            let image = servo::Image::new(
                2,
                3,
                Arc::new(GenericSharedMemory::from_bytes(&[9, 1, 2, 3, 4, 8])),
                1..5,
                servo_format,
            );
            let favicon = favicon_image(&image);
            drop(image);

            assert_eq!(
                favicon,
                FaviconImage {
                    width: 2,
                    height: 3,
                    format,
                    bytes: vec![1, 2, 3, 4],
                }
            );
        }
    }
}
