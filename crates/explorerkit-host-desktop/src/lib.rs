//! Package-private desktop bridge for the React Native AppKit and Win32 adapters.
//!
//! This crate intentionally exposes only a C ABI for package-owned native code. It is not part of
//! the public ExplorerKit Rust facade or a general-purpose C SDK.

#![cfg(any(target_os = "macos", target_os = "windows"))]

use std::cell::UnsafeCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::{c_char, c_void, CStr, CString};
use std::mem;
#[cfg(target_os = "windows")]
use std::num::NonZeroIsize;
use std::num::NonZeroU64;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::{self, NonNull};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::thread::{self, ThreadId};

use explorerkit::events::HostEvent;
use explorerkit::host::Host;
use explorerkit::input::{
    HostInputEvent, KeyboardInputEvent, KeyboardInputKey, KeyboardInputState, KeyboardNamedKey,
    PointerButton, PointerButtonAction, PointerInputEvent, PointerScrollMode,
};
use explorerkit::runtime::{ensure_default_rustls_crypto_provider, Runtime, RuntimeError};
use explorerkit::surface::{
    HostSurface, MemoryClipboard, NativeSurface as ServoNativeSurface, SurfaceDelegate,
    SurfaceError, SurfaceFrame, SurfaceHost, SurfaceHostOptions, SurfacePoint, SurfaceSize,
    SurfaceTarget, SurfaceViewport,
};
use explorerkit::webview::{WebViewCommand, WebViewHandle};
use explorerkit_embedder::{encode_host_event_bridge, ControllerCommand};
#[cfg(target_os = "macos")]
use raw_window_handle::{AppKitDisplayHandle, AppKitWindowHandle};
use raw_window_handle::{DisplayHandle, RawDisplayHandle, RawWindowHandle, WindowHandle};
#[cfg(target_os = "windows")]
use raw_window_handle::{Win32WindowHandle, WindowsDisplayHandle};

const SURFACE_ID: &str = "react-native-desktop-explorer-view";
const INPUT_POINTER_MOVE: u32 = 0;
const INPUT_POINTER_BUTTON: u32 = 1;
const INPUT_POINTER_WHEEL: u32 = 2;
const INPUT_POINTER_LEAVE: u32 = 3;
const INPUT_KEYBOARD: u32 = 4;
const INPUT_IME_COMMIT: u32 = 5;
const INPUT_FOCUS: u32 = 6;
const POINTER_BUTTON_PRIMARY: u32 = 0;
const POINTER_BUTTON_SECONDARY: u32 = 1;
const POINTER_BUTTON_MIDDLE: u32 = 2;
const POINTER_BUTTON_BACK: u32 = 3;
const POINTER_BUTTON_FORWARD: u32 = 4;
const POINTER_BUTTON_OTHER: u32 = 5;
const KEY_KIND_CHARACTER: u32 = 0;
const KEY_KIND_NAMED: u32 = 1;
const MAX_COMMAND_BYTES: usize = 1024 * 1024;

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum TestPanic {
    Viewport,
    Command,
    Input,
    Pump,
    EventCapture,
    EventDrain,
}

#[cfg(test)]
thread_local! {
    static TEST_PANIC: std::cell::Cell<Option<TestPanic>> = const { std::cell::Cell::new(None) };
    static TEST_VIEWPORT_HOST_ERROR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
macro_rules! inject_test_panic {
    ($point:ident) => {
        TEST_PANIC.with(|fault| {
            if fault.get() == Some(TestPanic::$point) {
                fault.set(None);
                panic!(concat!("injected ", stringify!($point), " panic"));
            }
        })
    };
}

#[cfg(not(test))]
macro_rules! inject_test_panic {
    ($point:ident) => {};
}

type WakeCallback = unsafe extern "C" fn(*mut c_void, u64);
type EventCallback = unsafe extern "C" fn(*mut c_void, u64, u64, *const c_char);

/// Status values returned by every package-private bridge mutation.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplorerKitDesktopPrivateStatus {
    Ok = 0,
    NullPointer = 1,
    InvalidArgument = 2,
    StaleToken = 3,
    WrongThread = 4,
    Busy = 5,
    RuntimeError = 6,
    Panic = 7,
}

/// Callbacks retained for the lifetime of a private desktop host.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExplorerKitDesktopPrivateCallbacks {
    pub context: *mut c_void,
    pub wake: Option<WakeCallback>,
}

/// AppKit `NSView` or Win32 `HWND` plus the optional Win32 `HINSTANCE`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExplorerKitDesktopPrivateNativeSurface {
    pub window: *mut c_void,
    pub display: *mut c_void,
}

/// Child-local physical-pixel viewport with a top-left origin.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ExplorerKitDesktopPrivateViewport {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
}

/// Flat C representation translated into ExplorerKit's existing `HostInputEvent` types.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExplorerKitDesktopPrivateInput {
    pub kind: u32,
    pub x: f32,
    pub y: f32,
    pub delta_x: f64,
    pub delta_y: f64,
    pub button_kind: u32,
    pub button_code: u32,
    pub action: u32,
    pub scroll_mode: u32,
    pub key_kind: u32,
    pub key_code: u32,
    pub text: *const c_char,
    pub repeat: u8,
    pub is_composing: u8,
    pub is_focused: u8,
}

impl From<RuntimeError> for ExplorerKitDesktopPrivateStatus {
    fn from(_error: RuntimeError) -> Self {
        Self::RuntimeError
    }
}

type BridgeResult<T = ()> = Result<T, ExplorerKitDesktopPrivateStatus>;

fn invalid<T>() -> BridgeResult<T> {
    Err(ExplorerKitDesktopPrivateStatus::InvalidArgument)
}

struct Controller<H> {
    runtime: Runtime<H>,
    webview: WebViewHandle,
}

impl<H: Host> Controller<H> {
    fn new(host: H) -> Result<Self, RuntimeError> {
        let mut runtime = Runtime::new(host);
        let session = runtime.create_session();
        let webview = runtime.create_webview(session)?;
        Ok(Self { runtime, webview })
    }

    fn dispatch_controller_command(&mut self, command_json: &str) -> BridgeResult {
        let command = ControllerCommand::from_json(command_json)
            .map_err(|_| ExplorerKitDesktopPrivateStatus::InvalidArgument)?;
        let result = match command {
            ControllerCommand::LoadUrl(request) => self
                .runtime
                .dispatch_webview_command(self.webview, WebViewCommand::LoadUrl(request)),
            ControllerCommand::Reload => self.runtime.reload(self.webview),
            ControllerCommand::StopLoading => self.runtime.stop_loading(self.webview),
            ControllerCommand::GoBack => self.runtime.go_back(self.webview),
            ControllerCommand::GoForward => self.runtime.go_forward(self.webview),
            ControllerCommand::Focus => self.runtime.focus(self.webview),
            ControllerCommand::Blur => self.runtime.blur(self.webview),
            ControllerCommand::EvaluateJavaScript {
                evaluation_id,
                script,
            } => self
                .runtime
                .evaluate_javascript(self.webview, evaluation_id, script),
            ControllerCommand::ResolveNavigationRequest {
                navigation_id,
                allow,
            } => self
                .runtime
                .resolve_navigation_request(self.webview, navigation_id, allow),
            ControllerCommand::ResolveSimpleDialog {
                dialog_id,
                confirmed,
                prompt_value,
            } => self.runtime.resolve_simple_dialog(
                self.webview,
                dialog_id,
                confirmed,
                prompt_value.as_deref(),
            ),
            ControllerCommand::ResolveContextMenu {
                context_menu_id,
                action,
            } => self
                .runtime
                .resolve_context_menu(self.webview, context_menu_id, action),
            ControllerCommand::DismissContextMenu { context_menu_id } => self
                .runtime
                .dismiss_context_menu(self.webview, context_menu_id),
        };
        result.map_err(Into::into)
    }
}

#[derive(Clone, Copy)]
struct NativeSurface {
    window: NonNull<c_void>,
    #[cfg(target_os = "windows")]
    display: *mut c_void,
}

#[derive(Default)]
struct NativeSurfaceSlot(UnsafeCell<Option<NativeSurface>>);

impl NativeSurfaceSlot {
    fn get(&self) -> Option<&NativeSurface> {
        // SAFETY: bridge calls are serialized on the owning UI thread. The slot is only cleared
        // after Servo has returned every borrowed render target at detach/rollback.
        unsafe { (&*self.0.get()).as_ref() }
    }

    /// # Safety
    ///
    /// No render target borrowed from the current surface may still be live.
    unsafe fn set(&self, native: Option<NativeSurface>) {
        unsafe { *self.0.get() = native };
    }
}

#[derive(Clone, Default)]
struct DesktopSurface {
    native: Rc<NativeSurfaceSlot>,
}

