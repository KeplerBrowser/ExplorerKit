use std::{cell::RefCell, path::PathBuf, rc::Rc};

use dpi::PhysicalSize;
use euclid::Scale;
use servo::{
    ClipboardDelegate, CompositionEvent, CompositionState, DeviceIndependentPixel, DevicePixel,
    DevicePoint, EventLoopWaker, ImeEvent, InputEvent, Key, KeyState, KeyboardEvent, MouseButton,
    MouseButtonAction, MouseButtonEvent, MouseLeftViewportEvent, MouseMoveEvent, NamedKey,
    Preferences, RenderingContext, Servo, ServoBuilder, TouchEvent, TouchEventType, TouchId,
    TouchPointerType, WebView, WebViewBuilder, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
};
use url::Url;

use crate::{
    ContextMenuAction, HostEvent, HostInputEvent, HostSurface, ImeCompositionState,
    KeyboardInputEvent, KeyboardInputKey, KeyboardInputState, KeyboardKey, KeyboardNamedKey,
    ManagedChildRenderingContextFactory, ManagedChildWebViewFactory, NavigationRequest,
    NewWebViewDecision, NewWebViewRequest, PointerButton, PointerButtonAction, PointerInputEvent,
    PointerScrollMode, PopupRequestPolicy, PreparedManagedChild, ServoWebViewAdapter,
    ServoWebViewEvent, SurfaceSize, SurfaceViewport, TouchEventKind, WebViewCommand, WebViewHandle,
    WebViewRegistrar,
};

pub struct ServoWebViewInit {
    pub rendering_context: Rc<dyn RenderingContext>,
    pub clipboard_delegate: Rc<dyn ClipboardDelegate>,
    pub initial_url: Option<String>,
    pub density: f32,
    pub popup_policy: PopupRequestPolicy,
    pub managed_child_rendering_context_factory: Option<ManagedChildRenderingContextFactory>,
}

pub struct ServoWebView {
    adapter: ServoWebViewAdapter,
}

impl ServoWebView {
    /// Creates a standalone view with its own process-runtime lease.
    ///
    /// Hosts embedding multiple independent views use `ServoRuntime::create_webview`
    /// so those views share one owner and wake callback.
    pub fn new(
        init: ServoWebViewInit,
        event_loop_waker: Box<dyn EventLoopWaker>,
    ) -> Result<Self, String> {
        // Reject malformed initial URLs before acquiring the process engine.
        if let Some(url) = &init.initial_url {
            Url::parse(url).map_err(|error| error.to_string())?;
        }
        ServoRuntime::new(event_loop_waker)?.create_webview(init)
    }

    fn with_runtime(init: ServoWebViewInit, runtime: ServoRuntime) -> Result<Self, String> {
        let ServoWebViewInit {
            rendering_context,
            clipboard_delegate,
            initial_url,
            density,
            popup_policy,
            managed_child_rendering_context_factory,
        } = init;
        let initial_url = initial_url
            .as_deref()
            .map(Url::parse)
            .transpose()
            .map_err(|error| error.to_string())?;
        let adapter = if let Some(factory) = managed_child_rendering_context_factory {
            ServoWebViewAdapter::with_managed_child_rendering_context_factory(
                popup_policy,
                rendering_context.clone(),
                factory,
            )
        } else {
            ServoWebViewAdapter::new(popup_policy, rendering_context.clone())
        };
        adapter.set_runtime(runtime.clone());
        let delegate = adapter.webview_delegate();

        let builder = WebViewBuilder::new(runtime.lease.servo(), rendering_context)
            .clipboard_delegate(clipboard_delegate)
            .delegate(delegate)
            .hidpi_scale_factor(hidpi_scale_factor(density));
        let builder = if let Some(initial_url) = initial_url {
            builder.url(initial_url)
        } else {
            builder
        };

        let webview = builder.build();
        adapter.register_root_webview(&webview);

        Ok(Self { adapter })
    }

    pub fn set_hidpi_scale_factor(&mut self, density: f32) {
        if let Ok(webview) = self.adapter.native_webview() {
            webview.set_hidpi_scale_factor(hidpi_scale_factor(density));
        }
    }

    pub fn resize(&mut self, size: SurfaceSize) {
        if let Ok(webview) = self.adapter.native_webview() {
            webview.resize(physical_size(size));
        }
    }

    pub fn request_paint(&self) {
        self.adapter.request_paint();
    }

    /// Drain events for the root/single-surface host compatibility path.
    ///
    /// Managed child events are preserved internally as [`ServoWebViewEvent`] by
    /// [`Self::drain_webview_events`] / [`Self::perform_webview_updates`]. This
    /// Slice B foundation intentionally keeps the existing plain [`HostEvent`] path
    /// for hosts that have not adopted child surfaces yet; adapter ergonomics for
    /// child presentation remain a later slice.
    pub fn drain_events(&self) -> Vec<HostEvent> {
        self.adapter.drain_events()
    }

    pub fn drain_webview_events(&self) -> Vec<ServoWebViewEvent> {
        self.adapter.drain_webview_events()
    }

    pub(crate) fn from_adapter(adapter: ServoWebViewAdapter) -> Self {
        Self { adapter }
    }

    /// Installs prepared native child creation before this view's first native activity.
    ///
    /// Rejects an enabled legacy policy, an explicit legacy renderer factory, or
    /// existing children. The facade uses this seam for Runtime registration.
    pub fn set_managed_child_webview_factory(
        &mut self,
        factory: ManagedChildWebViewFactory,
    ) -> Result<(), String> {
        self.adapter.set_managed_child_webview_factory(factory)
    }

    #[doc(hidden)]
    pub fn __bind_runtime(
        &self,
        webview: WebViewHandle,
        registrar: WebViewRegistrar,
        navigation_state_events: bool,
    ) {
        self.adapter
            .bind_runtime(webview, registrar, navigation_state_events);
    }

