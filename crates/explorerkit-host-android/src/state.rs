use crate::native_window::release_native_window;
use crate::{AndroidRenderBackend, NativeWindowHandle, ServoStatus};
use explorerkit_embedder::{
    ContextMenuAction, ExplorerKitEvent, Host, HostError, HostEvent, ImeCompositionState,
    KeyboardKey, NavigationRequest, Runtime, RuntimeError, SessionHandle, SurfaceSize,
    SurfaceViewport, WebViewCommand, WebViewHandle,
};
use explorerkit_host::HostSurface;
use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

pub struct HostHandle {
    state: Box<AndroidHostState>,
    runtime: Runtime<AndroidHost>,
    _session: SessionHandle,
    pub(crate) webview: WebViewHandle,
}

pub struct AndroidHostState {
    pub(crate) current_url: Option<String>,
    pub(crate) pending_url: Option<String>,
    pub(crate) pending_commands: VecDeque<ControlCommand>,
    pub(crate) surface: Option<SurfaceSize>,
    pub(crate) surface_density: Option<f32>,
    pub(crate) native_window: Option<NativeWindowHandle>,
    pub(crate) android_backend: Option<Box<dyn AndroidRenderBackend>>,
    pub(crate) pending_attach_native_window: Option<NativeWindowHandle>,
    pub(crate) pending_attach_density: Option<f32>,
    pub(crate) pending_resize_density: Option<f32>,
    pub(crate) events: VecDeque<HostEvent>,
    pub(crate) last_event: Option<HostEvent>,
    pub(crate) last_event_bridge_json: Option<String>,
    created_webview: Option<WebViewHandle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControlCommand {
    WebView(WebViewCommand),
    DispatchImeComposition {
        state: ImeCompositionState,
        text: String,
    },
    DismissInputMethod,
    DispatchKeyboardKey {
        key: KeyboardKey,
    },
    ResolveSimpleDialog {
        dialog_id: String,
        confirmed: bool,
        prompt_value: Option<String>,
    },
    ResolveSelectElement {
        select_element_id: String,
        selected_options: Vec<usize>,
    },
    ResolveFilePicker {
        file_picker_id: String,
        selected_paths: Vec<String>,
    },
    DismissFilePicker {
        file_picker_id: String,
    },
    ResolvePermission {
        allow: bool,
    },
    ResolveNavigationRequest {
        navigation_id: String,
        allow: bool,
    },
    ResolveContextMenu {
        context_menu_id: String,
        action: ContextMenuAction,
    },
    DismissContextMenu {
        context_menu_id: String,
    },
    EvaluateJavaScript {
        evaluation_id: String,
        script: String,
    },
}

impl Drop for HostHandle {
    fn drop(&mut self) {
        if let Some(backend) = self.android_backend.as_mut() {
            backend.shutdown();
        }
        release_native_window(self.native_window.take());
        release_native_window(self.pending_attach_native_window.take());
    }
}

impl Deref for HostHandle {
    type Target = AndroidHostState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl DerefMut for HostHandle {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

impl HostHandle {
    pub(crate) fn new() -> Self {
        let mut state = Box::new(AndroidHostState::new());
        let state_ptr = NonNull::from(state.as_mut());
        let mut runtime = Runtime::new(AndroidHost::new(state_ptr));
        let session = runtime.create_session();
        let webview = runtime
            .create_webview(session)
            .expect("android host webview creation should not fail");

        Self {
            state,
            runtime,
            _session: session,
            webview,
        }
    }

    pub(crate) fn dispatch_webview_command_runtime(
        &mut self,
        command: WebViewCommand,
    ) -> Result<(), RuntimeError> {
        self.runtime
            .dispatch_webview_command(self.webview, command)?;
        self.drain_runtime_events();
        Ok(())
    }

    pub(crate) fn load_url_runtime(&mut self, url: &str) -> Result<(), RuntimeError> {
        self.runtime.load_url(self.webview, url)?;
        self.drain_runtime_events();
        Ok(())
    }

    pub(crate) fn attach_surface_runtime(
        &mut self,
        surface: HostSurface,
        size: SurfaceSize,
    ) -> Result<(), RuntimeError> {
        self.runtime.attach_surface(self.webview, surface, size)?;
        self.drain_runtime_events();
        Ok(())
    }

    pub(crate) fn resize_surface_runtime(&mut self, size: SurfaceSize) -> Result<(), RuntimeError> {
        self.runtime.resize_surface(self.webview, size)?;
        self.drain_runtime_events();
        Ok(())
    }

    pub(crate) fn detach_surface_runtime(&mut self) -> Result<(), RuntimeError> {
        self.runtime.detach_surface(self.webview)?;
        self.drain_runtime_events();
        Ok(())
    }

    pub(crate) fn perform_updates_runtime(&mut self) -> Result<(), RuntimeError> {
        self.runtime.perform_updates(self.webview)?;
        self.drain_runtime_events();
        Ok(())
    }

    pub(crate) fn push_backend_error(
        &mut self,
        diagnostic_url: Option<&str>,
        message: impl Into<String>,
    ) {
        self.events.push_back(HostEvent::Error {
            url: diagnostic_url.map(str::to_owned),
            code: ServoStatus::BackendError as i32,
            message: message.into(),
        });
    }

    fn drain_runtime_events(&mut self) {
        let events = self.runtime.drain_events();
        for ExplorerKitEvent { webview, event, .. } in events {
            if webview != self.webview {
                continue;
            }
            self.observe_event(&event);
            self.events.push_back(event);
        }
    }
}

impl AndroidHostState {
    fn new() -> Self {
        Self {
            current_url: None,
            pending_url: None,
            pending_commands: VecDeque::new(),
            surface: None,
            surface_density: None,
            native_window: None,
            android_backend: None,
            pending_attach_native_window: None,
            pending_attach_density: None,
            pending_resize_density: None,
            events: VecDeque::new(),
            last_event: None,
            last_event_bridge_json: None,
            created_webview: None,
        }
    }

    fn observe_event(&mut self, event: &HostEvent) {
        if let HostEvent::UrlChanged { url } = event {
            self.current_url = Some(url.clone());
        }
    }
}

#[derive(Clone, Copy)]
struct AndroidHost {
    state: NonNull<AndroidHostState>,
}

impl AndroidHost {
    fn new(state: NonNull<AndroidHostState>) -> Self {
        Self { state }
    }

    fn state_mut(&mut self) -> &mut AndroidHostState {
        unsafe { self.state.as_mut() }
    }
}

impl Host for AndroidHost {
    fn create_webview(
        &mut self,
        _session: SessionHandle,
        webview: WebViewHandle,
    ) -> Result<(), HostError> {
        let state = self.state_mut();
        state.created_webview = Some(webview);
        Ok(())
    }

    fn attach_surface(
        &mut self,
        webview: WebViewHandle,
        _surface: &HostSurface,
        size: SurfaceSize,
    ) -> Result<Vec<HostEvent>, HostError> {
        let state = self.state_mut();
        ensure_created_webview(state, webview)?;

        if state.surface.is_some() || state.native_window.is_some() {
            release_native_window(state.pending_attach_native_window.take());
            state.pending_attach_density = None;
            return Ok(Vec::new());
        }

        let native_window = state.pending_attach_native_window.take();
        let density = state.pending_attach_density.take().unwrap_or(1.0);
        state.surface = Some(size);
        state.surface_density = Some(density);
        state.native_window = native_window;

        let Some(native_window) = native_window else {
            return Ok(Vec::new());
        };

        let was_backend_installed = state.android_backend.is_some();
        let pending_url = state.pending_url.clone();

        let attach_result = if was_backend_installed {
            let backend = state
                .android_backend
                .as_mut()
                .expect("android backend is present");
            backend.attach_surface(native_window, size, density)
        } else {
            install_android_backend_for_state(state, pending_url.as_deref()).and_then(|_| {
                let backend = state
                    .android_backend
                    .as_mut()
                    .ok_or_else(|| "android backend was not created".to_owned())?;
                backend.perform_updates()
            })
        };

        let mut events = attach_result.map_err(|message| {
            release_native_window(state.native_window.take());
            state.surface = None;
            state.surface_density = None;
            HostError::new(message)
        })?;

        if let Some(url) = pending_url {
            if was_backend_installed {
                let load_result = {
                    let backend = state
                        .android_backend
                        .as_mut()
                        .expect("android backend is present");
                    backend.load_url(&url)
                };
                events.extend(events_from_backend_result(load_result, Some(&url)));
            }
            state.pending_url = None;
        }

        Ok(events)
    }

    fn attach_surface_with_viewport(
        &mut self,
        webview: WebViewHandle,
        surface: &HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.state_mut().pending_attach_density = Some(viewport.scale_factor);
        self.attach_surface(webview, surface, viewport.size)
    }

    fn resize_surface(
        &mut self,
        webview: WebViewHandle,
        size: SurfaceSize,
    ) -> Result<Vec<HostEvent>, HostError> {
        let state = self.state_mut();
        ensure_created_webview(state, webview)?;

        state.surface = Some(size);
        let density = state
            .pending_resize_density
            .take()
            .or(state.surface_density)
            .unwrap_or(1.0);
        state.surface_density = Some(density);

        if state.native_window.is_none() {
            return Ok(Vec::new());
        }

        let current_url = state.current_url.clone();
        let Some(backend) = state.android_backend.as_mut() else {
            return Ok(Vec::new());
        };
        Ok(events_from_backend_result(
            backend.resize_surface(size, density),
            current_url.as_deref(),
        ))
    }

    fn set_surface_scale_factor(
        &mut self,
        webview: WebViewHandle,
        scale_factor: f32,
    ) -> Result<Vec<HostEvent>, HostError> {
        let state = self.state_mut();
        ensure_created_webview(state, webview)?;

        let Some(size) = state.surface else {
            state.surface_density = Some(scale_factor);
            return Ok(Vec::new());
        };
        state.surface_density = Some(scale_factor);

        if state.native_window.is_none() {
            return Ok(Vec::new());
        }

        let current_url = state.current_url.clone();
        let Some(backend) = state.android_backend.as_mut() else {
            return Ok(Vec::new());
        };
        Ok(events_from_backend_result(
            backend.resize_surface(size, scale_factor),
            current_url.as_deref(),
        ))
    }

    fn update_surface_viewport(
        &mut self,
        webview: WebViewHandle,
        viewport: SurfaceViewport,
        _previous_viewport: SurfaceViewport,
    ) -> Result<Vec<HostEvent>, HostError> {
        self.state_mut().pending_resize_density = Some(viewport.scale_factor);
        self.resize_surface(webview, viewport.size)
    }

    fn detach_surface(&mut self, webview: WebViewHandle) -> Result<Vec<HostEvent>, HostError> {
        let state = self.state_mut();
        ensure_created_webview(state, webview)?;

        if state.surface.is_none() {
            return Ok(Vec::new());
        }

        let backend_events = if let (Some(_), Some(backend)) =
            (state.native_window, state.android_backend.as_mut())
        {
            backend.detach_surface().map_err(HostError::new)?
        } else {
            Vec::new()
        };

        state.surface = None;
        state.surface_density = None;
        release_native_window(state.native_window.take());

        Ok(backend_events)
    }

    fn dispatch_webview_command(
        &mut self,
        webview: WebViewHandle,
        command: &WebViewCommand,
    ) -> Result<Vec<HostEvent>, HostError> {
        let state = self.state_mut();
        ensure_created_webview(state, webview)?;
        dispatch_android_webview_command(state, command.clone())
    }

    fn perform_updates(&mut self, webview: WebViewHandle) -> Result<Vec<HostEvent>, HostError> {
        let state = self.state_mut();
        ensure_created_webview(state, webview)?;

        let result = {
            let Some(backend) = state.android_backend.as_mut() else {
                return Ok(Vec::new());
            };
            backend.perform_updates()
        };
        let current_url = state.current_url.clone();
        Ok(events_from_backend_result(result, current_url.as_deref()))
    }

    fn observe_webview_events(
        &mut self,
        webview: WebViewHandle,
        events: &[HostEvent],
    ) -> Result<(), HostError> {
        let state = self.state_mut();
        ensure_created_webview(state, webview)?;
        for event in events {
            state.observe_event(event);
        }
        Ok(())
    }

    fn synthesize_webview_command_events(&self) -> bool {
        false
    }
}

fn ensure_created_webview(
    state: &AndroidHostState,
    webview: WebViewHandle,
) -> Result<(), HostError> {
    (state.created_webview == Some(webview))
        .then_some(())
        .ok_or_else(|| HostError::new(format!("webview {} is not registered", webview.raw())))
}

fn events_from_backend_result(
    result: Result<Vec<HostEvent>, String>,
    diagnostic_url: Option<&str>,
) -> Vec<HostEvent> {
    match result {
        Ok(events) => events,
        Err(message) => vec![HostEvent::Error {
            url: diagnostic_url.map(str::to_owned),
            code: ServoStatus::BackendError as i32,
            message,
        }],
    }
}

fn dispatch_android_webview_command(
    state: &mut AndroidHostState,
    command: WebViewCommand,
) -> Result<Vec<HostEvent>, HostError> {
    Ok(match command {
        WebViewCommand::LoadUrl(request) => load_navigation_request_for_state(state, request),
        WebViewCommand::StopLoading => {
            return Err(HostError::new(
                "native stop loading is unsupported by Servo",
            ));
        }
        WebViewCommand::Reload => {
            perform_android_navigation_command_for_state(state, |backend| backend.reload())
        }
        WebViewCommand::GoBack => {
            perform_android_navigation_command_for_state(state, |backend| backend.go_back())
        }
        WebViewCommand::GoForward => {
            perform_android_navigation_command_for_state(state, |backend| backend.go_forward())
        }
        WebViewCommand::Focus => {
            perform_android_navigation_command_for_state(state, |backend| backend.focus())
        }
        WebViewCommand::Blur => {
            perform_android_navigation_command_for_state(state, |backend| backend.blur())
        }
    })
}

fn load_navigation_request_for_state(
    state: &mut AndroidHostState,
    request: NavigationRequest,
) -> Vec<HostEvent> {
    let url = request.url;

    if let Err(message) = install_android_backend_for_state(state, None) {
        return vec![HostEvent::Error {
            url: Some(url),
            code: ServoStatus::BackendError as i32,
            message,
        }];
    }

    state.pending_url = Some(url.clone());

    if let (Some(_), Some(_), Some(backend)) = (
        state.surface,
        state.native_window,
        state.android_backend.as_mut(),
    ) {
        state.pending_url = None;
        let result = backend.load_url(&url);
        return events_from_backend_result(result, Some(&url));
    }

    Vec::new()
}

fn perform_android_navigation_command_for_state(
    state: &mut AndroidHostState,
    command: impl FnOnce(&mut dyn AndroidRenderBackend) -> Result<Vec<HostEvent>, String>,
) -> Vec<HostEvent> {
    let result = {
        let Some(backend) = state.android_backend.as_mut() else {
            return Vec::new();
        };
        command(backend.as_mut())
    };
    let current_url = state.current_url.clone();
    events_from_backend_result(result, current_url.as_deref())
}

fn install_android_backend_for_state(
    state: &mut AndroidHostState,
    initial_url: Option<&str>,
) -> Result<(), String> {
    if state.android_backend.is_some() {
        return Ok(());
    }

    let Some(native_window) = state.native_window else {
        return Ok(());
    };
    let Some(surface) = state.surface else {
        return Ok(());
    };
    let Some(surface_density) = state.surface_density else {
        return Ok(());
    };

    #[cfg(target_os = "android")]
    {
        state.android_backend = Some(Box::new(crate::android_backend::ServoAndroidBackend::new(
            native_window,
            surface,
            surface_density,
            initial_url,
        )?));
    }

    #[cfg(not(target_os = "android"))]
    let _ = (native_window, surface, surface_density, initial_url);

    Ok(())
}

#[cfg(test)]
impl HostHandle {
    pub(crate) fn set_android_backend_for_tests(&mut self, backend: Box<dyn AndroidRenderBackend>) {
        self.android_backend = Some(backend);
    }

    pub(crate) fn load_android_request_for_tests(&mut self, request: NavigationRequest) {
        crate::commands::load_navigation_request(self, request);
    }

    pub(crate) fn attach_android_surface_for_tests(
        &mut self,
        native_window: NativeWindowHandle,
        size: SurfaceSize,
        density: f32,
    ) {
        crate::commands::attach_surface_with_native_window(self, native_window, size, density);
    }

    pub(crate) fn perform_android_updates_for_tests(&mut self) {
        crate::commands::perform_android_updates(self);
    }
}