impl SurfaceDelegate for DesktopSurface {
    type Frame<'a> = SurfaceFrame<'a>;

    fn render_target(
        &mut self,
        _surface: &HostSurface,
        _viewport: SurfaceViewport,
    ) -> Result<SurfaceTarget<'_>, SurfaceError> {
        let native = self
            .native
            .get()
            .ok_or_else(|| SurfaceError::new("desktop native child surface is detached"))?;
        native_child_surface(native).map(Into::into)
    }
}

#[cfg(target_os = "macos")]
fn native_child_surface(native: &NativeSurface) -> Result<ServoNativeSurface<'_>, SurfaceError> {
    let display =
        unsafe { DisplayHandle::borrow_raw(RawDisplayHandle::AppKit(AppKitDisplayHandle::new())) };
    let window = unsafe {
        WindowHandle::borrow_raw(RawWindowHandle::AppKit(AppKitWindowHandle::new(
            native.window,
        )))
    };
    Ok(ServoNativeSurface::new(display, window))
}

#[cfg(target_os = "windows")]
fn native_child_surface(native: &NativeSurface) -> Result<ServoNativeSurface<'_>, SurfaceError> {
    let hwnd = NonZeroIsize::new(native.window.as_ptr() as isize)
        .ok_or_else(|| SurfaceError::new("Win32 child HWND must not be null"))?;
    let mut raw_window = Win32WindowHandle::new(hwnd);
    raw_window.hinstance = NonZeroIsize::new(native.display as isize);
    let display = unsafe {
        DisplayHandle::borrow_raw(RawDisplayHandle::Windows(WindowsDisplayHandle::new()))
    };
    let window = unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(raw_window)) };
    Ok(ServoNativeSurface::new(display, window))
}

struct CallbackGate {
    token: u64,
    callback: Mutex<Option<(usize, WakeCallback)>>,
}

impl CallbackGate {
    fn new(token: u64, callbacks: ExplorerKitDesktopPrivateCallbacks) -> Self {
        Self {
            token,
            callback: Mutex::new(
                callbacks
                    .wake
                    .map(|wake| (callbacks.context as usize, wake)),
            ),
        }
    }

    fn wake(&self) {
        let callback = lock_unpoisoned(&self.callback);
        if let Some((context, callback)) = *callback {
            unsafe { callback(context as *mut c_void, self.token) };
        }
    }

    fn deactivate(&self) {
        *lock_unpoisoned(&self.callback) = None;
    }
}

/// Opaque package-private host. Its runtime and controller identity remain Rust-owned.
#[repr(C)]
pub struct ExplorerKitDesktopPrivateHost {
    controller: Controller<SurfaceHost<DesktopSurface>>,
    surface: DesktopSurface,
    callbacks: Arc<CallbackGate>,
    events: BridgeEvents,
}

impl ExplorerKitDesktopPrivateHost {
    fn attach(&mut self, native: NativeSurface, viewport: SurfaceViewport) -> BridgeResult<u64> {
        if self.surface.native.get().is_some() {
            return Err(ExplorerKitDesktopPrivateStatus::InvalidArgument);
        }
        validate_native_surface_thread(native)?;
        let generation = allocate_id().ok_or(ExplorerKitDesktopPrivateStatus::RuntimeError)?;
        let native_slot = self.surface.native.clone();
        // SAFETY: no surface is attached, and calls are serialized on the owning UI thread.
        unsafe { native_slot.set(Some(native)) };
        let rollback = NativeAttachRollback::new(&native_slot);
        self.controller.runtime.attach_surface_with_viewport(
            self.controller.webview,
            HostSurface::new(SURFACE_ID),
            viewport,
        )?;
        self.events.latest_attachment_generation = Some(generation);
        rollback.commit();
        Ok(generation)
    }

    fn detach(&mut self) -> BridgeResult {
        if self.surface.native.get().is_none() {
            return Ok(());
        }
        self.controller
            .runtime
            .detach_surface(self.controller.webview)?;
        // SAFETY: runtime detach returned, so Servo no longer borrows the native handles.
        unsafe { self.surface.native.set(None) };
        Ok(())
    }

    fn capture_events(&mut self) {
        inject_test_panic!(EventCapture);
        self.events.capture(&mut self.controller);
    }
}

struct NativeAttachRollback<'a> {
    native: &'a NativeSurfaceSlot,
    armed: bool,
}

impl<'a> NativeAttachRollback<'a> {
    fn new(native: &'a NativeSurfaceSlot) -> Self {
        Self {
            native,
            armed: true,
        }
    }

    fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for NativeAttachRollback<'_> {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: SurfaceHost rolls a partial attach back before unwinding reaches this guard.
            unsafe { self.native.set(None) };
        }
    }
}

#[derive(Debug)]
struct QueuedEvent {
    attachment_generation: u64,
    json: String,
}

#[derive(Default)]
struct BridgeEvents {
    latest_attachment_generation: Option<u64>,
    pending: VecDeque<QueuedEvent>,
}

impl BridgeEvents {
    fn capture<H: Host>(&mut self, controller: &mut Controller<H>) {
        for event in controller
            .runtime
            .drain_events()
            .into_iter()
            .filter(|event| {
                event.webview == controller.webview
                    && !matches!(event.event, HostEvent::FaviconChanged { .. })
            })
        {
            let attachment_generation = match &event.event {
                // Runtime synthesizes only these events from surface operations. Every other
                // HostEvent originates from the retained controller/Servo delegate and therefore
                // survives native surface replacement.
                HostEvent::SurfaceAttached { .. }
                | HostEvent::SurfaceResized { .. }
                | HostEvent::SurfaceDetached => {
                    let Some(generation) = self.latest_attachment_generation else {
                        continue;
                    };
                    generation
                }
                _ => 0,
            };
            self.pending.push_back(QueuedEvent {
                attachment_generation,
                json: encode_host_event_bridge(&event.event),
            });
        }
    }

    fn drain(&mut self) -> BridgeResult<Vec<QueuedCallbackEvent>> {
        inject_test_panic!(EventDrain);
        let latest_generation = self.latest_attachment_generation;
        self.pending
            .drain(..)
            .filter(|event| {
                event.attachment_generation == 0
                    || Some(event.attachment_generation) == latest_generation
            })
            .map(|event| {
                Ok(QueuedCallbackEvent {
                    attachment_generation: event.attachment_generation,
                    json: CString::new(event.json)
                        .map_err(|_| ExplorerKitDesktopPrivateStatus::RuntimeError)?,
                })
            })
            .collect()
    }
}

struct QueuedCallbackEvent {
    attachment_generation: u64,
    json: CString,
}

#[derive(Clone, Copy)]
struct LiveHost {
    token: u64,
    owner: ThreadId,
    busy: bool,
}

struct IdAllocator {
    next: Option<NonZeroU64>,
}

impl Default for IdAllocator {
    fn default() -> Self {
        Self {
            next: NonZeroU64::new(1),
        }
    }
}

impl IdAllocator {
    fn allocate(&mut self) -> Option<u64> {
        let id = self.next?;
        self.next = id.get().checked_add(1).and_then(NonZeroU64::new);
        Some(id.get())
    }

    #[cfg(test)]
    fn starting_at(id: u64) -> Self {
        Self {
            next: NonZeroU64::new(id),
        }
    }
}

#[derive(Default)]
struct HostRegistry {
    live: HashMap<usize, LiveHost>,
    ids: IdAllocator,
}

impl HostRegistry {
    fn allocate_id(&mut self) -> Option<u64> {
        self.ids.allocate()
    }

    fn register(&mut self, address: usize, token: u64, owner: ThreadId) -> bool {
        if self.live.contains_key(&address) {
            return false;
        }
        self.live.insert(
            address,
            LiveHost {
                token,
                owner,
                busy: false,
            },
        );
        true
    }
}

static HOST_REGISTRY: LazyLock<Mutex<HostRegistry>> =
    LazyLock::new(|| Mutex::new(HostRegistry::default()));