    #[doc(hidden)]
    #[allow(
        clippy::type_complexity,
        reason = "Keep the native child callback signature explicit."
    )]
    pub fn __set_new_webview_handler(
        &self,
        handler: Option<Rc<dyn Fn(&NewWebViewRequest) -> NewWebViewDecision>>,
    ) -> Result<(), String> {
        self.adapter.set_new_webview_handler(handler)
    }

    #[doc(hidden)]
    #[allow(
        clippy::type_complexity,
        reason = "Keep the prepared native child callback signature explicit."
    )]
    pub fn __child_factory(
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
        self.adapter.child_factory(prepare)
    }

    #[doc(hidden)]
    pub fn __prepare_child(
        self,
        rendering_context: Rc<dyn RenderingContext>,
        commit: Box<dyn FnOnce(ServoWebView, String)>,
    ) -> PreparedManagedChild {
        self.adapter
            .set_rendering_context(rendering_context.clone());
        PreparedManagedChild {
            rendering_context,
            delegate: self.adapter.webview_delegate(),
            commit: Box::new(move |native| {
                self.adapter.register_root_webview(&native);
                commit(self, native.id().to_string());
            }),
        }
    }

    #[doc(hidden)]
    pub fn __is_live(&self) -> bool {
        self.adapter.is_live()
    }

    pub fn set_popup_policy(&mut self, popup_policy: PopupRequestPolicy) {
        self.adapter.set_popup_policy(popup_policy);
    }

    pub fn managed_child_webview_ids(&self) -> Vec<String> {
        self.adapter.managed_child_webview_ids()
    }

    pub fn opener_webview_id(&self, child_webview_id: &str) -> Option<String> {
        self.adapter.opener_webview_id(child_webview_id)
    }

    pub fn destroy_managed_child_webview(&mut self, child_webview_id: &str) -> Result<(), String> {
        self.adapter.destroy_managed_child_webview(child_webview_id)
    }

    /// Attach a host-owned surface to an existing managed popup child webview.
    ///
    /// This is a narrow child-surface adoption hook: the child is identified by Servo's
    /// managed `webview_id`, and surface lifecycle events remain routed through
    /// [`ServoWebViewEvent::webview_id`]. It is not the final React Native popup UI API.
    pub fn attach_managed_child_surface(
        &mut self,
        child_webview_id: &str,
        surface: HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<(), String> {
        self.adapter
            .attach_managed_child_surface(child_webview_id, surface, viewport)
    }

    pub fn update_managed_child_surface_viewport(
        &mut self,
        child_webview_id: &str,
        viewport: SurfaceViewport,
    ) -> Result<(), String> {
        self.adapter
            .update_managed_child_surface_viewport(child_webview_id, viewport)
    }

    pub fn resize_managed_child_surface(
        &mut self,
        child_webview_id: &str,
        size: SurfaceSize,
    ) -> Result<(), String> {
        let viewport = self
            .adapter
            .managed_child_surface_viewport(child_webview_id)
            .ok_or_else(|| {
                format!(
                    "managed child webview {child_webview_id} does not have an attached surface"
                )
            })?
            .with_size(size);
        self.update_managed_child_surface_viewport(child_webview_id, viewport)
    }

    pub fn detach_managed_child_surface(&mut self, child_webview_id: &str) -> Result<(), String> {
        self.adapter.detach_managed_child_surface(child_webview_id)
    }

    pub fn managed_child_surface_viewport(
        &self,
        child_webview_id: &str,
    ) -> Option<SurfaceViewport> {
        self.adapter
            .managed_child_surface_viewport(child_webview_id)
    }

    pub fn size(&self) -> SurfaceSize {
        let Ok(webview) = self.adapter.native_webview() else {
            return SurfaceSize::new(0, 0);
        };
        let size = webview.size();
        SurfaceSize::new(size.width as u32, size.height as u32)
    }

    pub fn dispatch_command(&mut self, command: WebViewCommand) -> Result<(), String> {
        match command {
            WebViewCommand::LoadUrl(request) => self.load_url_request(request),
            WebViewCommand::Reload => self.reload(),
            WebViewCommand::GoBack => self.go_back(),
            WebViewCommand::GoForward => self.go_forward(),
            WebViewCommand::Focus => self.focus(),
            WebViewCommand::Blur => self.blur(),
        }
    }

    pub fn load_url(&mut self, url: &str) -> Result<(), String> {
        self.load_url_request(NavigationRequest::new(url).map_err(|error| error.to_string())?)
    }

    pub fn load_url_request(&mut self, request: NavigationRequest) -> Result<(), String> {
        let parsed_url = Url::parse(&request.url).map_err(|error| error.to_string())?;
        self.adapter.native_webview()?.load(parsed_url);
        Ok(())
    }

    pub fn reload(&mut self) -> Result<(), String> {
        self.adapter.native_webview()?.reload();
        Ok(())
    }

    pub fn go_back(&mut self) -> Result<(), String> {
        self.adapter.native_webview()?.go_back(1);
        Ok(())
    }

    pub fn go_forward(&mut self) -> Result<(), String> {
        self.adapter.native_webview()?.go_forward(1);
        Ok(())
    }

    pub fn focus(&mut self) -> Result<(), String> {
        self.adapter.native_webview()?.focus();
        Ok(())
    }

    pub fn blur(&mut self) -> Result<(), String> {
        self.adapter.native_webview()?.blur();
        Ok(())
    }

    pub fn evaluate_javascript(&mut self, evaluation_id: &str, script: &str) -> Result<(), String> {
        self.adapter.evaluate_javascript(evaluation_id, script)
    }

    pub fn dispatch_touch_event(
        &mut self,
        event_kind: TouchEventKind,
        touch_id: i32,
        x: f32,
        y: f32,
    ) -> Result<(), String> {
        let point = WebViewPoint::Device(DevicePoint::new(x, y));
        let event_type = match event_kind {
            TouchEventKind::Down => TouchEventType::Down,
            TouchEventKind::Move => TouchEventType::Move,
            TouchEventKind::Up => TouchEventType::Up,
            TouchEventKind::Cancel => TouchEventType::Cancel,
        };
        self.adapter
            .native_webview()?
            .notify_input_event(InputEvent::Touch(TouchEvent::new(
                event_type,
                TouchId(touch_id),
                point,
                TouchPointerType::Touch,
            )));
        Ok(())
    }

    pub fn dispatch_host_input_event(&mut self, event: HostInputEvent) -> Result<(), String> {
        match event {
            HostInputEvent::Pointer(event) => self.dispatch_pointer_input(event),
            HostInputEvent::Keyboard(event) => self.dispatch_keyboard_input(event),
            HostInputEvent::ImeCommit { text } => self.commit_ime_text(&text),
            HostInputEvent::Focus { is_focused } => {
                if is_focused {
                    self.focus()
                } else {
                    self.blur()
                }
            }
        }
    }

    pub fn dispatch_pointer_input(&mut self, event: PointerInputEvent) -> Result<(), String> {
        match event {
            PointerInputEvent::Moved { x, y } => {
                let point = WebViewPoint::Device(DevicePoint::new(x, y));
                self.adapter
                    .native_webview()?
                    .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point)));
            }
            PointerInputEvent::Button {
                action,
                button,
                x,
                y,
            } => {
                let action = match action {
                    PointerButtonAction::Pressed => MouseButtonAction::Down,
                    PointerButtonAction::Released => MouseButtonAction::Up,
                };
                let point = WebViewPoint::Device(DevicePoint::new(x, y));
                self.adapter
                    .native_webview()?
                    .notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                        action,
                        mouse_button(button),
                        point,
                    )));
            }
            PointerInputEvent::Wheel {
                delta_x,
                delta_y,
                mode,
                x,
                y,
            } => {
                let point = WebViewPoint::Device(DevicePoint::new(x, y));
                self.adapter
                    .native_webview()?
                    .notify_input_event(InputEvent::Wheel(WheelEvent::new(
                        WheelDelta {
                            x: delta_x,
                            y: delta_y,
                            z: 0.0,
                            mode: wheel_mode(mode),
                        },
                        point,
                    )));
            }
            PointerInputEvent::LeftViewport => {
                self.adapter
                    .native_webview()?
                    .notify_input_event(InputEvent::MouseLeftViewport(MouseLeftViewportEvent {
                        focus_moving_to_another_iframe: false,
                    }));
            }
        }
        Ok(())
    }

    pub fn dispatch_keyboard_input(&mut self, event: KeyboardInputEvent) -> Result<(), String> {
        self.adapter
            .native_webview()?
            .notify_input_event(InputEvent::Keyboard(KeyboardEvent::new_without_event(
                key_state(event.state),
                keyboard_key(event.key),
                Default::default(),
                Default::default(),
                Default::default(),
                event.repeat,
                event.is_composing,
            )));
        Ok(())
    }

    pub fn commit_ime_text(&mut self, text: &str) -> Result<(), String> {
        self.dispatch_ime_composition(ImeCompositionState::Update, text)?;
        self.dispatch_ime_composition(ImeCompositionState::End, text)
    }

    pub fn trigger_context_menu(&mut self, x: f32, y: f32) -> Result<(), String> {
        let point = WebViewPoint::Device(DevicePoint::new(x, y));
        self.adapter
            .native_webview()?
            .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point)));
        self.adapter
            .native_webview()?
            .notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                MouseButtonAction::Down,
                MouseButton::Secondary,
                point,
            )));
        self.adapter
            .native_webview()?
            .notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                MouseButtonAction::Up,
                MouseButton::Secondary,
                point,
            )));
        Ok(())
    }

    pub fn dispatch_ime_composition(
        &mut self,
        state: ImeCompositionState,
        text: &str,
    ) -> Result<(), String> {
        let state = match state {
            ImeCompositionState::Update => CompositionState::Update,
            ImeCompositionState::End => CompositionState::End,
        };
        self.adapter
            .native_webview()?
            .notify_input_event(InputEvent::Ime(ImeEvent::Composition(CompositionEvent {
                state,
                data: text.to_owned(),
            })));
        Ok(())
    }

    pub fn dismiss_input_method(&mut self) -> Result<(), String> {
        self.adapter
            .native_webview()?
            .notify_input_event(InputEvent::Ime(ImeEvent::Dismissed));
        Ok(())
    }

    pub fn dispatch_keyboard_key(&mut self, key: KeyboardKey) -> Result<(), String> {
        let key = match key {
            KeyboardKey::Backspace => Key::Named(NamedKey::Backspace),
            KeyboardKey::Delete => Key::Named(NamedKey::Delete),
            KeyboardKey::Enter => Key::Named(NamedKey::Enter),
        };
        self.adapter
            .native_webview()?
            .notify_input_event(InputEvent::Keyboard(KeyboardEvent::from_state_and_key(
                KeyState::Down,
                key,
            )));
        Ok(())
    }

    pub fn resolve_select_element(
        &mut self,
        select_element_id: &str,
        selected_options: Vec<usize>,
    ) -> Result<(), String> {
        self.adapter
            .resolve_select_element(select_element_id, selected_options)
    }

    pub fn resolve_file_picker(
        &mut self,
        file_picker_id: &str,
        selected_paths: Vec<String>,
    ) -> Result<(), String> {
        self.adapter
            .resolve_file_picker(file_picker_id, selected_paths)
    }

    pub fn dismiss_file_picker(&mut self, file_picker_id: &str) -> Result<(), String> {
        self.adapter.dismiss_file_picker(file_picker_id)
    }

    pub fn resolve_context_menu(
        &mut self,
        context_menu_id: &str,
        action: ContextMenuAction,
    ) -> Result<(), String> {
        self.adapter.resolve_context_menu(context_menu_id, action)
    }

    pub fn dismiss_context_menu(&mut self, context_menu_id: &str) -> Result<(), String> {
        self.adapter.dismiss_context_menu(context_menu_id)
    }

    pub fn has_pending_permission_request(&self) -> bool {
        self.adapter.has_pending_permission_request()
    }

    pub fn pending_permission_request(&self) -> Option<(String, String)> {
        self.adapter.pending_permission_request()
    }

    pub fn resolve_permission(&mut self, allow: bool) -> Result<(), String> {
        self.adapter.resolve_permission(allow)
    }

    pub fn resolve_navigation_request(
        &mut self,
        navigation_id: &str,
        allow: bool,
    ) -> Result<(), String> {
        self.adapter
            .resolve_navigation_request(navigation_id, allow)
    }

    pub fn resolve_simple_dialog(
        &mut self,
        dialog_id: &str,
        confirmed: bool,
        prompt_value: Option<&str>,
    ) -> Result<(), String> {
        self.adapter
            .resolve_simple_dialog(dialog_id, confirmed, prompt_value)
    }

    /// Run Servo and return plain root/single-surface host events.
    ///
    /// Use [`Self::perform_webview_updates`] when callers need child webview ids.
    pub fn perform_updates(
        &mut self,
        surface_attached: bool,
        before_spin: impl FnOnce(),
        present: impl FnOnce(),
    ) -> Result<Vec<HostEvent>, String> {
        Ok(self
            .perform_webview_updates(surface_attached, before_spin, present)?
            .into_iter()
            .map(|event| event.event)
            .collect())
    }

    pub fn perform_webview_updates(
        &mut self,
        surface_attached: bool,
        before_spin: impl FnOnce(),
        present: impl FnOnce(),
    ) -> Result<Vec<ServoWebViewEvent>, String> {
        before_spin();
        self.adapter.runtime().perform_updates()?;

        Ok(self.paint_pending(surface_attached, present))
    }

    /// Paints this view if needed and drains its events after a shared engine update.
    ///
    /// This does not spin Servo. The owner must call `ServoRuntime::perform_updates`
    /// before presenting the independent views affected by that update.
    pub fn present_pending(
        &mut self,
        surface_attached: bool,
        present: impl FnOnce(),
    ) -> Vec<HostEvent> {
        self.paint_pending(surface_attached, present)
            .into_iter()
            .map(|event| event.event)
            .collect()
    }

    fn paint_pending(
        &mut self,
        surface_attached: bool,
        present: impl FnOnce(),
    ) -> Vec<ServoWebViewEvent> {
        if surface_attached && self.adapter.take_needs_paint() {
            if let Ok(webview) = self.adapter.native_webview() {
                webview.paint();
                present();
            }
        }

        self.adapter.drain_webview_events()
    }

    pub fn perform_managed_child_updates(
        &mut self,
        child_webview_id: &str,
        before_spin: impl FnOnce(),
        present: impl FnOnce(),
    ) -> Result<Vec<ServoWebViewEvent>, String> {
        self.adapter
            .ensure_managed_child_webview(child_webview_id)?;
        let surface_attached = self
            .adapter
            .managed_child_surface_attached(child_webview_id)?;

        before_spin();
        self.adapter.runtime().perform_updates()?;

        if surface_attached
            && self
                .adapter
                .take_managed_child_needs_paint(child_webview_id)
                .unwrap_or(false)
        {
            self.adapter.paint_managed_child(child_webview_id)?;
            present();
        }

        Ok(self.adapter.drain_webview_events())
    }
}

