use crate::commands::{
    attach_surface_with_native_window, load_navigation_request_from_str, perform_android_updates,
    push_runtime_error, queue_backend_result, queue_control_command,
};
use crate::registry::{free_live_host, register_live_host, with_live_host, with_live_host_mut};
use crate::state::{ControlCommand, HostHandle};
use crate::{NativeWindowHandle, ServoStatus, SurfaceSize};
use explorerkit_embedder::{
    encode_host_event_bridge, ContextMenuAction, HostEvent, HostSurface, ImeCompositionState,
    KeyboardKey, TouchEventKind, WebViewCommand,
};
use std::ffi::{c_char, CStr};

fn copy_string(value: Option<&str>, output: *mut c_char, capacity: usize) -> usize {
    if output.is_null() || capacity == 0 {
        return 0;
    }

    let Some(value) = value else {
        unsafe {
            *output = 0;
        }
        return 0;
    };

    let bytes = value.as_bytes();
    let length = bytes.len().min(capacity.saturating_sub(1));

    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), output.cast::<u8>(), length);
        *output.add(length) = 0;
    }

    length
}

fn touch_event_kind_from_i32(action: i32) -> Option<TouchEventKind> {
    match action {
        0 => Some(TouchEventKind::Down),
        1 => Some(TouchEventKind::Move),
        2 => Some(TouchEventKind::Up),
        3 => Some(TouchEventKind::Cancel),
        _ => None,
    }
}

fn simple_dialog_action_from_i32(action: i32) -> Option<bool> {
    match action {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

fn ime_composition_state_from_i32(state: i32) -> Option<ImeCompositionState> {
    match state {
        0 => Some(ImeCompositionState::Update),
        1 => Some(ImeCompositionState::End),
        _ => None,
    }
}

fn keyboard_key_from_i32(key: i32) -> Option<KeyboardKey> {
    match key {
        0 => Some(KeyboardKey::Backspace),
        1 => Some(KeyboardKey::Delete),
        2 => Some(KeyboardKey::Enter),
        _ => None,
    }
}

#[no_mangle]
pub extern "C" fn servo_host_new(_target: u8) -> *mut HostHandle {
    let handle = Box::into_raw(Box::new(HostHandle::new()));
    register_live_host(handle);
    handle
}

#[no_mangle]
/// Loads a URL into the host.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`, and `url` must point to a
/// NUL-terminated UTF-8 string.
pub unsafe extern "C" fn servo_host_load_url(
    host: *mut HostHandle,
    url: *const c_char,
) -> ServoStatus {
    if host.is_null() || url.is_null() {
        return ServoStatus::NullPointer;
    }

    let raw_url = unsafe { CStr::from_ptr(url) };
    let url = match raw_url.to_str() {
        Ok(value) => value,
        Err(_) => {
            return with_live_host_mut(host, |handle| {
                handle.events.push_back(HostEvent::Error {
                    url: None,
                    code: ServoStatus::InvalidUrl as i32,
                    message: "url is not valid utf-8".to_owned(),
                });
                ServoStatus::InvalidUrl
            })
            .unwrap_or(ServoStatus::NullPointer);
        }
    };

    with_live_host_mut(host, |handle| {
        match load_navigation_request_from_str(handle, url) {
            Ok(()) => ServoStatus::Ok,
            Err(status) => status,
        }
    })
    .unwrap_or(ServoStatus::NullPointer)
}

#[no_mangle]
/// Queues a surface-attached event for the host.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
///
/// Calls made while a surface is already tracked are ignored.
pub unsafe extern "C" fn servo_host_attach_surface(host: *mut HostHandle, width: u32, height: u32) {
    let _ = with_live_host_mut(host, |handle| {
        if handle.surface.is_some() {
            return;
        }

        let size = SurfaceSize::new(width, height);
        let _ = handle.attach_surface_runtime(HostSurface::new("android-surface"), size);
    });
}

#[no_mangle]
/// Queues a surface-attached event for the host and stores an Android native window.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`, and `native_window` must be a
/// valid Android native window pointer for the duration expected by the caller.
///
/// Calls made while a native window is already tracked are ignored.
pub unsafe extern "C" fn servo_host_attach_surface_with_native_window(
    host: *mut HostHandle,
    native_window: NativeWindowHandle,
    width: u32,
    height: u32,
    density: f32,
) {
    if native_window.is_null() {
        return;
    }

    let _ = with_live_host_mut(host, |handle| {
        let size = SurfaceSize::new(width, height);
        attach_surface_with_native_window(handle, native_window, size, density);
    });
}

#[no_mangle]
/// Queues a surface-resized event for the host.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
///
/// Calls made before a surface is tracked are ignored.
pub unsafe extern "C" fn servo_host_resize_surface(
    host: *mut HostHandle,
    width: u32,
    height: u32,
    density: f32,
) {
    let _ = with_live_host_mut(host, |handle| {
        if handle.surface.is_none() {
            return;
        }
        let size = SurfaceSize::new(width, height);
        handle.pending_resize_density = Some(density);
        if let Err(error) = handle.resize_surface_runtime(size) {
            let current_url = handle.current_url.clone();
            push_runtime_error(handle, error, current_url.as_deref());
        }
    });
}

#[no_mangle]
/// Queues a surface-detached event for the host.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
///
/// Calls made before a surface is tracked are ignored.
pub unsafe extern "C" fn servo_host_detach_surface(host: *mut HostHandle) {
    let _ = with_live_host_mut(host, |handle| {
        if handle.surface.is_none() {
            return;
        }

        if let Err(error) = handle.detach_surface_runtime() {
            let current_url = handle.current_url.clone();
            push_runtime_error(handle, error, current_url.as_deref());
        }
    });
}

#[no_mangle]
/// Forwards a touch event into the Android-backed WebView.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_dispatch_touch_event(
    host: *mut HostHandle,
    action: i32,
    touch_id: i32,
    x: f32,
    y: f32,
) {
    let Some(event_kind) = touch_event_kind_from_i32(action) else {
        return;
    };

    let _ = with_live_host_mut(host, |handle| {
        let result = {
            let Some(backend) = handle.android_backend.as_mut() else {
                return;
            };
            backend.dispatch_touch_event(event_kind, touch_id, x, y)
        };
        let current_url = handle.current_url.clone();
        queue_backend_result(handle, result, current_url.as_deref());
    });
}

#[no_mangle]
/// Triggers Servo's context-menu flow at the given point.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_trigger_context_menu(host: *mut HostHandle, x: f32, y: f32) {
    let _ = with_live_host_mut(host, |handle| {
        let result = {
            let Some(backend) = handle.android_backend.as_mut() else {
                return;
            };
            backend.trigger_context_menu(x, y)
        };
        let current_url = handle.current_url.clone();
        queue_backend_result(handle, result, current_url.as_deref());
    });
}

#[no_mangle]
/// Queues an IME composition event for the Android-backed WebView.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`, and `text` must point to a
/// NUL-terminated UTF-8 string.
pub unsafe extern "C" fn servo_host_dispatch_ime_composition(
    host: *mut HostHandle,
    state: i32,
    text: *const c_char,
) -> ServoStatus {
    if host.is_null() || text.is_null() {
        return ServoStatus::NullPointer;
    }

    let Some(state) = ime_composition_state_from_i32(state) else {
        return ServoStatus::BackendError;
    };
    let text = match unsafe { CStr::from_ptr(text) }.to_str() {
        Ok(value) => value.to_owned(),
        Err(_) => return ServoStatus::BackendError,
    };

    with_live_host_mut(host, |handle| {
        queue_control_command(
            handle,
            ControlCommand::DispatchImeComposition { state, text },
        );
        ServoStatus::Ok
    })
    .unwrap_or(ServoStatus::NullPointer)
}

#[no_mangle]
/// Queues an input-method dismissal event for the Android-backed WebView.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_dismiss_input_method(host: *mut HostHandle) -> ServoStatus {
    with_live_host_mut(host, |handle| {
        queue_control_command(handle, ControlCommand::DismissInputMethod);
        ServoStatus::Ok
    })
    .unwrap_or(ServoStatus::NullPointer)
}

#[no_mangle]
/// Queues a keyboard key event for the Android-backed WebView.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_dispatch_keyboard_key(
    host: *mut HostHandle,
    key: i32,
) -> ServoStatus {
    let Some(key) = keyboard_key_from_i32(key) else {
        return ServoStatus::BackendError;
    };

    with_live_host_mut(host, |handle| {
        queue_control_command(handle, ControlCommand::DispatchKeyboardKey { key });
        ServoStatus::Ok
    })
    .unwrap_or(ServoStatus::NullPointer)
}

#[no_mangle]
/// Drains any backend-generated Android updates into the host event queue.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_perform_updates(host: *mut HostHandle) {
    let _ = with_live_host_mut(host, perform_android_updates);
}