struct CallGuard {
    address: usize,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        if let Some(live) = lock_unpoisoned(&HOST_REGISTRY).live.get_mut(&self.address) {
            live.busy = false;
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn allocate_id() -> Option<u64> {
    lock_unpoisoned(&HOST_REGISTRY).allocate_id()
}

fn register_host(address: usize, token: u64) -> bool {
    lock_unpoisoned(&HOST_REGISTRY).register(address, token, thread::current().id())
}

fn catch_boundary<T>(operation: impl FnOnce() -> T) -> Result<T, ()> {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(value) => Ok(value),
        Err(payload) => {
            // A custom panic payload may panic in Drop; leak it rather than unwind across C.
            mem::forget(payload);
            Err(())
        }
    }
}

fn begin_call(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
) -> Result<CallGuard, ExplorerKitDesktopPrivateStatus> {
    if host.is_null() {
        return Err(ExplorerKitDesktopPrivateStatus::NullPointer);
    }
    let address = host as usize;
    let mut registry = lock_unpoisoned(&HOST_REGISTRY);
    let Some(live) = registry.live.get_mut(&address) else {
        return Err(ExplorerKitDesktopPrivateStatus::StaleToken);
    };
    if live.token != token {
        return Err(ExplorerKitDesktopPrivateStatus::StaleToken);
    }
    if live.owner != thread::current().id() {
        return Err(ExplorerKitDesktopPrivateStatus::WrongThread);
    }
    if live.busy {
        return Err(ExplorerKitDesktopPrivateStatus::Busy);
    }
    live.busy = true;
    Ok(CallGuard { address })
}

fn with_host(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
    operation: impl FnOnce(&mut ExplorerKitDesktopPrivateHost) -> BridgeResult,
) -> ExplorerKitDesktopPrivateStatus {
    match accepted_host_call(host, token, |host| {
        let host = unsafe { &mut *host };
        // Capture before the operation so a failed or panicking attach cannot strand older
        // controller events in Runtime where a later attachment could misclassify them.
        host.capture_events();
        let result = operation(host);
        host.capture_events();
        result
    }) {
        Ok((_guard, ())) => ExplorerKitDesktopPrivateStatus::Ok,
        Err(status) => status,
    }
}

fn accepted_host_call<T>(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
    operation: impl FnOnce(*mut ExplorerKitDesktopPrivateHost) -> BridgeResult<T>,
) -> Result<(CallGuard, T), ExplorerKitDesktopPrivateStatus> {
    let mut accepted = false;
    match catch_boundary(|| {
        let guard = begin_call(host, token)?;
        accepted = true;
        operation(host).map(|value| (guard, value))
    }) {
        Ok(result) => result,
        Err(()) => {
            if accepted {
                retire_accepted_host(host, token);
            }
            Err(ExplorerKitDesktopPrivateStatus::Panic)
        }
    }
}

fn retire_accepted_host(host: *mut ExplorerKitDesktopPrivateHost, token: u64) {
    let address = host as usize;
    let retired = {
        let mut registry = lock_unpoisoned(&HOST_REGISTRY);
        if registry
            .live
            .get(&address)
            .is_some_and(|live| live.token == token)
        {
            registry.live.remove(&address);
            true
        } else {
            false
        }
    };
    if !retired {
        return;
    }

    let mut host = unsafe { Box::from_raw(host) };
    host.callbacks.deactivate();
    let _ = catch_boundary(|| {
        if host.surface.native.get().is_some() {
            let _ = host.detach();
        }
    });
    let _ = catch_boundary(|| drop(host));
}

/// # Safety
///
/// `host` must be the allocation registered with `token`. `begin_call` validates the registry
/// entry and owning thread before this function reconstructs the `Box`.
unsafe fn take_registered_host(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
) -> Result<Box<ExplorerKitDesktopPrivateHost>, ExplorerKitDesktopPrivateStatus> {
    let guard = begin_call(host, token)?;
    lock_unpoisoned(&HOST_REGISTRY)
        .live
        .remove(&(host as usize));
    drop(guard);
    Ok(unsafe { Box::from_raw(host) })
}

fn finish_host(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
    detach: bool,
) -> ExplorerKitDesktopPrivateStatus {
    catch_boundary(|| {
        let mut host = match unsafe { take_registered_host(host, token) } {
            Ok(host) => host,
            Err(status) => return status,
        };
        host.callbacks.deactivate();
        if detach && host.surface.native.get().is_some() {
            match host.detach() {
                Ok(()) => ExplorerKitDesktopPrivateStatus::Ok,
                Err(status) => status,
            }
        } else {
            ExplorerKitDesktopPrivateStatus::Ok
        }
    })
    .unwrap_or(ExplorerKitDesktopPrivateStatus::Panic)
}

fn viewport(value: ExplorerKitDesktopPrivateViewport) -> BridgeResult<SurfaceViewport> {
    if value.width == 0 || value.height == 0 {
        return invalid();
    }
    if !value.scale_factor.is_finite() || value.scale_factor <= 0.0 {
        return invalid();
    }
    Ok(SurfaceViewport::with_origin(
        SurfacePoint::new(value.x, value.y),
        SurfaceSize::new(value.width, value.height),
        value.scale_factor,
    ))
}

fn native_surface(value: ExplorerKitDesktopPrivateNativeSurface) -> BridgeResult<NativeSurface> {
    Ok(NativeSurface {
        window: NonNull::new(value.window)
            .ok_or(ExplorerKitDesktopPrivateStatus::InvalidArgument)?,
        #[cfg(target_os = "windows")]
        display: value.display,
    })
}

/// # Safety
///
/// `value` must point to a NUL-terminated string that remains valid for the duration of the call.
unsafe fn required_string(value: *const c_char) -> BridgeResult<String> {
    if value.is_null() {
        return invalid();
    }
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| ExplorerKitDesktopPrivateStatus::InvalidArgument)
}

/// # Safety
///
/// A non-null `value` must point to `length` readable bytes for the duration of the returned
/// borrow. The bytes need not be NUL-terminated.
unsafe fn required_command_json<'a>(value: *const u8, length: usize) -> BridgeResult<&'a str> {
    if value.is_null() || length == 0 || length > MAX_COMMAND_BYTES {
        return invalid();
    }
    let bytes = unsafe { std::slice::from_raw_parts(value, length) };
    std::str::from_utf8(bytes).map_err(|_| ExplorerKitDesktopPrivateStatus::InvalidArgument)
}

fn input_event(value: ExplorerKitDesktopPrivateInput) -> BridgeResult<HostInputEvent> {
    match value.kind {
        INPUT_POINTER_MOVE => {
            validate_point(value.x, value.y)?;
            Ok(HostInputEvent::Pointer(PointerInputEvent::moved(
                value.x, value.y,
            )))
        }
        INPUT_POINTER_BUTTON => {
            validate_point(value.x, value.y)?;
            let action = match value.action {
                0 => PointerButtonAction::Released,
                1 => PointerButtonAction::Pressed,
                _ => return invalid(),
            };
            let button = pointer_button(value.button_kind, value.button_code)?;
            Ok(HostInputEvent::Pointer(PointerInputEvent::button(
                action, button, value.x, value.y,
            )))
        }
        INPUT_POINTER_WHEEL => {
            validate_point(value.x, value.y)?;
            if !value.delta_x.is_finite() || !value.delta_y.is_finite() {
                return invalid();
            }
            let mode = match value.scroll_mode {
                0 => PointerScrollMode::Lines,
                1 => PointerScrollMode::Pixels,
                _ => return invalid(),
            };
            Ok(HostInputEvent::Pointer(PointerInputEvent::wheel(
                value.delta_x,
                value.delta_y,
                mode,
                value.x,
                value.y,
            )))
        }
        INPUT_POINTER_LEAVE => Ok(HostInputEvent::Pointer(PointerInputEvent::left_viewport())),
        INPUT_KEYBOARD => {
            let state = match value.action {
                0 => KeyboardInputState::Released,
                1 => KeyboardInputState::Pressed,
                _ => return invalid(),
            };
            Ok(HostInputEvent::Keyboard(KeyboardInputEvent {
                key: keyboard_key(value.key_kind, value.key_code, value.text)?,
                state,
                repeat: byte_bool(value.repeat)?,
                is_composing: byte_bool(value.is_composing)?,
            }))
        }
        INPUT_IME_COMMIT => {
            let text = unsafe { required_string(value.text)? };
            if text.is_empty() {
                return invalid();
            }
            Ok(HostInputEvent::ImeCommit { text })
        }
        INPUT_FOCUS => Ok(HostInputEvent::Focus {
            is_focused: byte_bool(value.is_focused)?,
        }),
        _ => invalid(),
    }
}

fn validate_point(x: f32, y: f32) -> BridgeResult {
    if x.is_finite() && y.is_finite() {
        Ok(())
    } else {
        invalid()
    }
}

fn byte_bool(value: u8) -> BridgeResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => invalid(),
    }
}

fn pointer_button(kind: u32, code: u32) -> BridgeResult<PointerButton> {
    let known = match kind {
        POINTER_BUTTON_PRIMARY => Some(PointerButton::Primary),
        POINTER_BUTTON_SECONDARY => Some(PointerButton::Secondary),
        POINTER_BUTTON_MIDDLE => Some(PointerButton::Middle),
        POINTER_BUTTON_BACK => Some(PointerButton::Back),
        POINTER_BUTTON_FORWARD => Some(PointerButton::Forward),
        POINTER_BUTTON_OTHER => None,
        _ => return invalid(),
    };
    match known {
        Some(button) if code == 0 => Ok(button),
        Some(_) => invalid(),
        None => u16::try_from(code)
            .map(PointerButton::Other)
            .map_err(|_| ExplorerKitDesktopPrivateStatus::InvalidArgument),
    }
}