impl Drop for ServoWebView {
    fn drop(&mut self) {
        self.adapter.close();
    }
}

/// One owning-thread connection to the process Servo engine.
///
/// Clones share a single lease and event-loop waker. Each created view retains this
/// connection, while a host can retain it independently through a zero-view interval.
/// Native Servo reference counting owns the engine and webviews; this type only owns
/// the host connection and does not manage product tabs, windows, or popup policy.
#[derive(Clone)]
pub struct ServoRuntime {
    lease: Rc<ProcessServoRuntimeLease>,
}

impl ServoRuntime {
    /// Permanently shuts down the process engine after every view and other owner
    /// clone has been dropped. Ordinary drop releases the lease for later reuse.
    ///
    /// Servo supports only one engine initialization per process. Subsequent
    /// acquisition after this call returns an error, rather than rebuilding Servo.
    pub fn shutdown(self) -> Result<(), String> {
        let lease = Rc::try_unwrap(self.lease)
            .map_err(|_| "cannot shut down Servo while views or owner clones remain".to_owned())?;
        drop(lease);
        Self::shutdown_retained()
    }

    /// Permanently retires the process engine retained after ordinary owner drop.
    ///
    /// This also supports finalizing a host that never attached a surface. It must
    /// run on the engine's owning thread and cannot retire another live owner's engine.
    pub fn shutdown_retained() -> Result<(), String> {
        if PROCESS_SERVO_THREAD
            .get()
            .is_some_and(|owner| *owner != std::thread::current().id())
        {
            return Err("ExplorerKit process runtime must stay on its owning UI thread".into());
        }
        let runtime = PROCESS_SERVO_RUNTIME.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|runtime| runtime.waker.is_active())
            {
                return Err("cannot shut down Servo while a runtime owner remains".to_owned());
            }
            Ok(slot.take())
        })?;
        PROCESS_SERVO_SHUT_DOWN.store(true, std::sync::atomic::Ordering::Release);
        // Drop outside the thread-local borrow: Servo's shutdown pumps delegates.
        drop(runtime);
        Ok(())
    }

    /// Acquires the process engine on its owning UI thread.
    pub fn new(event_loop_waker: Box<dyn EventLoopWaker>) -> Result<Self, String> {
        Self::new_with_config_directory(event_loop_waker, None)
    }

    /// Acquires the process engine with Servo's process-wide config directory.
    ///
    /// The first initialization fixes this value for the process. Later owners must
    /// request the same directory, including whether one was configured at all.
    pub fn with_config_directory(
        event_loop_waker: Box<dyn EventLoopWaker>,
        config_directory: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        Self::new_with_config_directory(event_loop_waker, Some(config_directory.into()))
    }

    fn new_with_config_directory(
        event_loop_waker: Box<dyn EventLoopWaker>,
        config_directory: Option<PathBuf>,
    ) -> Result<Self, String> {
        Ok(Self {
            lease: Rc::new(acquire_process_servo_runtime(
                event_loop_waker,
                config_directory,
            )?),
        })
    }

    /// Creates an independent view with its own delegate and rendering context.
    pub fn create_webview(&self, init: ServoWebViewInit) -> Result<ServoWebView, String> {
        ServoWebView::with_runtime(init, self.clone())
    }

    /// Spins the shared engine; callbacks may target any of its views.
    pub fn perform_updates(&self) -> Result<(), String> {
        self.lease.servo().spin_event_loop();
        let errors = self.lease.errors.borrow_mut().drain(..).collect::<Vec<_>>();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

struct EngineDelegate(Rc<RefCell<Vec<String>>>);

impl servo::ServoDelegate for EngineDelegate {
    fn notify_error(&self, error: servo::ServoError) {
        self.0.borrow_mut().push(format!("{error:?}"));
    }
}

#[derive(Clone, Default)]
struct RoutedEventLoopWaker {
    target: std::sync::Arc<std::sync::Mutex<Option<Box<dyn EventLoopWaker>>>>,
}

impl RoutedEventLoopWaker {
    fn activate(&self, target: Box<dyn EventLoopWaker>) {
        *self
            .target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(target);
    }

    fn deactivate(&self) {
        *self
            .target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    fn is_active(&self) -> bool {
        self.target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }
}

impl EventLoopWaker for RoutedEventLoopWaker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        let target = self
            .target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .cloned();
        if let Some(target) = target {
            target.wake();
        }
    }
}

struct ProcessServoRuntime {
    servo: Servo,
    waker: RoutedEventLoopWaker,
    errors: Rc<RefCell<Vec<String>>>,
    config_directory: Option<PathBuf>,
}

impl ProcessServoRuntime {
    fn new(
        event_loop_waker: Box<dyn EventLoopWaker>,
        config_directory: Option<PathBuf>,
    ) -> Result<(Self, ProcessServoRuntimeLease), String> {
        // Keep the inventory-only default reader reachable from native static-library consumers.
        std::hint::black_box(
            &servo_default_resources::DefaultResourceReader
                as &dyn servo::resources::ResourceReaderMethods,
        );
        let _ = crate::ensure_default_rustls_crypto_provider();
        let waker = RoutedEventLoopWaker::default();
        waker.activate(event_loop_waker);

        let preferences = Preferences {
            viewport_meta_enabled: true,
            dom_permissions_enabled: true,
            dom_geolocation_enabled: true,
            dom_notification_enabled: true,
            ..Preferences::default()
        };

        if let Some(config_directory) = &config_directory {
            std::fs::create_dir_all(config_directory).map_err(|error| {
                format!(
                    "could not create Servo config directory {}: {error}",
                    config_directory.display()
                )
            })?;
        }
        let opts = servo::Opts {
            config_dir: config_directory.clone(),
            ..Default::default()
        };
        let servo = ServoBuilder::default()
            .opts(opts)
            .preferences(preferences)
            .event_loop_waker(Box::new(waker.clone()))
            .build();
        let errors = Rc::new(RefCell::new(Vec::new()));
        servo.set_delegate(Rc::new(EngineDelegate(errors.clone())));
        let lease = ProcessServoRuntimeLease {
            servo: servo.clone(),
            waker: waker.clone(),
            errors: errors.clone(),
        };
        Ok((
            Self {
                servo,
                waker,
                errors,
                config_directory,
            },
            lease,
        ))
    }