#[no_mangle]
/// Drains the next pending event into the last-event snapshot, stores its bridge JSON,
/// and returns that JSON length in bytes.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_take_next_event_bridge_json_len(
    host: *mut HostHandle,
) -> usize {
    with_live_host_mut(host, |handle| {
        handle.last_event = loop {
            match handle.events.pop_front() {
                Some(HostEvent::FaviconChanged { .. }) => continue,
                event => break event,
            }
        };
        handle.last_event_bridge_json = handle.last_event.as_ref().map(encode_host_event_bridge);
        handle
            .last_event_bridge_json
            .as_ref()
            .map_or(0, String::len)
    })
    .unwrap_or(0)
}

#[no_mangle]
/// Copies the last drained bridge JSON into `output`.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`, and `output` must point to a writable buffer.
pub unsafe extern "C" fn servo_host_last_event_bridge_json_copy(
    host: *const HostHandle,
    output: *mut c_char,
    capacity: usize,
) -> usize {
    with_live_host(host, |handle| {
        copy_string(handle.last_event_bridge_json.as_deref(), output, capacity)
    })
    .unwrap_or(0)
}

#[no_mangle]
/// Returns whether the Android backend has a pending permission request.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_has_pending_permission_request(
    host: *const HostHandle,
) -> bool {
    with_live_host(host, |handle| {
        handle
            .android_backend
            .as_ref()
            .map(|backend| backend.has_pending_permission_request())
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

#[no_mangle]
/// Returns the pending permission kind length in bytes.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_pending_permission_request_permission_len(
    host: *const HostHandle,
) -> usize {
    with_live_host(host, |handle| {
        handle
            .android_backend
            .as_ref()
            .and_then(|backend| backend.pending_permission_request())
            .map_or(0, |(permission, _)| permission.len())
    })
    .unwrap_or(0)
}

#[no_mangle]
/// Copies the pending permission kind into `output`.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`, and `output` must point to a writable buffer.
pub unsafe extern "C" fn servo_host_pending_permission_request_permission_copy(
    host: *const HostHandle,
    output: *mut c_char,
    capacity: usize,
) -> usize {
    with_live_host(host, |handle| {
        let permission = handle
            .android_backend
            .as_ref()
            .and_then(|backend| backend.pending_permission_request())
            .map(|(permission, _)| permission);
        copy_string(permission.as_deref(), output, capacity)
    })
    .unwrap_or(0)
}

#[no_mangle]
/// Returns the pending permission origin length in bytes.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_pending_permission_request_origin_len(
    host: *const HostHandle,
) -> usize {
    with_live_host(host, |handle| {
        handle
            .android_backend
            .as_ref()
            .and_then(|backend| backend.pending_permission_request())
            .map_or(0, |(_, origin)| origin.len())
    })
    .unwrap_or(0)
}

#[no_mangle]
/// Copies the pending permission origin into `output`.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`, and `output` must point to a writable buffer.
pub unsafe extern "C" fn servo_host_pending_permission_request_origin_copy(
    host: *const HostHandle,
    output: *mut c_char,
    capacity: usize,
) -> usize {
    with_live_host(host, |handle| {
        let origin = handle
            .android_backend
            .as_ref()
            .and_then(|backend| backend.pending_permission_request())
            .map(|(_, origin)| origin);
        copy_string(origin.as_deref(), output, capacity)
    })
    .unwrap_or(0)
}

#[no_mangle]
/// Returns the most recent error code from the host.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_last_error_code(host: *const HostHandle) -> i32 {
    with_live_host(host, |handle| {
        handle
            .events
            .back()
            .and_then(HostEvent::error_code)
            .unwrap_or(0)
    })
    .unwrap_or(-1)
}

#[no_mangle]
/// Returns the current URL length in bytes.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_current_url_len(host: *const HostHandle) -> usize {
    with_live_host(host, |handle| {
        handle.current_url.as_ref().map_or(0, String::len)
    })
    .unwrap_or(0)
}

#[no_mangle]
/// Copies the current URL into `output` and returns the number of bytes written.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`, and `output` must point to a
/// writable buffer of at least `capacity` bytes.
pub unsafe extern "C" fn servo_host_current_url_copy(
    host: *const HostHandle,
    output: *mut c_char,
    capacity: usize,
) -> usize {
    with_live_host(host, |handle| {
        copy_string(handle.current_url.as_deref(), output, capacity)
    })
    .unwrap_or(0)
}

#[no_mangle]
/// Returns the last error message length in bytes.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_last_error_message_len(host: *const HostHandle) -> usize {
    with_live_host(host, |handle| {
        handle
            .events
            .back()
            .and_then(HostEvent::error_message)
            .map_or(0, str::len)
    })
    .unwrap_or(0)
}

#[no_mangle]
/// Copies the last error message into `output` and returns bytes written.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`, and `output` must point to a writable buffer.
pub unsafe extern "C" fn servo_host_last_error_message_copy(
    host: *const HostHandle,
    output: *mut c_char,
    capacity: usize,
) -> usize {
    with_live_host(host, |handle| {
        let message = handle.events.back().and_then(HostEvent::error_message);
        copy_string(message, output, capacity)
    })
    .unwrap_or(0)
}

#[no_mangle]
/// Reloads the current page.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_reload(host: *mut HostHandle) {
    let _ = with_live_host_mut(host, |handle| {
        crate::commands::dispatch_webview_command(handle, WebViewCommand::Reload);
    });
}

#[no_mangle]
/// Navigates back by one step.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_go_back(host: *mut HostHandle) {
    let _ = with_live_host_mut(host, |handle| {
        crate::commands::dispatch_webview_command(handle, WebViewCommand::GoBack);
    });
}

#[no_mangle]
/// Navigates forward by one step.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_go_forward(host: *mut HostHandle) {
    let _ = with_live_host_mut(host, |handle| {
        crate::commands::dispatch_webview_command(handle, WebViewCommand::GoForward);
    });
}

#[no_mangle]
/// Focuses the WebView for keyboard input.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_focus(host: *mut HostHandle) {
    let _ = with_live_host_mut(host, |handle| {
        crate::commands::dispatch_webview_command(handle, WebViewCommand::Focus);
    });
}

#[no_mangle]
/// Removes focus from the WebView.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_blur(host: *mut HostHandle) {
    let _ = with_live_host_mut(host, |handle| {
        crate::commands::dispatch_webview_command(handle, WebViewCommand::Blur);
    });
}

#[no_mangle]
/// Resolves a pending JavaScript simple dialog.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`, and `dialog_id` must point to a valid,
/// NUL-terminated UTF-8 string. `prompt_value` may be null.
pub unsafe extern "C" fn servo_host_resolve_simple_dialog(
    host: *mut HostHandle,
    dialog_id: *const c_char,
    action: i32,
    prompt_value: *const c_char,
) {
    if host.is_null() || dialog_id.is_null() {
        return;
    }

    let Ok(dialog_id) = unsafe { CStr::from_ptr(dialog_id) }.to_str() else {
        return;
    };
    let Some(confirmed) = simple_dialog_action_from_i32(action) else {
        let _ = with_live_host_mut(host, |handle| {
            let current_url = handle.current_url.clone();
            handle.events.push_back(HostEvent::Error {
                url: current_url,
                code: ServoStatus::BackendError as i32,
                message: format!("simple dialog action {action} is unsupported"),
            });
        });
        return;
    };
    let prompt_value = if prompt_value.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(prompt_value) }.to_str().ok()
    };

    let _ = with_live_host_mut(host, |handle| {
        let result = {
            let Some(backend) = handle.android_backend.as_mut() else {
                return;
            };
            backend.resolve_simple_dialog(dialog_id, confirmed, prompt_value)
        };
        let current_url = handle.current_url.clone();
        queue_backend_result(handle, result, current_url.as_deref());
    });
}

#[no_mangle]
/// Resolves a select element prompt by providing the selected option IDs.
///
/// # Safety
/// - `host` must be a valid HostHandle pointer
/// - `select_element_id` must be a valid C string pointer
/// - `selected_options` must point to an array of at least `selected_options_len` valid usize values
pub unsafe extern "C" fn servo_host_resolve_select_element(
    host: *mut HostHandle,
    select_element_id: *const c_char,
    selected_options: *const usize,
    selected_options_len: usize,
) {
    if host.is_null() || select_element_id.is_null() {
        return;
    }

    let Ok(select_element_id) = unsafe { CStr::from_ptr(select_element_id) }.to_str() else {
        return;
    };

    let selected_options_vec = if selected_options.is_null() || selected_options_len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(selected_options, selected_options_len) }.to_vec()
    };

    let _ = with_live_host_mut(host, |handle| {
        queue_control_command(
            handle,
            ControlCommand::ResolveSelectElement {
                select_element_id: select_element_id.to_string(),
                selected_options: selected_options_vec,
            },
        );
    });
}