fn keyboard_key(kind: u32, code: u32, text: *const c_char) -> BridgeResult<KeyboardInputKey> {
    match kind {
        KEY_KIND_CHARACTER if code == 0 => {
            let text = unsafe { required_string(text)? };
            if text.is_empty() {
                invalid()
            } else {
                Ok(KeyboardInputKey::Character(text))
            }
        }
        KEY_KIND_NAMED if text.is_null() => named_key(code).map(KeyboardInputKey::Named),
        _ => invalid(),
    }
}

fn named_key(code: u32) -> BridgeResult<KeyboardNamedKey> {
    match code {
        0 => Ok(KeyboardNamedKey::Backspace),
        1 => Ok(KeyboardNamedKey::Delete),
        2 => Ok(KeyboardNamedKey::Enter),
        3 => Ok(KeyboardNamedKey::Tab),
        4 => Ok(KeyboardNamedKey::Escape),
        5 => Ok(KeyboardNamedKey::Space),
        6 => Ok(KeyboardNamedKey::ArrowLeft),
        7 => Ok(KeyboardNamedKey::ArrowRight),
        8 => Ok(KeyboardNamedKey::ArrowUp),
        9 => Ok(KeyboardNamedKey::ArrowDown),
        _ => invalid(),
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    fn pthread_main_np() -> std::ffi::c_int;
}

#[cfg(target_os = "macos")]
fn platform_ui_thread() -> bool {
    unsafe { pthread_main_np() != 0 }
}

#[cfg(target_os = "windows")]
fn platform_ui_thread() -> bool {
    true
}

#[cfg(target_os = "macos")]
fn validate_native_surface_thread(_native: NativeSurface) -> BridgeResult {
    if platform_ui_thread() {
        Ok(())
    } else {
        Err(ExplorerKitDesktopPrivateStatus::WrongThread)
    }
}

#[cfg(target_os = "windows")]
#[link(name = "user32")]
extern "system" {
    fn GetWindowThreadProcessId(window: *mut c_void, process_id: *mut u32) -> u32;
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadId() -> u32;
}

#[cfg(target_os = "windows")]
fn validate_native_surface_thread(native: NativeSurface) -> BridgeResult {
    let owner = unsafe { GetWindowThreadProcessId(native.window.as_ptr(), ptr::null_mut()) };
    let current = unsafe { GetCurrentThreadId() };
    if owner != 0 && owner == current {
        Ok(())
    } else {
        Err(ExplorerKitDesktopPrivateStatus::WrongThread)
    }
}

fn build_host(
    token: u64,
    callbacks: ExplorerKitDesktopPrivateCallbacks,
) -> Result<Box<ExplorerKitDesktopPrivateHost>, RuntimeError> {
    let callback_gate = Arc::new(CallbackGate::new(token, callbacks));
    let waker_gate = callback_gate.clone();
    let options = SurfaceHostOptions::new(
        Arc::new(move || waker_gate.wake()),
        Rc::new(MemoryClipboard::default()),
    );
    let surface = DesktopSurface::default();
    let controller = Controller::new(SurfaceHost::new(surface.clone(), options))?;
    Ok(Box::new(ExplorerKitDesktopPrivateHost {
        controller,
        surface,
        callbacks: callback_gate,
        events: BridgeEvents::default(),
    }))
}

/// Create one Rust-owned ExplorerKit controller and return its opaque generation token.
///
/// AppKit callers must create on the application main thread. Win32 callers must create on the
/// thread that owns the child `HWND`. The callback context must remain valid until accepted
/// destroy or a host-consuming failure returns. A detached/state-precondition viewport
/// [`RuntimeError`] is nonterminal and leaves the token valid; a host failure after accepted
/// viewport preflight consumes the token, deactivates callbacks, and makes every later export
/// stale. Wake callbacks may run off the UI thread. Foreign callbacks must contain every
/// exception, unwind, or non-local jump before returning through this ABI.
///
/// # Safety
///
/// `out_host` and `out_token` must be valid, writable pointers. Callback function pointers and
/// their context must remain valid until destroy returns.
#[no_mangle]
pub unsafe extern "C" fn explorerkit_desktop_private_create(
    callbacks: ExplorerKitDesktopPrivateCallbacks,
    out_host: *mut *mut ExplorerKitDesktopPrivateHost,
    out_token: *mut u64,
) -> ExplorerKitDesktopPrivateStatus {
    catch_boundary(|| {
        if out_host.is_null() || out_token.is_null() {
            return ExplorerKitDesktopPrivateStatus::NullPointer;
        }
        unsafe {
            out_host.write(ptr::null_mut());
            out_token.write(0);
        }
        if !platform_ui_thread() {
            return ExplorerKitDesktopPrivateStatus::WrongThread;
        }

        let _ = ensure_default_rustls_crypto_provider();
        let Some(token) = allocate_id() else {
            return ExplorerKitDesktopPrivateStatus::RuntimeError;
        };
        let host = match build_host(token, callbacks) {
            Ok(host) => host,
            Err(_) => return ExplorerKitDesktopPrivateStatus::RuntimeError,
        };
        let address = (&*host as *const ExplorerKitDesktopPrivateHost) as usize;
        if !register_host(address, token) {
            return ExplorerKitDesktopPrivateStatus::RuntimeError;
        }
        let host = Box::into_raw(host);
        unsafe {
            out_host.write(host);
            out_token.write(token);
        }
        ExplorerKitDesktopPrivateStatus::Ok
    })
    .unwrap_or(ExplorerKitDesktopPrivateStatus::Panic)
}

/// Deterministically detach and drop the Servo webview/runtime, then disable future callbacks.
/// Native handles from a successful attach remain borrowed until this function returns.
///
/// # Safety
///
/// `host` and `token` must be the live pair returned by create and used on its owning UI thread.
#[no_mangle]
pub unsafe extern "C" fn explorerkit_desktop_private_destroy(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
) -> ExplorerKitDesktopPrivateStatus {
    finish_host(host, token, true)
}

/// Attach an app-owned native child surface through Servo's `WindowRenderingContext` path.
///
/// # Safety
///
/// The host/token pair must be live on its owning UI thread. `out_attachment_generation` must be
/// writable. Attach failure does not retain the native handles. Attach success borrows them until
/// an accepted detach or destroy returns. An accepted runtime error or panic consumes the host;
/// callers must create a new host rather than retrying or destroying the stale token.
#[no_mangle]
pub unsafe extern "C" fn explorerkit_desktop_private_attach(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
    surface: ExplorerKitDesktopPrivateNativeSurface,
    viewport_value: ExplorerKitDesktopPrivateViewport,
    out_attachment_generation: *mut u64,
) -> ExplorerKitDesktopPrivateStatus {
    if out_attachment_generation.is_null() {
        return ExplorerKitDesktopPrivateStatus::NullPointer;
    }
    unsafe { out_attachment_generation.write(0) };
    let mut generation = 0;
    let status = with_host(host, token, |host| {
        let native = native_surface(surface)?;
        let viewport = viewport(viewport_value)?;
        generation = host.attach(native, viewport)?;
        Ok(())
    });
    if status == ExplorerKitDesktopPrivateStatus::Ok {
        unsafe { out_attachment_generation.write(generation) };
    } else if status == ExplorerKitDesktopPrivateStatus::RuntimeError {
        // An accepted runtime attach failure consumes the host so no retry path can retain or
        // reacquire the app-owned handles after this call returns.
        let _ = finish_host(host, token, true);
    }
    status
}

/// Update Servo's physical size and HiDPI scale before the next paint.
///
/// # Safety
///
/// The host/token pair must be live and used on its owning UI thread.
#[no_mangle]
pub unsafe extern "C" fn explorerkit_desktop_private_update_viewport(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
    viewport_value: ExplorerKitDesktopPrivateViewport,
) -> ExplorerKitDesktopPrivateStatus {
    let mut host_failed = false;
    let status = with_host(host, token, |host| {
        let viewport = viewport(viewport_value)?;
        inject_test_panic!(Viewport);
        #[cfg(test)]
        let result = if TEST_VIEWPORT_HOST_ERROR.replace(false) {
            Err(RuntimeError::Host(explorerkit::host::HostError::new(
                "injected viewport host error",
            )))
        } else {
            host.controller
                .runtime
                .update_surface_viewport(host.controller.webview, viewport)
        };
        #[cfg(not(test))]
        let result = host
            .controller
            .runtime
            .update_surface_viewport(host.controller.webview, viewport);
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                host_failed = matches!(&error, RuntimeError::Host(_));
                Err(error.into())
            }
        }
    });
    if host_failed && status == ExplorerKitDesktopPrivateStatus::RuntimeError {
        let teardown_status = finish_host(host, token, false);
        if teardown_status == ExplorerKitDesktopPrivateStatus::Panic {
            return teardown_status;
        }
    }
    status
}