    fn acquire(
        &mut self,
        event_loop_waker: Box<dyn EventLoopWaker>,
        config_directory: Option<&std::path::Path>,
    ) -> Result<ProcessServoRuntimeLease, String> {
        if self.config_directory.as_deref() != config_directory {
            return Err(format!(
                "ExplorerKit process config directory is already fixed to {:?}; requested {:?}",
                self.config_directory.as_deref(),
                config_directory
            ));
        }
        if self.waker.is_active() {
            return Err("ExplorerKit supports one live Servo runtime owner per process; create sibling views through that owner".into());
        }
        self.waker.activate(event_loop_waker);
        Ok(ProcessServoRuntimeLease {
            servo: self.servo.clone(),
            waker: self.waker.clone(),
            errors: self.errors.clone(),
        })
    }
}

struct ProcessServoRuntimeLease {
    servo: Servo,
    waker: RoutedEventLoopWaker,
    errors: Rc<RefCell<Vec<String>>>,
}

impl ProcessServoRuntimeLease {
    fn servo(&self) -> &Servo {
        &self.servo
    }
}

impl Drop for ProcessServoRuntimeLease {
    fn drop(&mut self) {
        self.waker.deactivate();
    }
}

static PROCESS_SERVO_THREAD: std::sync::OnceLock<std::thread::ThreadId> =
    std::sync::OnceLock::new();

static PROCESS_SERVO_SHUT_DOWN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

thread_local! {
    static PROCESS_SERVO_RUNTIME: RefCell<Option<ProcessServoRuntime>> = const { RefCell::new(None) };
}

fn acquire_process_servo_runtime(
    event_loop_waker: Box<dyn EventLoopWaker>,
    config_directory: Option<PathBuf>,
) -> Result<ProcessServoRuntimeLease, String> {
    if PROCESS_SERVO_SHUT_DOWN.load(std::sync::atomic::Ordering::Acquire) {
        return Err(
            "ExplorerKit process engine has been shut down; Servo cannot restart in this process"
                .into(),
        );
    }
    let current_thread = std::thread::current().id();
    let owner_thread = PROCESS_SERVO_THREAD.get_or_init(|| current_thread);
    if *owner_thread != current_thread {
        return Err("ExplorerKit process runtime must stay on its owning UI thread".into());
    }

    PROCESS_SERVO_RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        if let Some(runtime) = runtime.as_mut() {
            return runtime.acquire(event_loop_waker, config_directory.as_deref());
        }
        let (process_runtime, lease) =
            ProcessServoRuntime::new(event_loop_waker, config_directory)?;
        *runtime = Some(process_runtime);
        Ok(lease)
    })
}

fn mouse_button(button: PointerButton) -> MouseButton {
    match button {
        PointerButton::Primary => MouseButton::Primary,
        PointerButton::Secondary => MouseButton::Secondary,
        PointerButton::Middle => MouseButton::Auxiliary,
        PointerButton::Back => MouseButton::Back,
        PointerButton::Forward => MouseButton::Forward,
        PointerButton::Other(value) => MouseButton::Other(value),
    }
}

fn wheel_mode(mode: PointerScrollMode) -> WheelMode {
    match mode {
        PointerScrollMode::Lines => WheelMode::DeltaLine,
        PointerScrollMode::Pixels => WheelMode::DeltaPixel,
    }
}

fn key_state(state: KeyboardInputState) -> KeyState {
    match state {
        KeyboardInputState::Pressed => KeyState::Down,
        KeyboardInputState::Released => KeyState::Up,
    }
}

fn keyboard_key(key: KeyboardInputKey) -> Key {
    match key {
        KeyboardInputKey::Character(value) => Key::Character(value),
        KeyboardInputKey::Named(KeyboardNamedKey::Space) => Key::Character(" ".to_owned()),
        KeyboardInputKey::Named(key) => Key::Named(named_key(key)),
    }
}

fn named_key(key: KeyboardNamedKey) -> NamedKey {
    match key {
        KeyboardNamedKey::Backspace => NamedKey::Backspace,
        KeyboardNamedKey::Delete => NamedKey::Delete,
        KeyboardNamedKey::Enter => NamedKey::Enter,
        KeyboardNamedKey::Tab => NamedKey::Tab,
        KeyboardNamedKey::Escape => NamedKey::Escape,
        KeyboardNamedKey::Space => unreachable!("space is emitted as a character key"),
        KeyboardNamedKey::ArrowLeft => NamedKey::ArrowLeft,
        KeyboardNamedKey::ArrowRight => NamedKey::ArrowRight,
        KeyboardNamedKey::ArrowUp => NamedKey::ArrowUp,
        KeyboardNamedKey::ArrowDown => NamedKey::ArrowDown,
    }
}

fn physical_size(size: SurfaceSize) -> PhysicalSize<u32> {
    PhysicalSize::new(size.width, size.height)
}