#[no_mangle]
/// Resolves a file picker prompt by providing selected file paths.
///
/// # Safety
/// - `host` must be a valid HostHandle pointer
/// - `file_picker_id` must be a valid C string pointer
/// - `selected_paths` must point to an array of at least `selected_paths_len` valid C strings
pub unsafe extern "C" fn servo_host_resolve_file_picker(
    host: *mut HostHandle,
    file_picker_id: *const c_char,
    selected_paths: *const *const c_char,
    selected_paths_len: usize,
) {
    if host.is_null() || file_picker_id.is_null() {
        return;
    }

    let Ok(file_picker_id) = unsafe { CStr::from_ptr(file_picker_id) }.to_str() else {
        return;
    };

    let mut selected_paths_vec = Vec::with_capacity(selected_paths_len);
    if !selected_paths.is_null() {
        for selected_path in
            unsafe { std::slice::from_raw_parts(selected_paths, selected_paths_len) }
        {
            if selected_path.is_null() {
                return;
            }
            let Ok(selected_path) = unsafe { CStr::from_ptr(*selected_path) }.to_str() else {
                return;
            };
            selected_paths_vec.push(selected_path.to_owned());
        }
    } else if selected_paths_len != 0 {
        return;
    }

    let _ = with_live_host_mut(host, |handle| {
        queue_control_command(
            handle,
            ControlCommand::ResolveFilePicker {
                file_picker_id: file_picker_id.to_owned(),
                selected_paths: selected_paths_vec,
            },
        );
    });
}

#[no_mangle]
/// Dismisses a pending file picker prompt.
///
/// # Safety
/// - `host` must be a valid HostHandle pointer
/// - `file_picker_id` must be a valid C string pointer
pub unsafe extern "C" fn servo_host_dismiss_file_picker(
    host: *mut HostHandle,
    file_picker_id: *const c_char,
) {
    if host.is_null() || file_picker_id.is_null() {
        return;
    }

    let Ok(file_picker_id) = unsafe { CStr::from_ptr(file_picker_id) }.to_str() else {
        return;
    };

    let _ = with_live_host_mut(host, |handle| {
        queue_control_command(
            handle,
            ControlCommand::DismissFilePicker {
                file_picker_id: file_picker_id.to_owned(),
            },
        );
    });
}

#[no_mangle]
/// Resolves the pending permission request.
///
/// # Safety
/// `host` must be a valid, live `HostHandle*`.
pub unsafe extern "C" fn servo_host_resolve_permission(
    host: *mut HostHandle,
    allow: bool,
) -> ServoStatus {
    with_live_host_mut(host, |handle| {
        queue_control_command(handle, ControlCommand::ResolvePermission { allow });
        ServoStatus::Ok
    })
    .unwrap_or(ServoStatus::NullPointer)
}

#[no_mangle]
/// Resolves a pending navigation policy request.
///
/// # Safety
/// - `host` must be a valid HostHandle pointer
/// - `navigation_id` must be a valid C string pointer
pub unsafe extern "C" fn servo_host_resolve_navigation_request(
    host: *mut HostHandle,
    navigation_id: *const c_char,
    allow: bool,
) -> ServoStatus {
    if host.is_null() || navigation_id.is_null() {
        return ServoStatus::NullPointer;
    }

    let Ok(navigation_id) = unsafe { CStr::from_ptr(navigation_id) }.to_str() else {
        return ServoStatus::BackendError;
    };

    with_live_host_mut(host, |handle| {
        queue_control_command(
            handle,
            ControlCommand::ResolveNavigationRequest {
                navigation_id: navigation_id.to_owned(),
                allow,
            },
        );
        ServoStatus::Ok
    })
    .unwrap_or(ServoStatus::NullPointer)
}

#[no_mangle]
/// Resolves a pending context menu by selecting an action.
///
/// # Safety
/// - `host` must be a valid HostHandle pointer
/// - `context_menu_id` must be a valid C string pointer
/// - `action` must be a valid C string pointer
pub unsafe extern "C" fn servo_host_resolve_context_menu(
    host: *mut HostHandle,
    context_menu_id: *const c_char,
    action: *const c_char,
) {
    if host.is_null() || context_menu_id.is_null() || action.is_null() {
        return;
    }

    let Ok(context_menu_id) = unsafe { CStr::from_ptr(context_menu_id) }.to_str() else {
        return;
    };
    let Ok(action) = unsafe { CStr::from_ptr(action) }.to_str() else {
        return;
    };
    let Some(action) = ContextMenuAction::from_str(action) else {
        let _ = with_live_host_mut(host, |handle| {
            let current_url = handle.current_url.clone();
            handle.events.push_back(HostEvent::Error {
                url: current_url,
                code: ServoStatus::BackendError as i32,
                message: format!("context menu action {action} is unsupported"),
            });
        });
        return;
    };

    let _ = with_live_host_mut(host, |handle| {
        queue_control_command(
            handle,
            ControlCommand::ResolveContextMenu {
                context_menu_id: context_menu_id.to_owned(),
                action,
            },
        );
    });
}

#[no_mangle]
/// Dismisses a pending context menu.
///
/// # Safety
/// - `host` must be a valid HostHandle pointer
/// - `context_menu_id` must be a valid C string pointer
pub unsafe extern "C" fn servo_host_dismiss_context_menu(
    host: *mut HostHandle,
    context_menu_id: *const c_char,
) {
    if host.is_null() || context_menu_id.is_null() {
        return;
    }

    let Ok(context_menu_id) = unsafe { CStr::from_ptr(context_menu_id) }.to_str() else {
        return;
    };

    let _ = with_live_host_mut(host, |handle| {
        queue_control_command(
            handle,
            ControlCommand::DismissContextMenu {
                context_menu_id: context_menu_id.to_owned(),
            },
        );
    });
}

#[no_mangle]
/// Frees a host handle previously returned by `servo_host_new`.
///
/// # Safety
/// `host` must either be null or a pointer returned by `servo_host_new` that
/// has not already been freed.
pub unsafe extern "C" fn servo_host_free(host: *mut HostHandle) {
    free_live_host(host);
}

#[cfg(test)]
mod tests {
    use super::copy_string;
    use crate::registry::{register_live_host, unregister_live_host};
    use crate::state::ControlCommand;
    use crate::AndroidRenderBackend;
    use crate::*;
    use explorerkit_embedder::{
        encode_host_event_bridge, ContextMenuAction, ContextMenuElementInformation,
        ContextMenuItem, FaviconImage, FaviconPixelFormat, HostEvent, NavigationRequest,
        PopupRequestPolicy, SelectElementOption, SelectElementOptionOrOptgroup, SimpleDialogKind,
        TouchEventKind, WebViewCommand,
    };
    use std::ffi::{c_char, CString};
    use std::mem;
    use std::sync::{Arc, Mutex};

    fn cstring(value: &str) -> CString {
        CString::new(value).unwrap()
    }

    fn copied_string(len: usize, copy: impl FnOnce(*mut c_char, usize) -> usize) -> String {
        let mut buffer = vec![0u8; len + 1];
        let written = copy(buffer.as_mut_ptr().cast(), buffer.len());

        assert_eq!(written, len);
        std::ffi::CStr::from_bytes_until_nul(&buffer)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    }

    fn next_event_bridge_json(host: *mut HostHandle) -> Option<String> {
        let len = unsafe { servo_host_take_next_event_bridge_json_len(host) };
        if len == 0 {
            return None;
        }

        Some(copied_string(len, |output, capacity| unsafe {
            servo_host_last_event_bridge_json_copy(host, output, capacity)
        }))
    }

    fn assert_next_event(host: *mut HostHandle, expected: HostEvent) {
        assert_eq!(
            next_event_bridge_json(host),
            Some(encode_host_event_bridge(&expected))
        );
    }

    fn assert_no_event(host: *mut HostHandle) {
        assert_eq!(next_event_bridge_json(host), None);
    }