/// Detach Servo's rendering context while retaining the Rust controller identity. Runtime failure
/// or panic consumes the host and deactivates callbacks before returning, so the native handles
/// are no longer borrowed and the token becomes stale.
///
/// # Safety
///
/// The host/token pair must be live and used on its owning UI thread.
#[no_mangle]
pub unsafe extern "C" fn explorerkit_desktop_private_detach(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
) -> ExplorerKitDesktopPrivateStatus {
    let status = with_host(host, token, ExplorerKitDesktopPrivateHost::detach);
    if status == ExplorerKitDesktopPrivateStatus::RuntimeError {
        let teardown_status = finish_host(host, token, false);
        if teardown_status != ExplorerKitDesktopPrivateStatus::Ok {
            return teardown_status;
        }
    }
    status
}

/// Parse and dispatch one opaque Rust-owned controller command or response envelope.
///
/// # Safety
///
/// The host/token pair must be live on its owning UI thread. A non-null `command` must point to
/// exactly `command_length` readable bytes for the duration of the call. The slice must be
/// non-empty, no larger than `EXPLORERKIT_DESKTOP_PRIVATE_MAX_COMMAND_BYTES`, and contain a valid
/// UTF-8 ControllerCommand v1 JSON envelope; it need not be NUL-terminated. Native adapters must
/// not parse or validate command names or command-specific payload fields.
#[no_mangle]
pub unsafe extern "C" fn explorerkit_desktop_private_dispatch_controller_command(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
    command: *const u8,
    command_length: usize,
) -> ExplorerKitDesktopPrivateStatus {
    with_host(host, token, |host| {
        let command_json = unsafe { required_command_json(command, command_length)? };
        inject_test_panic!(Command);
        host.controller.dispatch_controller_command(command_json)
    })
}

/// Translate AppKit/Win32 UI input into ExplorerKit's existing `HostInputEvent` vocabulary.
///
/// # Safety
///
/// The host/token pair must be live on its owning UI thread. Any string pointer selected by
/// `input.kind` must point to a valid NUL-terminated string for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn explorerkit_desktop_private_dispatch_input(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
    input: ExplorerKitDesktopPrivateInput,
) -> ExplorerKitDesktopPrivateStatus {
    with_host(host, token, |host| {
        let input = input_event(input)?;
        inject_test_panic!(Input);
        host.controller
            .runtime
            .dispatch_input_event(host.controller.webview, input)
            .map_err(Into::into)
    })
}

/// Pump Servo through its `Servo::spin_event_loop`/paint path on the owning UI thread.
///
/// # Safety
///
/// The host/token pair must be live and used on its owning UI thread.
#[no_mangle]
pub unsafe extern "C" fn explorerkit_desktop_private_pump(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
) -> ExplorerKitDesktopPrivateStatus {
    with_host(host, token, |host| {
        inject_test_panic!(Pump);
        host.controller
            .runtime
            .perform_updates(host.controller.webview)
            .map_err(Into::into)
    })
}