fn hidpi_scale_factor(density: f32) -> Scale<f32, DeviceIndependentPixel, DevicePixel> {
    Scale::new(density)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FaviconImage, LoadStatusKind, PopupRequestPolicy};
    use servo::{SoftwareRenderingContext, StringRequest};
    use std::cell::Cell;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::Command;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn host_pointer_buttons_keep_servo_semantics() {
        for (host, servo) in [
            (PointerButton::Primary, MouseButton::Primary),
            (PointerButton::Middle, MouseButton::Auxiliary),
            (PointerButton::Secondary, MouseButton::Secondary),
            (PointerButton::Back, MouseButton::Back),
            (PointerButton::Forward, MouseButton::Forward),
            (PointerButton::Other(6), MouseButton::Other(6)),
        ] {
            assert_eq!(mouse_button(host), servo);
        }
    }

    #[derive(Clone)]
    struct TestEventLoopWaker(Arc<AtomicBool>);

    impl EventLoopWaker for TestEventLoopWaker {
        fn clone_box(&self) -> Box<dyn EventLoopWaker> {
            Box::new(self.clone())
        }

        fn wake(&self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    #[derive(Default)]
    struct TestClipboardDelegate;

    impl ClipboardDelegate for TestClipboardDelegate {
        fn get_text(&self, _webview: WebView, request: StringRequest) {
            request.success(String::new());
        }
    }

    fn test_webview_init(
        initial_url: &str,
        popup_policy: PopupRequestPolicy,
        managed_child_rendering_context_factory: Option<ManagedChildRenderingContextFactory>,
        rendering_context: Rc<dyn RenderingContext>,
    ) -> ServoWebViewInit {
        ServoWebViewInit {
            rendering_context,
            clipboard_delegate: Rc::new(TestClipboardDelegate),
            initial_url: Some(initial_url.to_owned()),
            density: 1.0,
            popup_policy,
            managed_child_rendering_context_factory,
        }
    }

    fn test_rendering_context() -> Rc<dyn RenderingContext> {
        let rendering_context: Rc<dyn RenderingContext> = Rc::new(
            SoftwareRenderingContext::new(PhysicalSize::new(500, 500))
                .expect("software rendering context should be available for tests"),
        );
        rendering_context
            .make_current()
            .expect("software rendering context should become current");
        rendering_context
    }

    fn collect_events_until(
        webview: &mut ServoWebView,
        mut done: impl FnMut(&[HostEvent]) -> bool,
    ) -> Vec<HostEvent> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut events = Vec::new();

        while !done(&events) {
            events.extend(
                webview
                    .perform_updates(true, || {}, || {})
                    .expect("Servo updates should succeed"),
            );

            if Instant::now() > deadline {
                panic!("timed out waiting for Servo events; collected {events:?}");
            }
            thread::sleep(Duration::from_millis(1));
        }

        events
    }

    fn collect_webview_events_until(
        webview: &mut ServoWebView,
        mut done: impl FnMut(&[ServoWebViewEvent]) -> bool,
    ) -> Vec<ServoWebViewEvent> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut events = Vec::new();

        while !done(&events) {
            events.extend(
                webview
                    .perform_webview_updates(true, || {}, || {})
                    .expect("Servo updates should succeed"),
            );

            if Instant::now() > deadline {
                panic!("timed out waiting for Servo webview events; collected {events:?}");
            }
            thread::sleep(Duration::from_millis(1));
        }

        events
    }

    fn has_complete_load(events: &[HostEvent]) -> bool {
        events.iter().any(|event| {
            matches!(
                event,
                HostEvent::LoadStatusChanged {
                    status: LoadStatusKind::Complete,
                    ..
                }
            )
        })
    }

    fn has_routed_complete_load(events: &[ServoWebViewEvent]) -> bool {
        events.iter().any(|event| {
            matches!(
                event.event,
                HostEvent::LoadStatusChanged {
                    status: LoadStatusKind::Complete,
                }
            )
        })
    }

    fn popup_requests(events: &[HostEvent]) -> Vec<&HostEvent> {
        events
            .iter()
            .filter(|event| matches!(event, HostEvent::PopupRequested { .. }))
            .collect()
    }

    fn assert_default_denied_popup_event(event: &HostEvent) {
        match event {
            HostEvent::PopupRequested {
                request_id,
                parent_webview_id,
                parent_url,
                target_url,
                window_features,
                wants_popup,
                policy,
            } => {
                assert!(!request_id.is_empty());
                assert!(!parent_webview_id.is_empty());
                assert!(parent_url
                    .as_deref()
                    .is_some_and(|url| url.starts_with("data:text/html,")));
                assert_eq!(target_url, &None);
                assert_eq!(window_features, &None);
                assert_eq!(wants_popup, &None);
                assert_eq!(*policy, PopupRequestPolicy::DefaultDeny);
            }
            other => panic!("expected popup request event, got {other:?}"),
        }
    }

    fn popup_created_child_ids(events: &[ServoWebViewEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|routed_event| match &routed_event.event {
                HostEvent::PopupCreated {
                    child_webview_id, ..
                } => Some(child_webview_id.clone()),
                _ => None,
            })
            .collect()
    }

    fn has_child_complete_load(events: &[ServoWebViewEvent], child_webview_id: &str) -> bool {
        events.iter().any(|routed_event| {
            routed_event.webview_id == child_webview_id
                && matches!(
                    routed_event.event,
                    HostEvent::LoadStatusChanged {
                        status: LoadStatusKind::Complete,
                        ..
                    }
                )
        })
    }

    fn has_child_closed(events: &[ServoWebViewEvent], child_webview_id: &str) -> bool {
        events.iter().any(|routed_event| {
            routed_event.webview_id == child_webview_id
                && matches!(routed_event.event, HostEvent::Closed)
        })
    }

    fn has_child_surface_event(
        events: &[ServoWebViewEvent],
        child_webview_id: &str,
        matches_event: impl Fn(&HostEvent) -> bool,
    ) -> bool {
        events.iter().any(|routed_event| {
            routed_event.webview_id == child_webview_id && matches_event(&routed_event.event)
        })
    }

    fn navigation_requests(events: &[ServoWebViewEvent]) -> Vec<(String, String)> {
        events
            .iter()
            .filter_map(|routed_event| match &routed_event.event {
                HostEvent::NavigationRequested { navigation_id, .. } => {
                    Some((routed_event.webview_id.clone(), navigation_id.clone()))
                }
                _ => None,
            })
            .collect()
    }

    fn navigation_request_ids(events: &[ServoWebViewEvent]) -> Vec<String> {
        navigation_requests(events)
            .into_iter()
            .map(|(_, navigation_id)| navigation_id)
            .collect()
    }

    fn javascript_result<'a>(
        events: &'a [HostEvent],
        evaluation_id: &str,
    ) -> Option<&'a HostEvent> {
        events.iter().find(|event| {
            matches!(
                event,
                HostEvent::JavaScriptEvaluationResult {
                    evaluation_id: id,
                    ..
                } if id == evaluation_id
            )
        })
    }

    fn event_mentions_url(event: &HostEvent, expected_url: &str) -> bool {
        match event {
            HostEvent::NavigationRequested { url, .. } | HostEvent::UrlChanged { url } => {
                url == expected_url
            }
            HostEvent::Crashed { url, .. } | HostEvent::Error { url, .. } => {
                url.as_deref() == Some(expected_url)
            }
            HostEvent::PopupRequested {
                parent_url,
                target_url,
                ..
            }
            | HostEvent::PopupCreated {
                parent_url,
                target_url,
                ..
            } => {
                parent_url.as_deref() == Some(expected_url)
                    || target_url.as_deref() == Some(expected_url)
            }
            HostEvent::HistoryChanged { entries, .. } => {
                entries.iter().any(|url| url == expected_url)
            }
            _ => false,
        }
    }

    #[test]
    fn persistent_config_directory_survives_process_restart() {
        const DIRECTORY_ENV: &str = "EXPLORERKIT_TEST_CONFIG_DIRECTORY";
        const URL_ENV: &str = "EXPLORERKIT_TEST_PROFILE_URL";

        if let (Some(config_directory), Ok(url)) =
            (std::env::var_os(DIRECTORY_ENV), std::env::var(URL_ENV))
        {
            let config_directory = PathBuf::from(config_directory);
            let wake = Arc::new(AtomicBool::new(false));
            let owner = ServoRuntime::with_config_directory(
                Box::new(TestEventLoopWaker(wake.clone())),
                &config_directory,
            )
            .unwrap();
            let mut webview = owner
                .create_webview(test_webview_init(
                    &url,
                    PopupRequestPolicy::DefaultDeny,
                    None,
                    test_rendering_context(),
                ))
                .unwrap();
            collect_events_until(&mut webview, has_complete_load);
            drop(webview);
            drop(owner);

            let reused = ServoRuntime::with_config_directory(
                Box::new(TestEventLoopWaker(wake.clone())),
                &config_directory,
            )
            .expect("the retained process engine should accept the same directory");
            let mismatch = ServoRuntime::with_config_directory(
                Box::new(TestEventLoopWaker(wake)),
                config_directory.join("different"),
            )
            .err()
            .expect("a different directory should be rejected");
            assert!(mismatch.contains("already fixed"), "{mismatch}");
            assert!(
                ServoRuntime::new(Box::new(TestEventLoopWaker(Arc::new(AtomicBool::new(
                    false
                )))))
                .err()
                .is_some_and(|error| error.contains("already fixed"))
            );
            reused.shutdown().unwrap();
            return;
        }

        let config_directory = std::env::temp_dir().join(format!(
            "explorerkit-profile-restart-{}",
            std::process::id()
        ));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let cookie_seen = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let server = {
            let cookie_seen = cookie_seen.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    };
                    stream.set_nonblocking(false).unwrap();
                    let mut request = [0; 4096];
                    let count = stream.read(&mut request).unwrap();
                    let request = String::from_utf8_lossy(&request[..count]);
                    let set_cookie = request.starts_with("GET /set ");
                    if request.starts_with("GET /check ")
                        && request.lines().any(|line| {
                            line.to_ascii_lowercase().starts_with("cookie:")
                                && line.contains("explorerkit_profile=persisted")
                        })
                    {
                        cookie_seen.store(true, Ordering::Release);
                    }
                    let cookie = if set_cookie {
                        "Set-Cookie: explorerkit_profile=persisted; Path=/; SameSite=Lax\r\n"
                    } else {
                        ""
                    };
                    let body = "<!doctype html><title>profile</title>";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
                        body.len(), cookie, body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }
            })
        };

        for route in ["set", "check"] {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "servo_webview::tests::persistent_config_directory_survives_process_restart",
                    "--nocapture",
                ])
                .env(DIRECTORY_ENV, &config_directory)
                .env(URL_ENV, format!("http://{address}/{route}"))
                .status()
                .unwrap();
            assert!(status.success(), "profile {route} process failed");
        }

        stop.store(true, Ordering::Release);
        server.join().unwrap();
        assert!(
            cookie_seen.load(Ordering::Acquire),
            "the second process did not load the persisted cookie"
        );
        std::fs::remove_dir_all(config_directory).unwrap();
    }

    #[test]
    fn popup_and_process_runtime_lifetimes_share_one_real_servo_proof() {
        const STARTUP_REPLACEMENT_URL: &str =
            "data:text/html,%3Ctitle%3EStartup%3C/title%3Estartup-replacement";
        const FIRST_URL: &str =
            "data:text/html,<script>window.popupResult=window.open('https://example.com/popup')</script>";
        const REPLACEMENT_URL: &str =
            "data:text/html,%3Ctitle%3EReplacement%3C/title%3Ereplacement-runtime";

        let rendering_context = test_rendering_context();
        let invalid = ServoWebView::new(
            test_webview_init(
                "not a valid URL",
                PopupRequestPolicy::DefaultDeny,
                None,
                rendering_context.clone(),
            ),
            Box::new(TestEventLoopWaker(Arc::new(AtomicBool::new(false)))),
        );
        assert!(invalid.is_err());
        assert!(PROCESS_SERVO_THREAD.get().is_none());
        PROCESS_SERVO_RUNTIME.with(|runtime| assert!(runtime.borrow().is_none()));

        let child_context_count = Rc::new(Cell::new(0));
        let first_wake = Arc::new(AtomicBool::new(false));
        let child_context_factory: ManagedChildRenderingContextFactory = Rc::new({
            let child_context_count = child_context_count.clone();
            move |parent_rendering_context| {
                child_context_count.set(child_context_count.get() + 1);
                let context: Rc<dyn RenderingContext> = Rc::new(
                    SoftwareRenderingContext::new(parent_rendering_context.size())
                        .expect("managed child software context should be available"),
                );
                context
            }
        });
        let mut webview = ServoWebView::new(
            test_webview_init(
                "about:blank",
                PopupRequestPolicy::DefaultDeny,
                Some(child_context_factory),
                rendering_context.clone(),
            ),
            Box::new(TestEventLoopWaker(first_wake.clone())),
        )
        .expect("Servo webview should be created");
        webview
            .load_url(STARTUP_REPLACEMENT_URL)
            .expect("replacement probe should be accepted before the first update");
        webview.resize(SurfaceSize::new(500, 500));
        webview.request_paint();
        let parent_size = webview.size();

        let startup_events = collect_events_until(&mut webview, |events| {
            has_complete_load(events)
                && events.iter().any(
                    |event| matches!(event, HostEvent::UrlChanged { url } if url == "about:blank"),
                )
        });
        assert!(startup_events.iter().any(|event| matches!(
            event,
            HostEvent::LoadStatusChanged {
                status: LoadStatusKind::Complete,
            }
        )));
        assert!(startup_events
            .iter()
            .any(|event| matches!(event, HostEvent::UrlChanged { url } if url == "about:blank")));
        assert!(startup_events.iter().all(
            |event| !matches!(event, HostEvent::UrlChanged { url } if url == STARTUP_REPLACEMENT_URL)
        ));

        webview
            .load_url(STARTUP_REPLACEMENT_URL)
            .expect("replacement navigation should load after blank completes");
        let startup_replacement_events = collect_events_until(&mut webview, |events| {
            has_complete_load(events)
                && events.iter().any(
                    |event| matches!(event, HostEvent::UrlChanged { url } if url == STARTUP_REPLACEMENT_URL),
                )
        });
        assert!(has_complete_load(&startup_replacement_events));
        assert!(startup_replacement_events.iter().any(
            |event| matches!(event, HostEvent::UrlChanged { url } if url == STARTUP_REPLACEMENT_URL)
        ));
        webview
            .evaluate_javascript(
                "startup-document",
                "document.title + ':' + document.body.textContent",
            )
            .expect("startup document evaluation should be queued");
        let startup_document_events = collect_events_until(&mut webview, |events| {
            javascript_result(events, "startup-document").is_some()
        });
        assert_eq!(
            javascript_result(&startup_document_events, "startup-document"),
            Some(&HostEvent::JavaScriptEvaluationResult {
                evaluation_id: "startup-document".to_owned(),
                ok: true,
                value_json: Some(
                    r#"{"type":"string","value":"Startup:startup-replacement"}"#.to_owned()
                ),
                error_type: None,
            })
        );

        webview
            .evaluate_javascript(
                "engine-capabilities",
                "typeof WebGLRenderingContext === 'function' && crypto.getRandomValues(new Uint8Array(1)) instanceof Uint8Array",
            )
            .expect("engine capability evaluation should be queued");
        let capability_events = collect_events_until(&mut webview, |events| {
            javascript_result(events, "engine-capabilities").is_some()
        });
        assert_eq!(
            javascript_result(&capability_events, "engine-capabilities"),
            Some(&HostEvent::JavaScriptEvaluationResult {
                evaluation_id: "engine-capabilities".to_owned(),
                ok: true,
                value_json: Some(r#"{"type":"boolean","value":true}"#.to_owned()),
                error_type: None,
            })
        );

        webview
            .load_url(FIRST_URL)
            .expect("popup fixture should load");
        let events = collect_events_until(&mut webview, |events| {
            has_complete_load(events) && !popup_requests(events).is_empty()
        });
        let popup_events = popup_requests(&events);
        assert_eq!(popup_events.len(), 1);
        assert_default_denied_popup_event(popup_events[0]);

        let concurrent_wake = Arc::new(AtomicBool::new(false));
        let concurrent = ServoWebView::new(
            test_webview_init(
                "about:blank",
                PopupRequestPolicy::DefaultDeny,
                None,
                rendering_context.clone(),
            ),
            Box::new(TestEventLoopWaker(concurrent_wake.clone())),
        );
        let concurrent_error = match concurrent {
            Ok(_) => panic!("a concurrent root webview must be rejected"),
            Err(error) => error,
        };
        assert_eq!(
            concurrent_error,
            "ExplorerKit supports one live Servo runtime owner per process; create sibling views through that owner"
        );
        first_wake.store(false, Ordering::Relaxed);
        concurrent_wake.store(false, Ordering::Relaxed);
        PROCESS_SERVO_RUNTIME.with(|runtime| {
            runtime
                .borrow()
                .as_ref()
                .expect("process runtime should remain initialized")
                .waker
                .wake();
        });
        assert!(first_wake.load(Ordering::Relaxed));
        assert!(!concurrent_wake.load(Ordering::Relaxed));

        webview
            .evaluate_javascript("window-open-result", "window.popupResult === null")
            .expect("javascript evaluation should be queued");
        let result_events = collect_events_until(&mut webview, |events| {
            javascript_result(events, "window-open-result").is_some()
        });
        assert_eq!(
            javascript_result(&result_events, "window-open-result"),
            Some(&HostEvent::JavaScriptEvaluationResult {
                evaluation_id: "window-open-result".to_owned(),
                ok: true,
                value_json: Some(r#"{"type":"boolean","value":true}"#.to_owned()),
                error_type: None,
            })
        );

        webview
            .load_url(
                "data:text/html,%3Ca%20href%3D%22https%3A%2F%2Fexample.com%2Fpopup%22%20target%3D%22_blank%22%20style%3D%22display%3Ablock%3Bwidth%3A200px%3Bheight%3A80px%22%3Eopen%3C%2Fa%3E",
            )
            .expect("target blank fixture should load");
        let navigation_events = collect_events_until(&mut webview, has_complete_load);
        assert!(popup_requests(&navigation_events).is_empty());

        webview
            .dispatch_pointer_input(PointerInputEvent::moved(10.0, 10.0))
            .expect("pointer move should dispatch");
        webview
            .dispatch_pointer_input(PointerInputEvent::button(
                PointerButtonAction::Pressed,
                PointerButton::Primary,
                10.0,
                10.0,
            ))
            .expect("pointer down should dispatch");
        webview
            .dispatch_pointer_input(PointerInputEvent::button(
                PointerButtonAction::Released,
                PointerButton::Primary,
                10.0,
                10.0,
            ))
            .expect("pointer up should dispatch");

        let click_events =
            collect_events_until(&mut webview, |events| !popup_requests(events).is_empty());
        let popup_events = popup_requests(&click_events);
        assert_eq!(popup_events.len(), 1);
        assert_default_denied_popup_event(popup_events[0]);
        assert!(click_events.iter().all(|event| !matches!(
            event,
            HostEvent::UrlChanged { url }
                if url == "https://example.com/popup"
        )));
        assert!(webview.managed_child_webview_ids().is_empty());
        assert_eq!(child_context_count.get(), 0);

        webview.set_popup_policy(PopupRequestPolicy::ManagedChild);
        let pending_child_url = "data:text/html,%3Ctitle%3EPending%3C/title%3Epending";
        let pending_parent_url = format!(
            "data:text/html,<button id=go style=\"display:block;width:200px;height:80px\" onclick=\"\
             window.pendingPopup=window.open('{pending_child_url}');\
             \">open</button>"
        );
        webview
            .load_url(&pending_parent_url)
            .expect("managed pending-navigation fixture should load");
        let pending_navigation_events = collect_events_until(&mut webview, has_complete_load);
        assert!(popup_requests(&pending_navigation_events).is_empty());

        webview
            .dispatch_pointer_input(PointerInputEvent::moved(10.0, 10.0))
            .expect("pointer move should dispatch");
        webview
            .dispatch_pointer_input(PointerInputEvent::button(
                PointerButtonAction::Pressed,
                PointerButton::Primary,
                10.0,
                10.0,
            ))
            .expect("pointer down should dispatch");
        webview
            .dispatch_pointer_input(PointerInputEvent::button(
                PointerButtonAction::Released,
                PointerButton::Primary,
                10.0,
                10.0,
            ))
            .expect("pointer up should dispatch");
        let pending_child_events = collect_webview_events_until(&mut webview, |events| {
            popup_created_child_ids(events).len() == 1 && navigation_requests(events).len() == 1
        });
        let pending_child_id = popup_created_child_ids(&pending_child_events)
            .into_iter()
            .next()
            .expect("managed popup should create one child");
        assert_eq!(child_context_count.get(), 1);
        let (pending_navigation_webview_id, pending_navigation_id) =
            navigation_requests(&pending_child_events)
                .into_iter()
                .next()
                .expect("managed child should have one pending navigation");
        assert_eq!(pending_navigation_webview_id, pending_child_id);
        assert_eq!(
            webview.managed_child_webview_ids(),
            vec![pending_child_id.clone()]
        );
        webview
            .destroy_managed_child_webview(&pending_child_id)
            .expect("host destroy should remove child with pending navigation");
        assert!(webview.managed_child_webview_ids().is_empty());
        assert_eq!(webview.opener_webview_id(&pending_child_id), None);
        assert_eq!(
            webview.resolve_navigation_request(&pending_navigation_id, true),
            Err(format!(
                "navigation request {pending_navigation_id} is no longer pending"
            ))
        );

        let retained_child_url = concat!(
            "data:text/html,%253Ctitle%253ERetained%253C%252Ftitle%253E",
            "%253Cscript%253Eonmessage%253D()%253D%253Ewindow.open(%2527about%253Ablank%2527)",
            "%253C%252Fscript%253E"
        );
        let closing_child_url =
            "data:text/html,%253Cscript%253Ewindow.close()%253C%252Fscript%253E";
        let managed_parent_url = format!(
            "data:text/html,<button id=go style=\"display:block;width:200px;height:80px\" onclick=\"\
             window.firstPopup=window.open('{retained_child_url}');\
             window.secondPopup=window.open('{closing_child_url}');\
             \">open</button>"
        );
        webview
            .load_url(&managed_parent_url)
            .expect("managed popup fixture should load");
        let managed_navigation_events = collect_events_until(&mut webview, has_complete_load);
        assert!(popup_requests(&managed_navigation_events).is_empty());

        webview
            .dispatch_pointer_input(PointerInputEvent::moved(10.0, 10.0))
            .expect("pointer move should dispatch");
        webview
            .dispatch_pointer_input(PointerInputEvent::button(
                PointerButtonAction::Pressed,
                PointerButton::Primary,
                10.0,
                10.0,
            ))
            .expect("pointer down should dispatch");
        webview
            .dispatch_pointer_input(PointerInputEvent::button(
                PointerButtonAction::Released,
                PointerButton::Primary,
                10.0,
                10.0,
            ))
            .expect("pointer up should dispatch");

        assert_managed_popup_children_are_retained_routed_and_cleaned_up(
            &mut webview,
            retained_child_url,
            closing_child_url,
            parent_size,
        );
        assert_eq!(child_context_count.get(), 3);

        drop(webview);
        let replacement_wake = Arc::new(AtomicBool::new(false));
        let mut replacement = ServoWebView::new(
            test_webview_init(
                REPLACEMENT_URL,
                PopupRequestPolicy::DefaultDeny,
                None,
                rendering_context,
            ),
            Box::new(TestEventLoopWaker(replacement_wake.clone())),
        )
        .expect("replacement webview should reuse the process Servo runtime");
        assert!(replacement.managed_child_webview_ids().is_empty());
        assert!(replacement.drain_events().is_empty());
        first_wake.store(false, Ordering::Relaxed);
        replacement_wake.store(false, Ordering::Relaxed);
        PROCESS_SERVO_RUNTIME.with(|runtime| {
            runtime
                .borrow()
                .as_ref()
                .expect("process runtime should remain initialized")
                .waker
                .wake();
        });
        assert!(!first_wake.load(Ordering::Relaxed));
        assert!(replacement_wake.load(Ordering::Relaxed));
        let replacement_events = collect_events_until(&mut replacement, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    HostEvent::LoadStatusChanged {
                        status: LoadStatusKind::Complete,
                    }
                )
            })
        });
        assert!(replacement_events
            .iter()
            .any(|event| matches!(event, HostEvent::UrlChanged { url } if url == REPLACEMENT_URL)));
        let first_url = Url::parse(FIRST_URL).unwrap().to_string();
        assert!(!replacement_events
            .iter()
            .any(|event| event_mentions_url(event, &first_url)));
        drop(replacement);
        assert_shared_views_and_final_shutdown();
    }

    fn assert_shared_views_and_final_shutdown() {
        const FIRST_URL: &str = concat!(
            "data:text/html,<title>First</title><link rel='icon' href='data:image/png;base64,",
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABAQMAAAAl21bKAAAAA1BMVEX/AAAZ4gk3AAAACklEQVQI12NgAAAAAgAB4iG8MwAAAABJRU5ErkJggg==",
            "'>"
        );
        const SECOND_URL: &str = concat!(
            "data:text/html,<title>Second</title><link rel='icon' href='data:image/png;base64,",
            "iVBORw0KGgoAAAANSUhEUgAAAAIAAAABAQMAAADO7O3JAAAAA1BMVEUAAP+KeNJXAAAACklEQVQI12NgAAAAAgAB4iG8MwAAAABJRU5ErkJggg==",
            "'>"
        );
        const REPLACEMENT_URL: &str = concat!(
            "data:text/html,<title>First replacement</title><link rel='icon' href='data:image/png;base64,",
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAACAQMAAACjTyRkAAAAA1BMVEUAgACc+aWRAAAADElEQVQI12NgYGAAAAAEAAEnNCcKAAAAAElFTkSuQmCC",
            "'>"
        );

        let wake = Arc::new(AtomicBool::new(false));
        let owner = ServoRuntime::new(Box::new(TestEventLoopWaker(wake.clone()))).unwrap();
        assert!(ServoRuntime::shutdown_retained().is_err());
        let mut first = owner
            .create_webview(test_webview_init(
                FIRST_URL,
                PopupRequestPolicy::DefaultDeny,
                None,
                test_rendering_context(),
            ))
            .unwrap();
        let mut second = owner
            .create_webview(test_webview_init(
                SECOND_URL,
                PopupRequestPolicy::DefaultDeny,
                None,
                test_rendering_context(),
            ))
            .unwrap();
        let first_events = collect_webview_events_until(&mut first, |events| {
            has_routed_complete_load(events)
                && events.iter().any(|event| {
                    matches!(
                        event.event,
                        HostEvent::FaviconChanged {
                            favicon: Some(FaviconImage {
                                width: 1,
                                height: 1,
                                ..
                            })
                        }
                    )
                })
        });
        let second_events = collect_webview_events_until(&mut second, |events| {
            has_routed_complete_load(events)
                && events.iter().any(|event| {
                    matches!(
                        event.event,
                        HostEvent::FaviconChanged {
                            favicon: Some(FaviconImage {
                                width: 2,
                                height: 1,
                                ..
                            })
                        }
                    )
                })
        });
        let first_webview_id = first_events[0].webview_id.clone();
        let second_webview_id = second_events[0].webview_id.clone();
        assert_ne!(first_webview_id, second_webview_id);
        assert!(first_events
            .iter()
            .all(|event| event.webview_id == first_webview_id));
        assert!(second_events
            .iter()
            .all(|event| event.webview_id == second_webview_id));

        first.load_url(REPLACEMENT_URL).unwrap();
        let replacement_events = collect_webview_events_until(&mut first, |events| {
            has_routed_complete_load(events)
                && events.iter().any(|event| {
                    matches!(
                        event.event,
                        HostEvent::FaviconChanged {
                            favicon: Some(FaviconImage {
                                width: 1,
                                height: 2,
                                ..
                            })
                        }
                    )
                })
        });
        assert!(replacement_events
            .iter()
            .all(|event| event.webview_id == first_webview_id));
        first
            .evaluate_javascript("same-id", "document.title === 'First replacement'")
            .unwrap();
        second
            .evaluate_javascript("same-id", "document.title === 'Second'")
            .unwrap();
        for view in [&mut first, &mut second] {
            let events = collect_events_until(view, |events| {
                javascript_result(events, "same-id").is_some()
            });
            assert!(
                matches!(javascript_result(&events, "same-id"), Some(HostEvent::JavaScriptEvaluationResult {
                ok: true, value_json: Some(value), ..
            }) if value == r#"{"type":"boolean","value":true}"#)
            );
        }
        drop(first);
        second
            .evaluate_javascript("survivor", "document.title === 'Second'")
            .unwrap();
        let events = collect_events_until(&mut second, |events| {
            javascript_result(events, "survivor").is_some()
        });
        assert!(
            matches!(javascript_result(&events, "survivor"), Some(HostEvent::JavaScriptEvaluationResult {
            ok: true, value_json: Some(value), ..
        }) if value == r#"{"type":"boolean","value":true}"#)
        );
        drop(second);
        owner.perform_updates().unwrap();
        let mut replacement = owner
            .create_webview(test_webview_init(
                "data:text/html,<title>AfterEmpty</title>",
                PopupRequestPolicy::DefaultDeny,
                None,
                test_rendering_context(),
            ))
            .unwrap();
        collect_events_until(&mut replacement, has_complete_load);
        // Shutdown refuses to invalidate a live sibling's native engine reference.
        assert!(owner.shutdown().is_err());
        replacement
            .evaluate_javascript("still-live", "document.title === 'AfterEmpty'")
            .unwrap();
        let events = collect_events_until(&mut replacement, |events| {
            javascript_result(events, "still-live").is_some()
        });
        assert!(
            matches!(javascript_result(&events, "still-live"), Some(HostEvent::JavaScriptEvaluationResult {
            ok: true, value_json: Some(value), ..
        }) if value == r#"{"type":"boolean","value":true}"#)
        );
        drop(replacement);
        ServoRuntime::shutdown_retained().unwrap();
        assert!(ServoRuntime::new(Box::new(TestEventLoopWaker(wake))).is_err());
    }

    fn assert_managed_popup_children_are_retained_routed_and_cleaned_up(
        webview: &mut ServoWebView,
        retained_child_url: &str,
        closing_child_url: &str,
        parent_size: SurfaceSize,
    ) {
        let mut events = collect_webview_events_until(webview, |events| {
            popup_created_child_ids(events).len() == 2 && navigation_request_ids(events).len() == 2
        });
        assert!(events
            .iter()
            .all(|event| !matches!(event.event, HostEvent::PopupRequested { .. })));

        let created_ids = popup_created_child_ids(&events);
        assert_eq!(created_ids.len(), 2);
        assert_ne!(created_ids[0], created_ids[1]);
        for navigation_id in navigation_request_ids(&events) {
            webview
                .resolve_navigation_request(&navigation_id, true)
                .expect("popup child navigation should be explicitly allowed for the fixture");
        }
        let followup_events = collect_webview_events_until(webview, |followup_events| {
            let closed_id = created_ids
                .iter()
                .find(|child_id| has_child_closed(followup_events, child_id));
            closed_id.is_some()
                && created_ids.iter().any(|child_id| {
                    Some(child_id) != closed_id
                        && has_child_complete_load(followup_events, child_id)
                })
        });
        events.extend(followup_events);

        let closed_child_webview_id = created_ids
            .iter()
            .find(|child_id| has_child_closed(&events, child_id))
            .expect("one managed child should close itself")
            .clone();
        let retained_child_webview_id = created_ids
            .iter()
            .find(|child_id| **child_id != closed_child_webview_id)
            .expect("one managed child should remain retained")
            .clone();

        let popup_created_events = events
            .iter()
            .filter(|event| matches!(event.event, HostEvent::PopupCreated { .. }))
            .collect::<Vec<_>>();
        let parent_webview_id = match &popup_created_events[0].event {
            HostEvent::PopupCreated {
                parent_webview_id,
                parent_url,
                target_url,
                window_features,
                wants_popup,
                policy,
                ..
            } => {
                assert_eq!(*policy, PopupRequestPolicy::ManagedChild);
                assert!(parent_url
                    .as_deref()
                    .is_some_and(|url| url.starts_with("data:text/html,")));
                assert_eq!(target_url, &None);
                assert_eq!(window_features, &None);
                assert_eq!(wants_popup, &None);
                parent_webview_id.clone()
            }
            other => panic!("expected popup-created event, got {other:?}"),
        };
        for popup_created in popup_created_events {
            match &popup_created.event {
                HostEvent::PopupCreated {
                    parent_webview_id: event_parent_webview_id,
                    child_webview_id,
                    policy,
                    ..
                } => {
                    assert_eq!(&popup_created.webview_id, event_parent_webview_id);
                    assert_eq!(event_parent_webview_id, &parent_webview_id);
                    assert!(created_ids.contains(child_webview_id));
                    assert_eq!(*policy, PopupRequestPolicy::ManagedChild);
                }
                other => panic!("expected popup-created event, got {other:?}"),
            }
        }

        assert_eq!(
            webview.managed_child_webview_ids(),
            vec![retained_child_webview_id.clone()]
        );
        assert_eq!(
            webview.opener_webview_id(&retained_child_webview_id),
            Some(parent_webview_id.clone())
        );
        assert_eq!(webview.opener_webview_id(&closed_child_webview_id), None);
        assert!(has_child_complete_load(&events, &retained_child_webview_id));
        assert!(has_child_closed(&events, &closed_child_webview_id));
        assert!(events.iter().all(|event| {
            !(event.webview_id == parent_webview_id
                && matches!(
                    &event.event,
                    HostEvent::UrlChanged { url }
                        if url == retained_child_url || url == closing_child_url
                ))
        }));

        let child_surface = HostSurface::new("popup-child-surface");
        let child_viewport = SurfaceViewport::new(SurfaceSize::new(240, 160), 2.0);
        webview
            .attach_managed_child_surface(
                &retained_child_webview_id,
                child_surface.clone(),
                child_viewport,
            )
            .expect("managed popup child surface should attach");
        assert_eq!(webview.size(), parent_size);
        assert_eq!(
            webview.managed_child_surface_viewport(&retained_child_webview_id),
            Some(child_viewport)
        );
        let attach_events = webview.drain_webview_events();
        assert!(has_child_surface_event(
            &attach_events,
            &retained_child_webview_id,
            |event| matches!(event, HostEvent::SurfaceAttached { size } if *size == child_viewport.size)
        ));
        assert!(attach_events.iter().all(|event| {
            !(event.webview_id == parent_webview_id
                && matches!(event.event, HostEvent::SurfaceAttached { .. }))
        }));

        let child_present_count = Rc::new(Cell::new(0));
        let child_present_count_for_update = child_present_count.clone();
        webview
            .perform_managed_child_updates(
                &retained_child_webview_id,
                || {},
                move || {
                    child_present_count_for_update.set(child_present_count_for_update.get() + 1);
                },
            )
            .expect("managed popup child surface should render");
        assert!(child_present_count.get() > 0);

        let resized_viewport = SurfaceViewport::new(SurfaceSize::new(320, 180), 1.5);
        webview
            .update_managed_child_surface_viewport(&retained_child_webview_id, resized_viewport)
            .expect("managed popup child surface should resize");
        assert_eq!(webview.size(), parent_size);
        assert_eq!(
            webview.managed_child_surface_viewport(&retained_child_webview_id),
            Some(resized_viewport)
        );
        let resize_events = webview.drain_webview_events();
        assert!(has_child_surface_event(
            &resize_events,
            &retained_child_webview_id,
            |event| matches!(event, HostEvent::SurfaceResized { size } if *size == resized_viewport.size)
        ));
        let present_before_resize = child_present_count.get();
        let child_present_count_for_resize = child_present_count.clone();
        webview
            .perform_managed_child_updates(
                &retained_child_webview_id,
                || {},
                move || {
                    child_present_count_for_resize.set(child_present_count_for_resize.get() + 1);
                },
            )
            .expect("resized managed popup child surface should render");
        assert!(child_present_count.get() > present_before_resize);

        webview
            .detach_managed_child_surface(&retained_child_webview_id)
            .expect("managed popup child surface should detach");
        assert_eq!(
            webview.managed_child_surface_viewport(&retained_child_webview_id),
            None
        );
        let detach_events = webview.drain_webview_events();
        assert!(has_child_surface_event(
            &detach_events,
            &retained_child_webview_id,
            |event| matches!(event, HostEvent::SurfaceDetached)
        ));
        let present_before_detached_update = child_present_count.get();
        let child_present_count_for_detached_update = child_present_count.clone();
        webview
            .perform_managed_child_updates(
                &retained_child_webview_id,
                || {},
                move || {
                    child_present_count_for_detached_update
                        .set(child_present_count_for_detached_update.get() + 1);
                },
            )
            .expect("detached managed popup child should stay valid for cleanup");
        assert_eq!(child_present_count.get(), present_before_detached_update);

        let root_present_count = Rc::new(Cell::new(0));
        let root_present_count_for_update = root_present_count.clone();
        webview.request_paint();
        webview
            .perform_updates(
                true,
                || {},
                move || {
                    root_present_count_for_update.set(root_present_count_for_update.get() + 1);
                },
            )
            .expect("parent should still render after child surface cleanup");
        assert_eq!(root_present_count.get(), 1);
        assert_eq!(webview.size(), parent_size);

        webview.set_popup_policy(PopupRequestPolicy::DefaultDeny);
        webview
            .evaluate_javascript(
                "legacy-policy-change",
                "firstPopup.postMessage('open', '*')",
            )
            .unwrap();
        let denied = collect_webview_events_until(webview, |events| {
            events
                .iter()
                .any(|event| matches!(event.event, HostEvent::PopupRequested { .. }))
        });
        assert!(denied.iter().any(|event| {
            event.webview_id == retained_child_webview_id
                && matches!(
                    event.event,
                    HostEvent::PopupRequested {
                        policy: PopupRequestPolicy::DefaultDeny,
                        ..
                    }
                )
        }));
        assert_eq!(
            webview.managed_child_webview_ids(),
            vec![retained_child_webview_id.clone()]
        );

        webview
            .destroy_managed_child_webview(&retained_child_webview_id)
            .expect("host destroy should remove retained child");
        assert!(webview.managed_child_webview_ids().is_empty());
        assert_eq!(webview.opener_webview_id(&retained_child_webview_id), None);
    }
}