    #[test]
    fn creates_loads_and_frees_a_host_handle() {
        let host = servo_host_new(0);
        assert!(!host.is_null());

        let url = cstring("https://example.com");
        let status = unsafe { servo_host_load_url(host, url.as_ptr()) };
        assert_eq!(status, ServoStatus::Ok);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn accepts_data_urls_when_loading_a_host() {
        let host = servo_host_new(0);
        let url = cstring("data:text/plain,hello");
        let status = unsafe { servo_host_load_url(host, url.as_ptr()) };
        assert_eq!(status, ServoStatus::Ok);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn exposes_the_last_error_message_after_a_failed_load() {
        let host = servo_host_new(0);
        let url = cstring("https:///");
        assert_eq!(
            unsafe { servo_host_load_url(host, url.as_ptr()) },
            ServoStatus::InvalidUrl
        );

        let len = unsafe { servo_host_last_error_message_len(host) };
        let mut buffer = vec![0u8; len as usize + 1];
        let written = unsafe {
            servo_host_last_error_message_copy(host, buffer.as_mut_ptr().cast(), buffer.len())
        };

        assert_eq!(written, len);
        let message = std::ffi::CStr::from_bytes_until_nul(&buffer)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(message.contains("invalid url"));

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn keeps_requested_url_pending_until_servo_reports_it() {
        let host = servo_host_new(0);
        let url = cstring("example.com");

        assert_eq!(
            unsafe { servo_host_load_url(host, url.as_ptr()) },
            ServoStatus::Ok
        );
        assert_no_event(host);
        assert_eq!(
            copied_string(
                unsafe { servo_host_current_url_len(host) },
                |output, capacity| unsafe { servo_host_current_url_copy(host, output, capacity) }
            ),
            ""
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn drains_surface_lifecycle_events_in_order() {
        let host = servo_host_new(0);

        unsafe {
            servo_host_attach_surface(host, 640, 480);
            servo_host_resize_surface(host, 800, 600, 1.0);
            servo_host_detach_surface(host);
        }

        assert_next_event(
            host,
            HostEvent::SurfaceAttached {
                size: SurfaceSize::new(640, 480),
            },
        );
        assert_next_event(
            host,
            HostEvent::SurfaceResized {
                size: SurfaceSize::new(800, 600),
            },
        );
        assert_next_event(host, HostEvent::SurfaceDetached);
        assert_no_event(host);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn ignores_duplicate_surface_attach_until_detach() {
        let host = servo_host_new(0);

        unsafe {
            servo_host_attach_surface(host, 640, 480);
            servo_host_attach_surface(host, 800, 600);
        }

        assert_next_event(
            host,
            HostEvent::SurfaceAttached {
                size: SurfaceSize::new(640, 480),
            },
        );
        assert_no_event(host);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn drains_failure_details_through_bridge_json() {
        let host = servo_host_new(0);
        let url = cstring("https:///");

        assert_eq!(
            unsafe { servo_host_load_url(host, url.as_ptr()) },
            ServoStatus::InvalidUrl
        );

        let event_json = next_event_bridge_json(host).unwrap();
        assert!(event_json.starts_with(
            r#"{"name":"error","payload":{"url":"https:///","code":1,"message":"invalid url"#
        ));

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn bridge_json_drain_returns_none_when_the_queue_is_exhausted() {
        let host = servo_host_new(0);
        let url = cstring("https:///");

        assert_eq!(
            unsafe { servo_host_load_url(host, url.as_ptr()) },
            ServoStatus::InvalidUrl
        );
        assert!(next_event_bridge_json(host).is_some());
        assert_no_event(host);
        assert_eq!(
            copied_string(0, |output, capacity| unsafe {
                servo_host_last_event_bridge_json_copy(host, output, capacity)
            }),
            ""
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn bridge_json_skips_native_only_favicon_events() {
        let host = servo_host_new(0);
        unsafe {
            let handle = &mut *host;
            handle.events.push_back(HostEvent::FaviconChanged {
                favicon: Some(FaviconImage {
                    width: 1,
                    height: 1,
                    format: FaviconPixelFormat::RGBA8,
                    bytes: vec![1, 2, 3, 4],
                }),
            });
            handle.events.push_back(HostEvent::UrlChanged {
                url: "https://example.com/".to_owned(),
            });
        }

        assert_next_event(
            host,
            HostEvent::UrlChanged {
                url: "https://example.com/".to_owned(),
            },
        );
        assert_no_event(host);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn bridge_json_drain_pops_one_event_at_a_time() {
        let mut handle = HostHandle::new();
        handle.events.push_back(HostEvent::UrlChanged {
            url: "https://example.com/".to_owned(),
        });
        handle
            .events
            .push_back(HostEvent::StatusTextChanged { status: None });

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);

        assert_eq!(
            next_event_bridge_json(host).as_deref(),
            Some(r#"{"name":"urlChanged","payload":{"url":"https://example.com/"}}"#)
        );
        assert_eq!(
            next_event_bridge_json(host).as_deref(),
            Some(r#"{"name":"statusTextChanged","payload":{"status":null}}"#)
        );
        assert_no_event(host);
        assert_eq!(
            copied_string(0, |output, capacity| unsafe {
                servo_host_last_event_bridge_json_copy(host, output, capacity)
            }),
            ""
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn drains_simple_dialog_details_through_bridge_json() {
        let event = HostEvent::SimpleDialogRequested {
            dialog_id: "dialog-1".to_owned(),
            kind: SimpleDialogKind::Prompt,
            message: "Name?".to_owned(),
            default_value: Some("Servo".to_owned()),
        };
        let mut handle = HostHandle::new();
        handle.events.push_back(event.clone());

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);

        assert_next_event(host, event);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn drains_select_element_details_through_bridge_json() {
        let event = HostEvent::SelectElementRequested {
            select_element_id: "select-1".to_owned(),
            options: vec![
                SelectElementOptionOrOptgroup::Option(SelectElementOption {
                    id: 1,
                    label: "Servo".to_owned(),
                    is_disabled: false,
                }),
                SelectElementOptionOrOptgroup::Optgroup {
                    label: "Engines".to_owned(),
                    options: vec![
                        SelectElementOption {
                            id: 4,
                            label: "Layout".to_owned(),
                            is_disabled: false,
                        },
                        SelectElementOption {
                            id: 9,
                            label: "GPU".to_owned(),
                            is_disabled: true,
                        },
                    ],
                },
            ],
            selected_options: vec![1, 4],
            allow_select_multiple: true,
        };
        let mut handle = HostHandle::new();
        handle.events.push_back(event.clone());

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);

        assert_next_event(host, event);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn drains_context_menu_details_through_bridge_json() {
        let event = HostEvent::ContextMenuRequested {
            context_menu_id: "context-menu-1".to_owned(),
            x: 12,
            y: 24,
            width: 48,
            height: 64,
            element_info: ContextMenuElementInformation {
                is_link: true,
                is_image: true,
                is_editable_text: false,
                has_selection: true,
                link_url: Some("https://example.com/".to_owned()),
                image_url: Some("https://example.com/image.png".to_owned()),
                context_type: "link".to_owned(),
            },
            items: vec![
                ContextMenuItem::Item {
                    label: "Copy link".to_owned(),
                    action: ContextMenuAction::CopyLink,
                    enabled: true,
                },
                ContextMenuItem::Separator,
                ContextMenuItem::Item {
                    label: "Reload".to_owned(),
                    action: ContextMenuAction::Reload,
                    enabled: false,
                },
            ],
        };
        let mut handle = HostHandle::new();
        handle.events.push_back(event.clone());

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);

        assert_next_event(host, event);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn drains_file_picker_details_through_bridge_json() {
        let event = HostEvent::FilePickerRequested {
            file_picker_id: "file-picker-1".to_owned(),
            current_paths: vec![
                "/data/user/0/com.kepler.explorerkit.androidexample/cache/one.txt".to_owned(),
                "/data/user/0/com.kepler.explorerkit.androidexample/cache/two.png".to_owned(),
            ],
            filter_patterns: vec!["txt".to_owned(), "png".to_owned()],
            allow_select_multiple: true,
        };
        let mut handle = HostHandle::new();
        handle.events.push_back(event.clone());

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);

        assert_next_event(host, event);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn last_error_accessors_do_not_fall_back_to_drained_bridge_events() {
        let host = servo_host_new(0);
        let url = cstring("https:///");

        assert_eq!(
            unsafe { servo_host_load_url(host, url.as_ptr()) },
            ServoStatus::InvalidUrl
        );
        assert_eq!(
            unsafe { servo_host_last_error_code(host) },
            ServoStatus::InvalidUrl as i32
        );
        assert!(copied_string(
            unsafe { servo_host_last_error_message_len(host) },
            |output, capacity| unsafe {
                servo_host_last_error_message_copy(host, output, capacity)
            }
        )
        .contains("invalid url"));

        assert!(next_event_bridge_json(host).is_some());
        assert_eq!(unsafe { servo_host_last_error_code(host) }, 0);
        assert_eq!(unsafe { servo_host_last_error_message_len(host) }, 0);
        assert_eq!(
            copied_string(
                unsafe { servo_host_last_error_message_len(host) },
                |output, capacity| unsafe {
                    servo_host_last_error_message_copy(host, output, capacity)
                }
            ),
            ""
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn copy_string_truncates_and_terminates_small_buffers() {
        let mut buffer = [b'X' as c_char; 4];

        let written = copy_string(Some("hello"), buffer.as_mut_ptr(), buffer.len());

        assert_eq!(written, 3);
        assert_eq!(buffer[0] as u8, b'h');
        assert_eq!(buffer[1] as u8, b'e');
        assert_eq!(buffer[2] as u8, b'l');
        assert_eq!(buffer[3], 0);
    }

    struct FakeAndroidBackend {
        attach_error: Option<String>,
        detach_error: Option<String>,
        touch_events: Arc<Mutex<Vec<(TouchEventKind, i32, f32, f32)>>>,
        context_menu_points: Arc<Mutex<Vec<(f32, f32)>>>,
        select_element_resolutions: Arc<Mutex<Vec<(String, Vec<usize>)>>>,
        update_events: Vec<HostEvent>,
    }

    impl Default for FakeAndroidBackend {
        fn default() -> Self {
            Self {
                attach_error: None,
                detach_error: None,
                touch_events: Arc::new(Mutex::new(vec![])),
                context_menu_points: Arc::new(Mutex::new(vec![])),
                select_element_resolutions: Arc::new(Mutex::new(vec![])),
                update_events: vec![],
            }
        }
    }

    impl AndroidRenderBackend for FakeAndroidBackend {
        fn attach_surface(
            &mut self,
            _native_window: NativeWindowHandle,
            _size: SurfaceSize,
            _density: f32,
        ) -> Result<Vec<HostEvent>, String> {
            if let Some(message) = self.attach_error.clone() {
                return Err(message);
            }
            Ok(vec![])
        }

        fn resize_surface(
            &mut self,
            _size: SurfaceSize,
            _density: f32,
        ) -> Result<Vec<HostEvent>, String> {
            Ok(vec![])
        }

        fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
            if let Some(message) = self.detach_error.clone() {
                return Err(message);
            }
            Ok(vec![])
        }

        fn load_url(&mut self, url: &str) -> Result<Vec<HostEvent>, String> {
            Ok(vec![HostEvent::UrlChanged {
                url: url.to_owned(),
            }])
        }

        fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
            Ok(vec![])
        }

        fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
            Ok(vec![])
        }

        fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
            Ok(vec![])
        }

        fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
            Ok(vec![])
        }

        fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
            Ok(vec![])
        }

        fn dispatch_touch_event(
            &mut self,
            event_kind: TouchEventKind,
            touch_id: i32,
            x: f32,
            y: f32,
        ) -> Result<Vec<HostEvent>, String> {
            self.touch_events
                .lock()
                .expect("touch log should be available")
                .push((event_kind, touch_id, x, y));
            Ok(vec![])
        }

        fn trigger_context_menu(&mut self, x: f32, y: f32) -> Result<Vec<HostEvent>, String> {
            self.context_menu_points
                .lock()
                .expect("context menu log should be available")
                .push((x, y));
            Ok(vec![])
        }

        fn resolve_select_element(
            &mut self,
            select_element_id: &str,
            selected_options: Vec<usize>,
        ) -> Result<Vec<HostEvent>, String> {
            self.select_element_resolutions
                .lock()
                .expect("select element resolution log should be available")
                .push((select_element_id.to_owned(), selected_options));
            Ok(vec![])
        }

        fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
            Ok(mem::take(&mut self.update_events))
        }

        fn shutdown(&mut self) {}
    }

    #[test]
    fn validates_each_initial_url_before_retaining_the_last_accepted_navigation() {
        for (requests, expected_url) in [
            (
                [
                    (" example.com/a ", ServoStatus::Ok),
                    ("https:///", ServoStatus::InvalidUrl),
                ],
                "https://example.com/a",
            ),
            (
                [
                    ("https:///", ServoStatus::InvalidUrl),
                    ("example.com/b", ServoStatus::Ok),
                ],
                "https://example.com/b",
            ),
            (
                [
                    ("example.com/a", ServoStatus::Ok),
                    ("example.com/b", ServoStatus::Ok),
                ],
                "https://example.com/b",
            ),
        ] {
            let host = servo_host_new(0);
            for (input, expected_status) in requests {
                let input = cstring(input);
                assert_eq!(
                    unsafe { servo_host_load_url(host, input.as_ptr()) },
                    expected_status
                );
            }

            let handle = unsafe { &mut *host };
            assert_eq!(handle.pending_url.as_deref(), Some(expected_url));
            assert!(handle.current_url.is_none());
            assert!(handle.android_backend.is_none());
            assert!(handle.surface.is_none());
            assert!(handle.native_window.is_none());
            for (input, status) in requests {
                if status == ServoStatus::InvalidUrl {
                    assert!(matches!(
                        handle.events.pop_front(),
                        Some(HostEvent::Error { url: Some(url), code, .. })
                            if url == input && code == ServoStatus::InvalidUrl as i32
                    ));
                }
            }
            assert!(handle.events.is_empty());

            handle.set_android_backend_for_tests(Box::new(FakeAndroidBackend::default()));
            handle.attach_android_surface_for_tests(
                0xCAFEusize as NativeWindowHandle,
                SurfaceSize::new(640, 480),
                2.0,
            );

            assert!(handle.pending_url.is_none());
            assert_eq!(handle.current_url.as_deref(), Some(expected_url));
            assert_eq!(
                handle.events.drain(..).collect::<Vec<_>>(),
                vec![
                    HostEvent::SurfaceAttached {
                        size: SurfaceSize::new(640, 480)
                    },
                    HostEvent::UrlChanged {
                        url: expected_url.to_owned()
                    },
                ]
            );

            unsafe { servo_host_free(host) };
        }
    }

    #[test]
    fn defers_android_navigation_until_a_native_window_is_attached() {
        let mut handle = HostHandle::new();
        handle.set_android_backend_for_tests(Box::new(FakeAndroidBackend::default()));

        let request = NavigationRequest::new("example.com").unwrap();
        handle.load_android_request_for_tests(request.clone());

        assert_eq!(handle.pending_url.as_deref(), Some("https://example.com/"));
        assert_eq!(handle.current_url, None);
        assert!(handle.events.is_empty());

        handle.attach_android_surface_for_tests(
            0xCAFEusize as NativeWindowHandle,
            SurfaceSize::new(640, 480),
            2.0,
        );

        assert_eq!(
            handle.events.pop_front(),
            Some(HostEvent::SurfaceAttached {
                size: SurfaceSize::new(640, 480),
            })
        );
        assert_eq!(
            handle.events.pop_front(),
            Some(HostEvent::UrlChanged {
                url: "https://example.com/".to_owned(),
            })
        );
        assert_eq!(handle.current_url.as_deref(), Some("https://example.com/"));
    }

    #[test]
    fn token_load_url_commands_stay_pending_until_a_native_window_is_attached() {
        let mut handle = HostHandle::new();
        handle.set_android_backend_for_tests(Box::new(FakeAndroidBackend::default()));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let token = servo_host_controller_handle_token(host).unwrap();

        servo_host_load_url_with_token(&token, "example.com").unwrap();

        let handle = unsafe { &mut *host };
        assert!(matches!(
            handle.pending_commands.front(),
            Some(ControlCommand::WebView(WebViewCommand::LoadUrl(request)))
                if request.url == "https://example.com/"
        ));
        assert!(handle.pending_url.is_none());
        assert!(handle.events.is_empty());

        unsafe { servo_host_perform_updates(host) };

        let handle = unsafe { &mut *host };
        assert_eq!(handle.pending_url.as_deref(), Some("https://example.com/"));
        assert_eq!(handle.current_url, None);
        assert!(handle.events.is_empty());

        handle.attach_android_surface_for_tests(
            0xCAFEusize as NativeWindowHandle,
            SurfaceSize::new(640, 480),
            2.0,
        );

        assert_eq!(
            handle.events.pop_front(),
            Some(HostEvent::SurfaceAttached {
                size: SurfaceSize::new(640, 480),
            })
        );
        assert_eq!(
            handle.events.pop_front(),
            Some(HostEvent::UrlChanged {
                url: "https://example.com/".to_owned(),
            })
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn only_url_changed_updates_observed_android_url() {
        let update_events = vec![
            HostEvent::UrlChanged {
                url: "https://observed.example/".to_owned(),
            },
            HostEvent::LoadStatusChanged {
                status: explorerkit_embedder::LoadStatusKind::Complete,
            },
            HostEvent::HistoryChanged {
                entries: vec!["https://history.example/".to_owned()],
                current: 0,
                can_go_back: false,
                can_go_forward: false,
            },
            HostEvent::Error {
                url: Some("https://error.example/".to_owned()),
                code: ServoStatus::BackendError as i32,
                message: "diagnostic error".to_owned(),
            },
            HostEvent::Crashed {
                url: Some("https://crash.example/".to_owned()),
                reason: "diagnostic crash".to_owned(),
                backtrace: None,
            },
        ];
        let backend = FakeAndroidBackend {
            update_events: update_events.clone(),
            ..FakeAndroidBackend::default()
        };

        let mut handle = HostHandle::new();
        handle.set_android_backend_for_tests(Box::new(backend));

        handle.perform_android_updates_for_tests();

        assert_eq!(handle.events.drain(..).collect::<Vec<_>>(), update_events);
        assert_eq!(
            handle.current_url.as_deref(),
            Some("https://observed.example/")
        );
    }

    #[test]
    fn failed_android_attach_keeps_navigation_pending() {
        let backend = FakeAndroidBackend {
            attach_error: Some("surface boot failed".to_owned()),
            ..FakeAndroidBackend::default()
        };

        let mut handle = HostHandle::new();
        handle.set_android_backend_for_tests(Box::new(backend));

        crate::commands::load_navigation_request_from_str(&mut handle, "example.com").unwrap();
        assert_eq!(
            crate::commands::load_navigation_request_from_str(&mut handle, "https:///"),
            Err(ServoStatus::InvalidUrl)
        );
        assert!(matches!(
            handle.events.pop_front(),
            Some(HostEvent::Error { code: 1, .. })
        ));
        handle.attach_android_surface_for_tests(
            0xCAFEusize as NativeWindowHandle,
            SurfaceSize::new(640, 480),
            2.0,
        );

        assert_eq!(handle.pending_url.as_deref(), Some("https://example.com/"));
        assert!(handle.surface.is_none());
        assert!(handle.native_window.is_none());
        assert_eq!(
            handle.events.pop_front(),
            Some(HostEvent::Error {
                url: None,
                code: ServoStatus::BackendError as i32,
                message: "surface boot failed".to_owned(),
            })
        );
        assert!(handle.events.is_empty());
    }

    #[test]
    fn failed_android_detach_keeps_surface_state() {
        let backend = FakeAndroidBackend {
            detach_error: Some("surface detach failed".to_owned()),
            ..FakeAndroidBackend::default()
        };

        let mut handle = HostHandle::new();
        handle.set_android_backend_for_tests(Box::new(backend));
        handle.attach_android_surface_for_tests(
            0xCAFEusize as NativeWindowHandle,
            SurfaceSize::new(640, 480),
            2.0,
        );
        assert_eq!(
            handle.events.pop_front(),
            Some(HostEvent::SurfaceAttached {
                size: SurfaceSize::new(640, 480),
            })
        );

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        unsafe { servo_host_detach_surface(host) };
        unregister_live_host(host);
        let mut handle = unsafe { Box::from_raw(host) };

        assert_eq!(handle.surface, Some(SurfaceSize::new(640, 480)));
        assert_eq!(
            handle.native_window,
            Some(0xCAFEusize as NativeWindowHandle)
        );
        assert_eq!(
            handle.events.pop_front(),
            Some(HostEvent::Error {
                url: None,
                code: ServoStatus::BackendError as i32,
                message: "surface detach failed".to_owned(),
            })
        );
        assert!(handle.events.is_empty());
    }

    #[test]
    fn forwards_touch_events_to_the_android_backend() {
        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        let touch_events = Arc::new(Mutex::new(vec![]));
        handle.set_android_backend_for_tests(Box::new(FakeAndroidBackend {
            touch_events: touch_events.clone(),
            ..FakeAndroidBackend::default()
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        unsafe {
            servo_host_dispatch_touch_event(host, 0, 7, 12.5, 30.25);
            servo_host_dispatch_touch_event(host, 1, 7, 20.0, 40.0);
        }
        unregister_live_host(host);
        let handle = unsafe { Box::from_raw(host) };

        assert_eq!(
            *touch_events.lock().expect("touch log should be readable"),
            vec![
                (TouchEventKind::Down, 7, 12.5, 30.25),
                (TouchEventKind::Move, 7, 20.0, 40.0),
            ]
        );
        assert!(handle.events.is_empty());
    }

    #[test]
    fn forwards_context_menu_triggers_to_the_android_backend() {
        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        let context_menu_points = Arc::new(Mutex::new(vec![]));
        handle.set_android_backend_for_tests(Box::new(FakeAndroidBackend {
            context_menu_points: context_menu_points.clone(),
            ..FakeAndroidBackend::default()
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        unsafe {
            servo_host_trigger_context_menu(host, 18.0, 42.5);
        }
        unregister_live_host(host);
        let handle = unsafe { Box::from_raw(host) };

        assert_eq!(
            *context_menu_points
                .lock()
                .expect("context menu log should be readable"),
            vec![(18.0, 42.5)]
        );
        assert!(handle.events.is_empty());
    }

    #[test]
    fn forwards_all_selected_options_to_the_android_backend() {
        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        let select_element_resolutions = Arc::new(Mutex::new(vec![]));
        handle.set_android_backend_for_tests(Box::new(FakeAndroidBackend {
            select_element_resolutions: select_element_resolutions.clone(),
            ..FakeAndroidBackend::default()
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let select_element_id = cstring("select-1");
        let selected_options = [1_usize, 4, 9];

        unsafe {
            servo_host_resolve_select_element(
                host,
                select_element_id.as_ptr(),
                selected_options.as_ptr(),
                selected_options.len(),
            );
        }
        assert!(select_element_resolutions
            .lock()
            .expect("select resolution log should be readable")
            .is_empty());

        unsafe { servo_host_perform_updates(host) };
        unregister_live_host(host);
        let handle = unsafe { Box::from_raw(host) };

        assert_eq!(
            *select_element_resolutions
                .lock()
                .expect("select resolution log should be readable"),
            vec![("select-1".to_owned(), vec![1, 4, 9])]
        );
        assert!(handle.events.is_empty());
    }

    #[test]
    fn navigation_commands_are_no_ops_without_a_backend() {
        let host = servo_host_new(0);

        unsafe {
            servo_host_reload(host);
            servo_host_go_back(host);
            servo_host_go_forward(host);
            servo_host_focus(host);
            servo_host_blur(host);
        }

        assert_no_event(host);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn navigation_commands_delegate_to_the_backend_when_present() {
        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);

        let calls = Arc::new(Mutex::new(vec![]));

        struct CountingBackend {
            calls: Arc<Mutex<Vec<&'static str>>>,
        }

        impl AndroidRenderBackend for CountingBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }
            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }
            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }
            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }
            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push("reload");
                Ok(vec![])
            }
            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push("go_back");
                Ok(vec![])
            }
            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push("go_forward");
                Ok(vec![])
            }
            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push("focus");
                Ok(vec![])
            }
            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push("blur");
                Ok(vec![])
            }
            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }
            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }
            fn shutdown(&mut self) {}
        }

        handle.set_android_backend_for_tests(Box::new(CountingBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        unsafe {
            servo_host_reload(host);
            servo_host_go_back(host);
            servo_host_go_forward(host);
            servo_host_focus(host);
            servo_host_blur(host);
        }
        unsafe { servo_host_free(host) };

        assert_eq!(
            *calls.lock().unwrap(),
            vec!["reload", "go_back", "go_forward", "focus", "blur"]
        );
    }

    #[test]
    fn token_commands_are_drained_by_the_frame_update_loop() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        };
        use std::thread;
        use std::time::Duration;

        struct BlockingBackend {
            active: Arc<AtomicUsize>,
            reload_started: mpsc::Sender<()>,
            allow_reload_finish: mpsc::Receiver<()>,
        }

        impl AndroidRenderBackend for BlockingBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                assert_eq!(self.active.fetch_add(1, Ordering::SeqCst), 0);
                self.reload_started.send(()).unwrap();
                self.allow_reload_finish.recv().unwrap();
                self.active.fetch_sub(1, Ordering::SeqCst);
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                assert_eq!(self.active.fetch_add(1, Ordering::SeqCst), 0);
                self.active.fetch_sub(1, Ordering::SeqCst);
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let active = Arc::new(AtomicUsize::new(0));
        let (reload_started_tx, reload_started_rx) = mpsc::channel();
        let (allow_reload_finish_tx, allow_reload_finish_rx) = mpsc::channel();

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(BlockingBackend {
            active: active.clone(),
            reload_started: reload_started_tx,
            allow_reload_finish: allow_reload_finish_rx,
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let token = servo_host_controller_handle_token(host).unwrap();

        servo_host_reload_with_token(&token).unwrap();
        assert!(reload_started_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err());

        let host_address = host as usize;
        let updates_thread = thread::spawn(move || unsafe {
            servo_host_perform_updates(host_address as *mut HostHandle);
        });

        reload_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        allow_reload_finish_tx.send(()).unwrap();
        updates_thread.join().unwrap();

        assert_eq!(active.load(Ordering::SeqCst), 0);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn token_reloads_are_coalesced_before_updates_run() {
        let calls = Arc::new(Mutex::new(vec![]));

        struct CountingBackend {
            calls: Arc<Mutex<Vec<&'static str>>>,
        }

        impl AndroidRenderBackend for CountingBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push("reload");
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(CountingBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let token = servo_host_controller_handle_token(host).unwrap();

        servo_host_reload_with_token(&token).unwrap();
        servo_host_reload_with_token(&token).unwrap();
        unsafe { servo_host_perform_updates(host) };

        assert_eq!(*calls.lock().unwrap(), vec!["reload"]);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn token_navigation_commands_do_not_collapse_repeated_inputs() {
        let calls = Arc::new(Mutex::new(vec![]));

        struct CountingBackend {
            calls: Arc<Mutex<Vec<&'static str>>>,
        }

        impl AndroidRenderBackend for CountingBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push("go_back");
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(CountingBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let token = servo_host_controller_handle_token(host).unwrap();

        servo_host_go_back_with_token(&token).unwrap();
        servo_host_go_back_with_token(&token).unwrap();
        unsafe { servo_host_perform_updates(host) };

        assert_eq!(*calls.lock().unwrap(), vec!["go_back", "go_back"]);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn token_simple_dialog_resolutions_are_drained_by_the_frame_update_loop() {
        let calls = Arc::new(Mutex::new(Vec::<(String, bool, Option<String>)>::new()));

        struct DialogBackend {
            calls: Arc<Mutex<Vec<(String, bool, Option<String>)>>>,
        }

        impl AndroidRenderBackend for DialogBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resolve_simple_dialog(
                &mut self,
                dialog_id: &str,
                confirmed: bool,
                prompt_value: Option<&str>,
            ) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push((
                    dialog_id.to_owned(),
                    confirmed,
                    prompt_value.map(str::to_owned),
                ));
                Ok(vec![])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(DialogBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let token = servo_host_controller_handle_token(host).unwrap();

        servo_host_resolve_simple_dialog_with_token(&token, "dialog-1", true, Some("Servo"))
            .unwrap();
        assert!(calls.lock().unwrap().is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(
            *calls.lock().unwrap(),
            vec![("dialog-1".to_owned(), true, Some("Servo".to_owned()))]
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn token_navigation_request_resolutions_are_drained_by_the_frame_update_loop() {
        let calls = Arc::new(Mutex::new(Vec::<(String, bool)>::new()));

        struct NavigationPolicyBackend {
            calls: Arc<Mutex<Vec<(String, bool)>>>,
        }

        impl AndroidRenderBackend for NavigationPolicyBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resolve_navigation_request(
                &mut self,
                navigation_id: &str,
                allow: bool,
            ) -> Result<Vec<HostEvent>, String> {
                self.calls
                    .lock()
                    .unwrap()
                    .push((navigation_id.to_owned(), allow));
                Ok(vec![])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(NavigationPolicyBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let token = servo_host_controller_handle_token(host).unwrap();

        servo_host_resolve_navigation_request_with_token(&token, "navigation-1", false).unwrap();
        assert!(calls.lock().unwrap().is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(
            *calls.lock().unwrap(),
            vec![("navigation-1".to_owned(), false)]
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn token_context_menu_resolutions_are_drained_by_the_frame_update_loop() {
        let calls = Arc::new(Mutex::new(Vec::<(String, ContextMenuAction)>::new()));

        struct ContextMenuBackend {
            calls: Arc<Mutex<Vec<(String, ContextMenuAction)>>>,
        }

        impl AndroidRenderBackend for ContextMenuBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resolve_context_menu(
                &mut self,
                context_menu_id: &str,
                action: ContextMenuAction,
            ) -> Result<Vec<HostEvent>, String> {
                self.calls
                    .lock()
                    .unwrap()
                    .push((context_menu_id.to_owned(), action));
                Ok(vec![])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(ContextMenuBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let token = servo_host_controller_handle_token(host).unwrap();

        servo_host_resolve_context_menu_with_token(
            &token,
            "context-menu-1",
            ContextMenuAction::CopyLink,
        )
        .unwrap();
        assert!(calls.lock().unwrap().is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(
            *calls.lock().unwrap(),
            vec![("context-menu-1".to_owned(), ContextMenuAction::CopyLink)]
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn token_context_menu_dismissals_are_drained_by_the_frame_update_loop() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));

        struct ContextMenuBackend {
            calls: Arc<Mutex<Vec<String>>>,
        }

        impl AndroidRenderBackend for ContextMenuBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dismiss_context_menu(
                &mut self,
                context_menu_id: &str,
            ) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push(context_menu_id.to_owned());
                Ok(vec![])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(ContextMenuBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let token = servo_host_controller_handle_token(host).unwrap();

        servo_host_dismiss_context_menu_with_token(&token, "context-menu-2").unwrap();
        assert!(calls.lock().unwrap().is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(*calls.lock().unwrap(), vec!["context-menu-2".to_owned()]);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn context_menu_token_ffi_rejects_unsupported_actions() {
        let host = servo_host_new(0);
        let token = servo_host_controller_handle_token(host).unwrap();
        let token = cstring(&token);
        let context_menu_id = cstring("context-menu-3");
        let action = cstring("unsupported");

        assert_eq!(
            unsafe {
                servo_host_resolve_context_menu_with_token_ffi(
                    token.as_ptr(),
                    context_menu_id.as_ptr(),
                    action.as_ptr(),
                )
            },
            ServoStatus::BackendError
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn simple_dialog_resolution_is_delegated_to_the_backend() {
        let calls = Arc::new(Mutex::new(Vec::<(String, bool, Option<String>)>::new()));

        struct DialogBackend {
            calls: Arc<Mutex<Vec<(String, bool, Option<String>)>>>,
        }

        impl AndroidRenderBackend for DialogBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resolve_simple_dialog(
                &mut self,
                dialog_id: &str,
                confirmed: bool,
                prompt_value: Option<&str>,
            ) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push((
                    dialog_id.to_owned(),
                    confirmed,
                    prompt_value.map(str::to_owned),
                ));
                Ok(vec![HostEvent::SimpleDialogDismissed {
                    dialog_id: dialog_id.to_owned(),
                }])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(DialogBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let dialog_id = cstring("dialog-1");
        let prompt_value = cstring("Servo");

        unsafe {
            servo_host_resolve_simple_dialog(host, dialog_id.as_ptr(), 0, prompt_value.as_ptr());
        }

        assert_eq!(
            *calls.lock().unwrap(),
            vec![("dialog-1".to_owned(), true, Some("Servo".to_owned()))]
        );
        assert_next_event(
            host,
            HostEvent::SimpleDialogDismissed {
                dialog_id: "dialog-1".to_owned(),
            },
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn context_menu_resolution_is_delegated_to_the_backend_on_updates() {
        let calls = Arc::new(Mutex::new(Vec::<(String, ContextMenuAction)>::new()));

        struct ContextMenuBackend {
            calls: Arc<Mutex<Vec<(String, ContextMenuAction)>>>,
        }

        impl AndroidRenderBackend for ContextMenuBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resolve_context_menu(
                &mut self,
                context_menu_id: &str,
                action: ContextMenuAction,
            ) -> Result<Vec<HostEvent>, String> {
                self.calls
                    .lock()
                    .unwrap()
                    .push((context_menu_id.to_owned(), action));
                Ok(vec![HostEvent::ContextMenuDismissed {
                    context_menu_id: context_menu_id.to_owned(),
                }])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(ContextMenuBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let context_menu_id = cstring("context-menu-1");
        let action = cstring("copy-link");

        unsafe {
            servo_host_resolve_context_menu(host, context_menu_id.as_ptr(), action.as_ptr());
        }
        assert!(calls.lock().unwrap().is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(
            *calls.lock().unwrap(),
            vec![("context-menu-1".to_owned(), ContextMenuAction::CopyLink)]
        );
        assert_next_event(
            host,
            HostEvent::ContextMenuDismissed {
                context_menu_id: "context-menu-1".to_owned(),
            },
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn context_menu_dismissal_is_delegated_to_the_backend_on_updates() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));

        struct ContextMenuBackend {
            calls: Arc<Mutex<Vec<String>>>,
        }

        impl AndroidRenderBackend for ContextMenuBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dismiss_context_menu(
                &mut self,
                context_menu_id: &str,
            ) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push(context_menu_id.to_owned());
                Ok(vec![HostEvent::ContextMenuDismissed {
                    context_menu_id: context_menu_id.to_owned(),
                }])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(ContextMenuBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let context_menu_id = cstring("context-menu-2");

        unsafe {
            servo_host_dismiss_context_menu(host, context_menu_id.as_ptr());
        }
        assert!(calls.lock().unwrap().is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(*calls.lock().unwrap(), vec!["context-menu-2".to_owned()]);
        assert_next_event(
            host,
            HostEvent::ContextMenuDismissed {
                context_menu_id: "context-menu-2".to_owned(),
            },
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn file_picker_resolution_is_delegated_to_the_backend_on_updates() {
        let calls = Arc::new(Mutex::new(Vec::<(String, Vec<String>)>::new()));

        struct FilePickerBackend {
            calls: Arc<Mutex<Vec<(String, Vec<String>)>>>,
        }

        impl AndroidRenderBackend for FilePickerBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resolve_file_picker(
                &mut self,
                file_picker_id: &str,
                selected_paths: Vec<String>,
            ) -> Result<Vec<HostEvent>, String> {
                self.calls
                    .lock()
                    .unwrap()
                    .push((file_picker_id.to_owned(), selected_paths));
                Ok(vec![HostEvent::FilePickerDismissed {
                    file_picker_id: file_picker_id.to_owned(),
                }])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(FilePickerBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let file_picker_id = cstring("file-picker-1");
        let selected_path_a = cstring("/cache/one.txt");
        let selected_path_b = cstring("/cache/two.png");
        let selected_paths = [selected_path_a.as_ptr(), selected_path_b.as_ptr()];

        unsafe {
            servo_host_resolve_file_picker(
                host,
                file_picker_id.as_ptr(),
                selected_paths.as_ptr(),
                selected_paths.len(),
            );
        }
        assert!(calls.lock().unwrap().is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(
            *calls.lock().unwrap(),
            vec![(
                "file-picker-1".to_owned(),
                vec!["/cache/one.txt".to_owned(), "/cache/two.png".to_owned()]
            )]
        );
        assert_next_event(
            host,
            HostEvent::FilePickerDismissed {
                file_picker_id: "file-picker-1".to_owned(),
            },
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn file_picker_dismissal_is_delegated_to_the_backend_on_updates() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));

        struct FilePickerBackend {
            calls: Arc<Mutex<Vec<String>>>,
        }

        impl AndroidRenderBackend for FilePickerBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dismiss_file_picker(
                &mut self,
                file_picker_id: &str,
            ) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push(file_picker_id.to_owned());
                Ok(vec![HostEvent::FilePickerDismissed {
                    file_picker_id: file_picker_id.to_owned(),
                }])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(FilePickerBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let file_picker_id = cstring("file-picker-2");

        unsafe {
            servo_host_dismiss_file_picker(host, file_picker_id.as_ptr());
        }
        assert!(calls.lock().unwrap().is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(*calls.lock().unwrap(), vec!["file-picker-2".to_owned()]);
        assert_next_event(
            host,
            HostEvent::FilePickerDismissed {
                file_picker_id: "file-picker-2".to_owned(),
            },
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn permission_event_drains_through_bridge_json() {
        let mut handle = HostHandle::new();
        let event = HostEvent::PermissionRequested {
            permission: "geolocation".to_owned(),
            origin: "http://127.0.0.1:3000".to_owned(),
        };
        handle.events.push_back(event.clone());
        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);

        assert_next_event(host, event);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn navigation_request_event_drains_through_bridge_json() {
        let mut handle = HostHandle::new();
        let event = HostEvent::NavigationRequested {
            navigation_id: "navigation-1".to_owned(),
            url: "https://example.com/blocked".to_owned(),
        };
        handle.events.push_back(event.clone());
        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);

        assert_next_event(host, event);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn popup_request_event_drains_through_bridge_json() {
        let mut handle = HostHandle::new();
        let event = HostEvent::PopupRequested {
            request_id: "request-1".to_owned(),
            parent_webview_id: "parent-1".to_owned(),
            parent_url: Some("https://parent.test/".to_owned()),
            target_url: None,
            window_features: None,
            policy: PopupRequestPolicy::DefaultDeny,
        };
        handle.events.push_back(event.clone());
        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);

        assert_next_event(host, event);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn popup_created_event_drains_through_bridge_json() {
        let mut handle = HostHandle::new();
        let event = HostEvent::PopupCreated {
            request_id: "request-1".to_owned(),
            parent_webview_id: "parent-1".to_owned(),
            child_webview_id: "child-1".to_owned(),
            parent_url: Some("https://parent.test/".to_owned()),
            target_url: None,
            window_features: None,
            policy: PopupRequestPolicy::ManagedChild,
        };
        handle.events.push_back(event.clone());
        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);

        assert_next_event(host, event);

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn navigation_request_resolution_is_queued_until_perform_updates() {
        let calls = Arc::new(Mutex::new(Vec::<(String, bool)>::new()));

        struct NavigationBackend {
            calls: Arc<Mutex<Vec<(String, bool)>>>,
        }

        impl AndroidRenderBackend for NavigationBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resolve_navigation_request(
                &mut self,
                navigation_id: &str,
                allow: bool,
            ) -> Result<Vec<HostEvent>, String> {
                self.calls
                    .lock()
                    .unwrap()
                    .push((navigation_id.to_owned(), allow));
                Ok(vec![HostEvent::NavigationRequested {
                    navigation_id: navigation_id.to_owned(),
                    url: if allow {
                        "https://example.com/allowed".to_owned()
                    } else {
                        "https://example.com/denied".to_owned()
                    },
                }])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(NavigationBackend {
            calls: calls.clone(),
        }));
        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let navigation_id = cstring("navigation-2");

        assert_eq!(
            unsafe { servo_host_resolve_navigation_request(host, navigation_id.as_ptr(), false) },
            ServoStatus::Ok
        );
        assert!(calls.lock().unwrap().is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(
            *calls.lock().unwrap(),
            vec![("navigation-2".to_owned(), false)]
        );
        assert_next_event(
            host,
            HostEvent::NavigationRequested {
                navigation_id: "navigation-2".to_owned(),
                url: "https://example.com/denied".to_owned(),
            },
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn pending_permission_request_accessors_and_resolution_delegate_to_backend() {
        let calls = Arc::new(Mutex::new(Vec::<bool>::new()));

        struct PermissionBackend {
            calls: Arc<Mutex<Vec<bool>>>,
        }

        impl AndroidRenderBackend for PermissionBackend {
            fn attach_surface(
                &mut self,
                _: NativeWindowHandle,
                _: SurfaceSize,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn resize_surface(&mut self, _: SurfaceSize, _: f32) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn detach_surface(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn load_url(&mut self, _: &str) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn reload(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_back(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn go_forward(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn focus(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn blur(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn dispatch_touch_event(
                &mut self,
                _: TouchEventKind,
                _: i32,
                _: f32,
                _: f32,
            ) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn has_pending_permission_request(&self) -> bool {
                true
            }

            fn pending_permission_request(&self) -> Option<(String, String)> {
                Some(("geolocation".to_owned(), "http://127.0.0.1:3000".to_owned()))
            }

            fn resolve_permission(&mut self, allow: bool) -> Result<Vec<HostEvent>, String> {
                self.calls.lock().unwrap().push(allow);
                Ok(vec![HostEvent::PermissionRequested {
                    permission: if allow { "allowed" } else { "denied" }.to_owned(),
                    origin: "http://127.0.0.1:3000".to_owned(),
                }])
            }

            fn perform_updates(&mut self) -> Result<Vec<HostEvent>, String> {
                Ok(vec![])
            }

            fn shutdown(&mut self) {}
        }

        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(PermissionBackend {
            calls: calls.clone(),
        }));
        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);

        assert!(unsafe { servo_host_has_pending_permission_request(host) });
        assert_eq!(
            copied_string(
                unsafe { servo_host_pending_permission_request_permission_len(host) },
                |output, capacity| unsafe {
                    servo_host_pending_permission_request_permission_copy(host, output, capacity)
                }
            ),
            "geolocation"
        );
        assert_eq!(
            copied_string(
                unsafe { servo_host_pending_permission_request_origin_len(host) },
                |output, capacity| unsafe {
                    servo_host_pending_permission_request_origin_copy(host, output, capacity)
                }
            ),
            "http://127.0.0.1:3000"
        );

        assert_eq!(
            unsafe { servo_host_resolve_permission(host, true) },
            ServoStatus::Ok
        );
        assert!(calls.lock().unwrap().is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(*calls.lock().unwrap(), vec![true]);
        assert_next_event(
            host,
            HostEvent::PermissionRequested {
                permission: "allowed".to_owned(),
                origin: "http://127.0.0.1:3000".to_owned(),
            },
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn expired_controller_handle_status_is_preserved_for_token_commands() {
        let host = servo_host_new(0);
        let token = servo_host_controller_handle_token(host).unwrap();

        unsafe { servo_host_free(host) };

        let token = cstring(&token);
        assert_eq!(
            unsafe { servo_host_reload_with_token_ffi(token.as_ptr()) },
            ServoStatus::ExpiredControllerHandle
        );
    }

    #[test]
    fn invalid_controller_handle_status_is_preserved_for_token_commands() {
        let token = cstring("not-a-handle");
        assert_eq!(
            unsafe { servo_host_reload_with_token_ffi(token.as_ptr()) },
            ServoStatus::InvalidControllerHandle
        );
    }

    #[test]
    fn blank_controller_handle_status_is_preserved_for_token_commands() {
        let token = cstring("   ");
        assert_eq!(
            unsafe { servo_host_reload_with_token_ffi(token.as_ptr()) },
            ServoStatus::InvalidControllerHandle
        );
    }
}