/// Drain translated Servo `WebViewDelegate` events synchronously with the controller token.
///
/// # Safety
///
/// The host/token pair must be live on its owning UI thread. The callback and its context must
/// remain valid and must not re-enter this host for the duration of the call. The callback must
/// contain every foreign exception, unwind, or non-local jump. `event_json` is borrowed UTF-8 and
/// is valid only synchronously during each callback invocation.
#[no_mangle]
pub unsafe extern "C" fn explorerkit_desktop_private_drain_events(
    host: *mut ExplorerKitDesktopPrivateHost,
    token: u64,
    callback: Option<EventCallback>,
    context: *mut c_void,
) -> ExplorerKitDesktopPrivateStatus {
    let prepared = accepted_host_call(host, token, |host| {
        let callback = callback.ok_or(ExplorerKitDesktopPrivateStatus::InvalidArgument)?;
        let host = unsafe { &mut *host };
        host.capture_events();
        let events = host.events.drain()?;
        Ok((callback, events))
    });
    let (_guard, (callback, events)) = match prepared {
        Ok(prepared) => prepared,
        Err(status) => return status,
    };

    // The callback runs outside Rust's unwind catcher. Foreign code must contain every language
    // exception/unwind before returning through this C ABI.
    for event in events {
        unsafe {
            callback(
                context,
                token,
                event.attachment_generation,
                event.json.as_ptr(),
            )
        };
    }
    ExplorerKitDesktopPrivateStatus::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorerkit::events::{HostEvent, LoadStatusKind};
    use explorerkit::host::HostError;
    use explorerkit_embedder::ContextMenuAction;
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    #[derive(Clone, Default)]
    struct TestHost {
        input: Rc<Cell<bool>>,
        updates: Rc<RefCell<Vec<HostEvent>>>,
        controller_responses: Rc<RefCell<Vec<String>>>,
    }

    impl Host for TestHost {
        fn create_webview(
            &mut self,
            _session: explorerkit::SessionHandle,
            _webview: WebViewHandle,
        ) -> Result<(), HostError> {
            Ok(())
        }

        fn evaluate_javascript(
            &mut self,
            _webview: WebViewHandle,
            evaluation_id: &str,
            _script: &str,
        ) -> Result<Vec<HostEvent>, HostError> {
            Ok(vec![HostEvent::JavaScriptEvaluationResult {
                evaluation_id: evaluation_id.to_owned(),
                ok: true,
                value_json: Some("true".into()),
                error_type: None,
            }])
        }

        fn resolve_navigation_request(
            &mut self,
            webview: WebViewHandle,
            navigation_id: &str,
            allow: bool,
        ) -> Result<Vec<HostEvent>, HostError> {
            self.controller_responses.borrow_mut().push(format!(
                "navigation:{}:{navigation_id}:{allow}",
                webview.raw()
            ));
            Ok(Vec::new())
        }

        fn resolve_simple_dialog(
            &mut self,
            webview: WebViewHandle,
            dialog_id: &str,
            confirmed: bool,
            prompt_value: Option<&str>,
        ) -> Result<Vec<HostEvent>, HostError> {
            self.controller_responses.borrow_mut().push(format!(
                "simple-dialog:{}:{dialog_id}:{confirmed}:{prompt_value:?}",
                webview.raw()
            ));
            Ok(Vec::new())
        }

        fn resolve_context_menu(
            &mut self,
            webview: WebViewHandle,
            context_menu_id: &str,
            action: ContextMenuAction,
        ) -> Result<Vec<HostEvent>, HostError> {
            self.controller_responses.borrow_mut().push(format!(
                "context-menu:{}:{context_menu_id}:{}",
                webview.raw(),
                action.as_str()
            ));
            Ok(Vec::new())
        }

        fn dismiss_context_menu(
            &mut self,
            webview: WebViewHandle,
            context_menu_id: &str,
        ) -> Result<Vec<HostEvent>, HostError> {
            self.controller_responses.borrow_mut().push(format!(
                "dismiss-context-menu:{}:{context_menu_id}",
                webview.raw()
            ));
            Ok(Vec::new())
        }

        fn dispatch_input_event(
            &mut self,
            _webview: WebViewHandle,
            _event: &HostInputEvent,
        ) -> Result<Vec<HostEvent>, HostError> {
            self.input.set(true);
            Ok(Vec::new())
        }

        fn perform_updates(
            &mut self,
            _webview: WebViewHandle,
        ) -> Result<Vec<HostEvent>, HostError> {
            Ok(self.updates.borrow_mut().drain(..).collect())
        }

        fn observe_webview_events(
            &mut self,
            _webview: WebViewHandle,
            _events: &[HostEvent],
        ) -> Result<(), HostError> {
            Ok(())
        }
    }

    fn has_event(events: &[QueuedCallbackEvent], generation: u64, text: &str) -> bool {
        events.iter().any(|event| {
            event.attachment_generation == generation && event.json.to_str().unwrap().contains(text)
        })
    }

    #[test]
    fn controller_stop_reports_existing_runtime_error_when_host_is_unsupported() {
        let mut controller = Controller::new(TestHost::default()).unwrap();
        assert_eq!(
            controller.dispatch_controller_command(r#"{"version":1,"command":"stopLoading"}"#),
            Err(ExplorerKitDesktopPrivateStatus::RuntimeError)
        );
        assert!(controller.runtime.drain_events().is_empty());
    }

    #[test]
    fn controller_dispatches_the_shared_command_and_response_envelope() {
        let controller_responses = Rc::new(RefCell::new(Vec::new()));
        let mut controller = Controller::new(TestHost {
            controller_responses: controller_responses.clone(),
            ..TestHost::default()
        })
        .unwrap();
        let webview = controller.webview;

        assert_eq!(
            controller.dispatch_controller_command(
                r#"{"version":1,"command":"evaluateJavaScript","evaluationId":"evaluation-json","script":"1 + 1"}"#,
            ),
            Ok(())
        );
        assert!(controller
            .runtime
            .drain_events()
            .iter()
            .any(|event| matches!(
                &event.event,
                HostEvent::JavaScriptEvaluationResult { evaluation_id, .. }
                    if evaluation_id == "evaluation-json"
            )));
        assert_eq!(
            controller.dispatch_controller_command(r#"{"version":2,"command":"reload"}"#),
            Err(ExplorerKitDesktopPrivateStatus::InvalidArgument)
        );
        for response in [
            r#"{"version":1,"command":"resolveNavigationRequest","navigationId":"navigation-1","allow":true}"#,
            r#"{"version":1,"command":"resolveSimpleDialog","dialogId":"dialog-1","confirmed":true,"promptValue":"Servo"}"#,
            r#"{"version":1,"command":"resolveContextMenu","contextMenuId":"context-menu-1","action":"copy-link"}"#,
            r#"{"version":1,"command":"dismissContextMenu","contextMenuId":"context-menu-2"}"#,
        ] {
            assert_eq!(controller.dispatch_controller_command(response), Ok(()));
        }
        assert_eq!(
            *controller_responses.borrow(),
            vec![
                format!("navigation:{}:navigation-1:true", webview.raw()),
                format!(
                    "simple-dialog:{}:dialog-1:true:Some(\"Servo\")",
                    webview.raw()
                ),
                format!("context-menu:{}:context-menu-1:copy-link", webview.raw()),
                format!("dismiss-context-menu:{}:context-menu-2", webview.raw()),
            ]
        );
    }

    #[test]
    fn lifecycle_routes_facade_input_and_translated_events() {
        let input = Rc::new(Cell::new(false));
        let updates = Rc::new(RefCell::new(vec![
            HostEvent::FaviconChanged {
                favicon: Some(explorerkit::FaviconImage {
                    width: 1,
                    height: 1,
                    format: explorerkit::FaviconPixelFormat::RGBA8,
                    bytes: vec![1, 2, 3, 4],
                }),
            },
            HostEvent::Error {
                url: Some("https://error.test/".into()),
                code: -1,
                message: "load failed".into(),
            },
            HostEvent::Crashed {
                url: Some("https://crash.test/".into()),
                reason: "renderer exited".into(),
                backtrace: None,
            },
        ]));
        let mut controller = Controller::new(TestHost {
            input: input.clone(),
            updates,
            ..TestHost::default()
        })
        .unwrap();
        let initial = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let resized = SurfaceViewport::new(SurfaceSize::new(640, 480), 1.5);
        let webview = controller.webview;
        let mut bridge_events = BridgeEvents {
            latest_attachment_generation: Some(41),
            ..BridgeEvents::default()
        };

        controller
            .runtime
            .attach_surface_with_viewport(webview, HostSurface::new(SURFACE_ID), initial)
            .unwrap();
        controller
            .runtime
            .update_surface_viewport(webview, resized)
            .unwrap();
        controller
            .runtime
            .dispatch_input_event(webview, HostInputEvent::Focus { is_focused: true })
            .unwrap();
        controller.runtime.load_url(webview, "example.com").unwrap();
        controller.runtime.focus(webview).unwrap();
        controller
            .runtime
            .evaluate_javascript(webview, "evaluation-1", "1 + 1 === 2")
            .unwrap();
        controller.runtime.perform_updates(webview).unwrap();
        controller.runtime.detach_surface(webview).unwrap();

        bridge_events.capture(&mut controller);
        let events = bridge_events.drain().unwrap();
        let events: Vec<_> = events
            .iter()
            .map(|event| (event.attachment_generation, event.json.to_str().unwrap()))
            .collect();
        assert!(input.get());
        assert!(events.iter().any(|(generation, event)| *generation == 41
            && event.contains("\"name\":\"surfaceResized\"")
            && event.contains("\"width\":640")));
        assert!(events.iter().any(|(generation, event)| *generation == 0
            && event.contains("\"name\":\"loadStatusChanged\"")
            && event.contains(LoadStatusKind::Complete.as_str())));
        for name in [
            "focusChanged",
            "error",
            "crashed",
            "javascriptEvaluationResult",
            "surfaceDetached",
        ] {
            let event_name = format!("\"name\":\"{name}\"");
            assert!(events.iter().any(|(_, event)| event.contains(&event_name)));
        }
        assert!(events
            .iter()
            .all(|(_, event)| !event.contains("\"name\":\"faviconChanged\"")));
    }

    #[test]
    fn detach_event_is_delivered_when_drained_before_reattach() {
        let mut controller = Controller::new(TestHost {
            input: Rc::new(Cell::new(false)),
            updates: Rc::new(RefCell::new(Vec::new())),
            ..TestHost::default()
        })
        .unwrap();
        let webview = controller.webview;
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let mut bridge_events = BridgeEvents {
            latest_attachment_generation: Some(51),
            ..BridgeEvents::default()
        };

        controller
            .runtime
            .attach_surface_with_viewport(webview, HostSurface::new(SURFACE_ID), viewport)
            .unwrap();
        bridge_events.capture(&mut controller);
        bridge_events.drain().unwrap();
        controller.runtime.detach_surface(webview).unwrap();
        bridge_events.capture(&mut controller);
        let drained = bridge_events.drain().unwrap();
        assert!(has_event(&drained, 51, "\"name\":\"surfaceDetached\""));

        controller
            .runtime
            .attach_surface_with_viewport(webview, HostSurface::new(SURFACE_ID), viewport)
            .unwrap();
        bridge_events.latest_attachment_generation = Some(52);
        bridge_events.capture(&mut controller);
        let reattached = bridge_events.drain().unwrap();
        assert!(has_event(&reattached, 52, "\"name\":\"surfaceAttached\""));
    }

    #[test]
    fn stale_attachment_events_do_not_cross_reattach_but_controller_events_do() {
        let updates = Rc::new(RefCell::new(Vec::new()));
        let mut controller = Controller::new(TestHost {
            input: Rc::new(Cell::new(false)),
            updates: updates.clone(),
            ..TestHost::default()
        })
        .unwrap();
        let webview = controller.webview;
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let mut bridge_events = BridgeEvents {
            latest_attachment_generation: Some(61),
            ..BridgeEvents::default()
        };

        controller
            .runtime
            .attach_surface_with_viewport(webview, HostSurface::new(SURFACE_ID), viewport)
            .unwrap();
        bridge_events.capture(&mut controller);
        bridge_events.drain().unwrap();
        controller.runtime.detach_surface(webview).unwrap();
        updates.borrow_mut().extend([
            HostEvent::SurfaceResized {
                size: SurfaceSize::new(999, 777),
            },
            HostEvent::Error {
                url: Some("https://controller.test/".into()),
                code: -2,
                message: "retained controller event".into(),
            },
        ]);
        controller.runtime.perform_updates(webview).unwrap();
        bridge_events.capture(&mut controller);

        controller
            .runtime
            .attach_surface_with_viewport(webview, HostSurface::new(SURFACE_ID), viewport)
            .unwrap();
        bridge_events.latest_attachment_generation = Some(62);
        bridge_events.capture(&mut controller);

        let events = bridge_events.drain().unwrap();
        assert!(has_event(&events, 62, "\"name\":\"surfaceAttached\""));
        assert!(!events.iter().any(|event| event.attachment_generation == 61));
        assert!(has_event(&events, 0, "retained controller event"));
    }

    #[test]
    fn failed_attach_preserves_queued_events_and_current_generation() {
        let updates = Rc::new(RefCell::new(vec![
            HostEvent::SurfaceResized {
                size: SurfaceSize::new(400, 300),
            },
            HostEvent::Error {
                url: Some("https://failed-attach.test/".into()),
                code: -3,
                message: "before failed attach".into(),
            },
        ]));
        let mut controller = Controller::new(TestHost {
            input: Rc::new(Cell::new(false)),
            updates,
            ..TestHost::default()
        })
        .unwrap();
        let webview = controller.webview;
        let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let mut bridge_events = BridgeEvents {
            latest_attachment_generation: Some(71),
            ..BridgeEvents::default()
        };

        controller
            .runtime
            .attach_surface_with_viewport(webview, HostSurface::new(SURFACE_ID), viewport)
            .unwrap();
        bridge_events.capture(&mut controller);
        bridge_events.drain().unwrap();
        controller.runtime.perform_updates(webview).unwrap();
        bridge_events.capture(&mut controller);
        assert!(controller
            .runtime
            .attach_surface_with_viewport(webview, HostSurface::new(SURFACE_ID), viewport)
            .is_err());
        bridge_events.capture(&mut controller);

        let events = bridge_events.drain().unwrap();
        assert!(has_event(&events, 71, "\"name\":\"surfaceResized\""));
        assert!(has_event(&events, 0, "before failed attach"));
    }

    fn input(kind: u32) -> ExplorerKitDesktopPrivateInput {
        ExplorerKitDesktopPrivateInput {
            kind,
            x: 0.0,
            y: 0.0,
            delta_x: 0.0,
            delta_y: 0.0,
            button_kind: 0,
            button_code: 0,
            action: 0,
            scroll_mode: 0,
            key_kind: 0,
            key_code: 0,
            text: ptr::null(),
            repeat: 0,
            is_composing: 0,
            is_focused: 0,
        }
    }

    #[test]
    fn input_abi_preserves_character_named_and_other_button_values() {
        let enter_text = CString::new("Enter").unwrap();
        let mut character = input(INPUT_KEYBOARD);
        character.action = 1;
        character.text = enter_text.as_ptr();
        assert_eq!(
            input_event(character).unwrap(),
            HostInputEvent::Keyboard(KeyboardInputEvent {
                key: KeyboardInputKey::Character("Enter".into()),
                state: KeyboardInputState::Pressed,
                repeat: false,
                is_composing: false,
            })
        );

        let mut named = input(INPUT_KEYBOARD);
        named.key_kind = KEY_KIND_NAMED;
        named.key_code = 2;
        assert_eq!(
            input_event(named).unwrap(),
            HostInputEvent::Keyboard(KeyboardInputEvent::new(
                KeyboardInputKey::Named(KeyboardNamedKey::Enter),
                KeyboardInputState::Released,
            ))
        );
        named.text = enter_text.as_ptr();
        assert_eq!(
            input_event(named).unwrap_err(),
            ExplorerKitDesktopPrivateStatus::InvalidArgument
        );

        character.key_code = 1;
        assert_eq!(
            input_event(character).unwrap_err(),
            ExplorerKitDesktopPrivateStatus::InvalidArgument
        );

        let mut other = input(INPUT_POINTER_BUTTON);
        other.button_kind = POINTER_BUTTON_OTHER;
        other.button_code = u32::from(u16::MAX);
        assert!(matches!(
            input_event(other).unwrap(),
            HostInputEvent::Pointer(PointerInputEvent::Button {
                button: PointerButton::Other(u16::MAX),
                ..
            })
        ));

        other.button_code = u32::from(u16::MAX) + 1;
        assert_eq!(
            input_event(other).unwrap_err(),
            ExplorerKitDesktopPrivateStatus::InvalidArgument
        );
        let mut noncanonical = input(INPUT_POINTER_BUTTON);
        noncanonical.button_code = 1;
        assert_eq!(
            input_event(noncanonical).unwrap_err(),
            ExplorerKitDesktopPrivateStatus::InvalidArgument
        );
    }

    #[test]
    fn coordinate_abi_preserves_child_local_physical_pixels() {
        let converted = viewport(ExplorerKitDesktopPrivateViewport {
            x: 24,
            y: 36,
            width: 640,
            height: 480,
            scale_factor: 2.0,
        })
        .unwrap();
        assert_eq!(converted.origin, SurfacePoint::new(24, 36));
        assert_eq!(converted.size, SurfaceSize::new(640, 480));
        assert_eq!(converted.scale_factor, 2.0);

        let mut pointer = input(INPUT_POINTER_MOVE);
        pointer.x = 18.0;
        pointer.y = 26.0;
        assert!(matches!(
            input_event(pointer).unwrap(),
            HostInputEvent::Pointer(PointerInputEvent::Moved { x: 18.0, y: 26.0 })
        ));

        let mut wheel = input(INPUT_POINTER_WHEEL);
        wheel.x = 18.0;
        wheel.y = 26.0;
        wheel.delta_x = 3.5;
        wheel.delta_y = -12.25;
        wheel.scroll_mode = 1;
        assert!(matches!(
            input_event(wheel).unwrap(),
            HostInputEvent::Pointer(PointerInputEvent::Wheel {
                delta_x: 3.5,
                delta_y: -12.25,
                mode: PointerScrollMode::Pixels,
                x: 18.0,
                y: 26.0,
            })
        ));
    }

    /// # Safety
    ///
    /// This callback does not dereference any boundary arguments.
    unsafe extern "C" fn ignore_event(
        _context: *mut c_void,
        _token: u64,
        _attachment_generation: u64,
        _event_json: *const c_char,
    ) {
    }

    /// # Safety
    ///
    /// `context` must point to a live `AtomicUsize`.
    unsafe extern "C" fn count_wake(context: *mut c_void, _token: u64) {
        unsafe { &*(context as *const AtomicUsize) }.fetch_add(1, Ordering::Relaxed);
    }

    fn assert_all_later_exports_are_stale(host: *mut ExplorerKitDesktopPrivateHost, token: u64) {
        let mut generation = u64::MAX;
        let viewport = ExplorerKitDesktopPrivateViewport {
            x: 0,
            y: 0,
            width: 320,
            height: 240,
            scale_factor: 2.0,
        };
        let reload = br#"{"version":1,"command":"reload"}"#;
        let mut focus = input(INPUT_FOCUS);
        focus.is_focused = 1;

        assert_eq!(
            unsafe {
                explorerkit_desktop_private_attach(
                    host,
                    token,
                    ExplorerKitDesktopPrivateNativeSurface {
                        window: ptr::null_mut(),
                        display: ptr::null_mut(),
                    },
                    viewport,
                    &mut generation,
                )
            },
            ExplorerKitDesktopPrivateStatus::StaleToken
        );
        assert_eq!(generation, 0);
        assert_eq!(
            unsafe { explorerkit_desktop_private_update_viewport(host, token, viewport) },
            ExplorerKitDesktopPrivateStatus::StaleToken
        );
        assert_eq!(
            unsafe { explorerkit_desktop_private_detach(host, token) },
            ExplorerKitDesktopPrivateStatus::StaleToken
        );
        assert_eq!(
            unsafe {
                explorerkit_desktop_private_dispatch_controller_command(
                    host,
                    token,
                    reload.as_ptr(),
                    reload.len(),
                )
            },
            ExplorerKitDesktopPrivateStatus::StaleToken
        );
        assert_eq!(
            unsafe { explorerkit_desktop_private_dispatch_input(host, token, focus) },
            ExplorerKitDesktopPrivateStatus::StaleToken
        );
        assert_eq!(
            unsafe { explorerkit_desktop_private_pump(host, token) },
            ExplorerKitDesktopPrivateStatus::StaleToken
        );
        assert_eq!(
            unsafe {
                explorerkit_desktop_private_drain_events(
                    host,
                    token,
                    Some(ignore_event),
                    ptr::null_mut(),
                )
            },
            ExplorerKitDesktopPrivateStatus::StaleToken
        );
        assert_eq!(
            unsafe { explorerkit_desktop_private_destroy(host, token) },
            ExplorerKitDesktopPrivateStatus::StaleToken
        );
    }

    #[test]
    fn accepted_panics_retire_every_operation_path() {
        fn run(
            point: TestPanic,
            operation: fn(
                *mut ExplorerKitDesktopPrivateHost,
                u64,
            ) -> ExplorerKitDesktopPrivateStatus,
        ) {
            let token = allocate_id().unwrap();
            let wake_count = AtomicUsize::new(0);
            let host = build_host(
                token,
                ExplorerKitDesktopPrivateCallbacks {
                    context: (&wake_count as *const AtomicUsize).cast_mut().cast(),
                    wake: Some(count_wake),
                },
            )
            .unwrap();
            let callbacks = host.callbacks.clone();
            let host = Box::into_raw(host);
            assert!(register_host(host as usize, token));
            TEST_PANIC.with(|fault| fault.set(Some(point)));

            assert_eq!(
                operation(host, token),
                ExplorerKitDesktopPrivateStatus::Panic
            );
            assert!(lock_unpoisoned(&callbacks.callback).is_none());
            callbacks.wake();
            assert_eq!(wake_count.load(Ordering::Relaxed), 0);
            assert_all_later_exports_are_stale(host, token);
        }

        fn viewport(
            host: *mut ExplorerKitDesktopPrivateHost,
            token: u64,
        ) -> ExplorerKitDesktopPrivateStatus {
            unsafe {
                explorerkit_desktop_private_update_viewport(
                    host,
                    token,
                    ExplorerKitDesktopPrivateViewport {
                        x: 0,
                        y: 0,
                        width: 320,
                        height: 240,
                        scale_factor: 2.0,
                    },
                )
            }
        }

        fn command(
            host: *mut ExplorerKitDesktopPrivateHost,
            token: u64,
        ) -> ExplorerKitDesktopPrivateStatus {
            let reload = br#"{"version":1,"command":"reload"}"#;
            unsafe {
                explorerkit_desktop_private_dispatch_controller_command(
                    host,
                    token,
                    reload.as_ptr(),
                    reload.len(),
                )
            }
        }

        fn input_operation(
            host: *mut ExplorerKitDesktopPrivateHost,
            token: u64,
        ) -> ExplorerKitDesktopPrivateStatus {
            let mut focus = input(INPUT_FOCUS);
            focus.is_focused = 1;
            unsafe { explorerkit_desktop_private_dispatch_input(host, token, focus) }
        }

        fn pump(
            host: *mut ExplorerKitDesktopPrivateHost,
            token: u64,
        ) -> ExplorerKitDesktopPrivateStatus {
            unsafe { explorerkit_desktop_private_pump(host, token) }
        }

        fn drain(
            host: *mut ExplorerKitDesktopPrivateHost,
            token: u64,
        ) -> ExplorerKitDesktopPrivateStatus {
            unsafe {
                explorerkit_desktop_private_drain_events(
                    host,
                    token,
                    Some(ignore_event),
                    ptr::null_mut(),
                )
            }
        }

        run(TestPanic::Viewport, viewport);
        run(TestPanic::Command, command);
        run(TestPanic::Input, input_operation);
        run(TestPanic::Pump, pump);
        run(TestPanic::EventCapture, drain);
        run(TestPanic::EventDrain, drain);
    }

    #[test]
    fn accepted_viewport_host_error_consumes_the_private_host() {
        let token = allocate_id().unwrap();
        let wake_count = AtomicUsize::new(0);
        let host = build_host(
            token,
            ExplorerKitDesktopPrivateCallbacks {
                context: (&wake_count as *const AtomicUsize).cast_mut().cast(),
                wake: Some(count_wake),
            },
        )
        .unwrap();
        let callbacks = host.callbacks.clone();
        let host = Box::into_raw(host);
        assert!(register_host(host as usize, token));
        TEST_VIEWPORT_HOST_ERROR.with(|fault| fault.set(true));

        assert_eq!(
            unsafe {
                explorerkit_desktop_private_update_viewport(
                    host,
                    token,
                    ExplorerKitDesktopPrivateViewport {
                        x: 0,
                        y: 0,
                        width: 320,
                        height: 240,
                        scale_factor: 2.0,
                    },
                )
            },
            ExplorerKitDesktopPrivateStatus::RuntimeError
        );
        assert!(lock_unpoisoned(&callbacks.callback).is_none());
        callbacks.wake();
        assert_eq!(wake_count.load(Ordering::Relaxed), 0);
        assert_all_later_exports_are_stale(host, token);
    }

    #[test]
    fn rejected_preflight_calls_leave_the_host_usable() {
        let token = allocate_id().unwrap();
        let host = build_host(
            token,
            ExplorerKitDesktopPrivateCallbacks {
                context: ptr::null_mut(),
                wake: None,
            },
        )
        .unwrap();
        let host = Box::into_raw(host);
        assert!(register_host(host as usize, token));

        assert_eq!(
            unsafe {
                explorerkit_desktop_private_update_viewport(
                    host,
                    token,
                    ExplorerKitDesktopPrivateViewport {
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 240,
                        scale_factor: 2.0,
                    },
                )
            },
            ExplorerKitDesktopPrivateStatus::InvalidArgument
        );
        assert_eq!(
            unsafe {
                explorerkit_desktop_private_dispatch_controller_command(host, token, ptr::null(), 1)
            },
            ExplorerKitDesktopPrivateStatus::InvalidArgument
        );
        assert_eq!(
            unsafe { explorerkit_desktop_private_drain_events(host, token, None, ptr::null_mut()) },
            ExplorerKitDesktopPrivateStatus::InvalidArgument
        );
        assert_eq!(
            unsafe {
                explorerkit_desktop_private_update_viewport(
                    host,
                    token,
                    ExplorerKitDesktopPrivateViewport {
                        x: 0,
                        y: 0,
                        width: 320,
                        height: 240,
                        scale_factor: 2.0,
                    },
                )
            },
            ExplorerKitDesktopPrivateStatus::RuntimeError
        );
        assert_eq!(
            unsafe { explorerkit_desktop_private_pump(host, token) },
            ExplorerKitDesktopPrivateStatus::Ok
        );
        assert_eq!(
            unsafe { explorerkit_desktop_private_destroy(host, token) },
            ExplorerKitDesktopPrivateStatus::Ok
        );
    }

    #[test]
    fn stale_and_destroyed_tokens_are_rejected_before_pointer_access() {
        let token = allocate_id().unwrap();
        let allocation = Box::into_raw(Box::new(0_u8));
        let host = allocation.cast::<ExplorerKitDesktopPrivateHost>();
        assert!(register_host(host as usize, token));

        assert_eq!(
            begin_call(host, token.wrapping_add(1)).err(),
            Some(ExplorerKitDesktopPrivateStatus::StaleToken)
        );
        let guard = begin_call(host, token).unwrap();
        assert_eq!(
            begin_call(host, token).err(),
            Some(ExplorerKitDesktopPrivateStatus::Busy)
        );
        let address = host as usize;
        assert_eq!(
            thread::spawn(move || begin_call(address as *mut _, token).err())
                .join()
                .unwrap(),
            Some(ExplorerKitDesktopPrivateStatus::WrongThread)
        );
        drop(guard);
        lock_unpoisoned(&HOST_REGISTRY)
            .live
            .remove(&(host as usize));
        assert_eq!(
            begin_call(host, token).err(),
            Some(ExplorerKitDesktopPrivateStatus::StaleToken)
        );

        unsafe { drop(Box::from_raw(allocation)) };
    }

    #[test]
    fn ids_exhaust_permanently_and_address_reuse_gets_a_fresh_token() {
        let mut ids = IdAllocator::starting_at(u64::MAX - 1);
        assert_eq!(ids.allocate(), Some(u64::MAX - 1));
        assert_eq!(ids.allocate(), Some(u64::MAX));
        assert_eq!(ids.allocate(), None);
        assert_eq!(ids.allocate(), None);

        let address = 0x1234;
        let owner = thread::current().id();
        let mut registry = HostRegistry::default();
        let first = registry.allocate_id().unwrap();
        assert!(registry.register(address, first, owner));
        registry.live.remove(&address);
        let second = registry.allocate_id().unwrap();
        assert!(registry.register(address, second, owner));

        assert_ne!(first, second);
        assert_eq!(registry.live[&address].token, second);
    }

    #[test]
    fn exported_destroy_waits_for_an_in_flight_wake_callback() {
        struct BlockingWake {
            calls: AtomicUsize,
            entered: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }

        /// # Safety
        ///
        /// The test keeps `context` alive until the callback thread exits.
        unsafe extern "C" fn wake(context: *mut c_void, _token: u64) {
            let context = unsafe { &*(context as *const BlockingWake) };
            context.calls.fetch_add(1, Ordering::Relaxed);
            context.entered.send(()).unwrap();
            lock_unpoisoned(&context.release).recv().unwrap();
        }

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let context = Box::new(BlockingWake {
            calls: AtomicUsize::new(0),
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        let token = allocate_id().unwrap();
        let host = build_host(
            token,
            ExplorerKitDesktopPrivateCallbacks {
                context: (&*context as *const BlockingWake).cast_mut().cast(),
                wake: Some(wake),
            },
        )
        .unwrap();
        let gate = host.callbacks.clone();
        let host = Box::into_raw(host);
        assert!(register_host(host as usize, token));
        let waking = {
            let gate = gate.clone();
            thread::spawn(move || gate.wake())
        };
        entered_rx.recv().unwrap();

        let destroy_returned = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = mpsc::channel();
        let releaser = {
            let destroy_returned = destroy_returned.clone();
            thread::spawn(move || {
                started_rx.recv().unwrap();
                thread::sleep(Duration::from_millis(50));
                assert!(!destroy_returned.load(Ordering::SeqCst));
                release_tx.send(()).unwrap();
            })
        };
        started_tx.send(()).unwrap();
        let status = unsafe { explorerkit_desktop_private_destroy(host, token) };
        destroy_returned.store(true, Ordering::SeqCst);

        assert_eq!(status, ExplorerKitDesktopPrivateStatus::Ok);
        waking.join().unwrap();
        releaser.join().unwrap();
        gate.wake();
        assert_eq!(context.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            begin_call(host, token).err(),
            Some(ExplorerKitDesktopPrivateStatus::StaleToken)
        );
    }
}
