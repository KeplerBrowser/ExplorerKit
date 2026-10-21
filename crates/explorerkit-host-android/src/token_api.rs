use crate::commands::queue_control_command;
use crate::registry::{host_handle_token, with_token_host_mut};
use crate::state::{ControlCommand, HostHandle};
use crate::ServoStatus;
use explorerkit_embedder::{ContextMenuAction, NavigationRequest, WebViewCommand};
pub use explorerkit_embedder::{ControllerCommand, ControllerCommandError};
use std::ffi::{c_char, CStr};

fn into_control_command(command: ControllerCommand) -> ControlCommand {
    match command {
        ControllerCommand::LoadUrl(request) => {
            ControlCommand::WebView(WebViewCommand::LoadUrl(request))
        }
        ControllerCommand::Reload => ControlCommand::WebView(WebViewCommand::Reload),
        ControllerCommand::StopLoading => ControlCommand::WebView(WebViewCommand::StopLoading),
        ControllerCommand::GoBack => ControlCommand::WebView(WebViewCommand::GoBack),
        ControllerCommand::GoForward => ControlCommand::WebView(WebViewCommand::GoForward),
        ControllerCommand::Focus => ControlCommand::WebView(WebViewCommand::Focus),
        ControllerCommand::Blur => ControlCommand::WebView(WebViewCommand::Blur),
        ControllerCommand::EvaluateJavaScript {
            evaluation_id,
            script,
        } => ControlCommand::EvaluateJavaScript {
            evaluation_id,
            script,
        },
        ControllerCommand::ResolveNavigationRequest {
            navigation_id,
            allow,
        } => ControlCommand::ResolveNavigationRequest {
            navigation_id,
            allow,
        },
        ControllerCommand::ResolveSimpleDialog {
            dialog_id,
            confirmed,
            prompt_value,
        } => ControlCommand::ResolveSimpleDialog {
            dialog_id,
            confirmed,
            prompt_value,
        },
        ControllerCommand::ResolveContextMenu {
            context_menu_id,
            action,
        } => ControlCommand::ResolveContextMenu {
            context_menu_id,
            action,
        },
        ControllerCommand::DismissContextMenu { context_menu_id } => {
            ControlCommand::DismissContextMenu { context_menu_id }
        }
    }
}

fn controller_error_from_token_message(message: String) -> ControllerCommandError {
    match message.as_str() {
        "controller handle must not be empty" | "controller handle is invalid" => {
            ControllerCommandError::InvalidControllerHandle
        }
        "controller handle is no longer valid" => ControllerCommandError::ExpiredControllerHandle,
        _ => ControllerCommandError::Backend(message),
    }
}

fn controller_error_status(error: &ControllerCommandError) -> ServoStatus {
    match error {
        ControllerCommandError::InvalidControllerHandle => ServoStatus::InvalidControllerHandle,
        ControllerCommandError::ExpiredControllerHandle => ServoStatus::ExpiredControllerHandle,
        ControllerCommandError::InvalidUrl(_) => ServoStatus::InvalidUrl,
        ControllerCommandError::InvalidEnvelope(_) | ControllerCommandError::Backend(_) => {
            ServoStatus::BackendError
        }
    }
}

/// Returns the opaque controller token for a live host.
///
/// The token is the JS-facing browser-control identity for a `HostHandle`. It remains valid
/// until the host is freed from the live-host registry; surface attach or detach does not
/// change token validity.
pub fn servo_host_controller_handle_token(host: *mut HostHandle) -> Option<String> {
    host_handle_token(host)
}

pub fn servo_host_send_controller_command_with_token(
    token: &str,
    command: ControllerCommand,
) -> Result<(), ControllerCommandError> {
    command.validate()?;

    with_token_host_mut(token, |handle| {
        if matches!(command, ControllerCommand::StopLoading) {
            return Err(ControllerCommandError::Backend(
                "native stop loading is unsupported by Servo".into(),
            ));
        }
        queue_control_command(handle, into_control_command(command));
        Ok(())
    })
    .map_err(controller_error_from_token_message)?
}

pub fn servo_host_send_controller_command_json_with_token(
    token: &str,
    command_json: &str,
) -> Result<(), ControllerCommandError> {
    let command = ControllerCommand::from_json(command_json)?;
    servo_host_send_controller_command_with_token(token, command)
}

/// Queues navigation for a live controller token.
///
/// If the host has no attached render surface yet, the normalized URL remains pending in Rust
/// state and is applied after a render handle is attached.
pub fn servo_host_load_url_with_token(token: &str, url: &str) -> Result<(), String> {
    servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::LoadUrl(NavigationRequest::new(url).map_err(|error| error.to_string())?),
    )
    .map_err(|error| error.to_string())
}

pub fn servo_host_reload_with_token(token: &str) -> Result<(), String> {
    servo_host_send_controller_command_with_token(token, ControllerCommand::Reload)
        .map_err(|error| error.to_string())
}

pub fn servo_host_go_back_with_token(token: &str) -> Result<(), String> {
    servo_host_send_controller_command_with_token(token, ControllerCommand::GoBack)
        .map_err(|error| error.to_string())
}

pub fn servo_host_go_forward_with_token(token: &str) -> Result<(), String> {
    servo_host_send_controller_command_with_token(token, ControllerCommand::GoForward)
        .map_err(|error| error.to_string())
}

pub fn servo_host_focus_with_token(token: &str) -> Result<(), String> {
    servo_host_send_controller_command_with_token(token, ControllerCommand::Focus)
        .map_err(|error| error.to_string())
}

pub fn servo_host_blur_with_token(token: &str) -> Result<(), String> {
    servo_host_send_controller_command_with_token(token, ControllerCommand::Blur)
        .map_err(|error| error.to_string())
}

pub fn servo_host_resolve_simple_dialog_with_token(
    token: &str,
    dialog_id: &str,
    confirmed: bool,
    prompt_value: Option<&str>,
) -> Result<(), String> {
    servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::ResolveSimpleDialog {
            dialog_id: dialog_id.to_owned(),
            confirmed,
            prompt_value: prompt_value.map(str::to_owned),
        },
    )
    .map_err(|error| error.to_string())
}

pub fn servo_host_resolve_navigation_request_with_token(
    token: &str,
    navigation_id: &str,
    allow: bool,
) -> Result<(), String> {
    servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::ResolveNavigationRequest {
            navigation_id: navigation_id.to_owned(),
            allow,
        },
    )
    .map_err(|error| error.to_string())
}

pub fn servo_host_resolve_context_menu_with_token(
    token: &str,
    context_menu_id: &str,
    action: ContextMenuAction,
) -> Result<(), String> {
    servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::ResolveContextMenu {
            context_menu_id: context_menu_id.to_owned(),
            action,
        },
    )
    .map_err(|error| error.to_string())
}

pub fn servo_host_dismiss_context_menu_with_token(
    token: &str,
    context_menu_id: &str,
) -> Result<(), String> {
    servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::DismissContextMenu {
            context_menu_id: context_menu_id.to_owned(),
        },
    )
    .map_err(|error| error.to_string())
}

fn status_from_controller_result(result: Result<(), ControllerCommandError>) -> ServoStatus {
    match result {
        Ok(()) => ServoStatus::Ok,
        Err(error) => controller_error_status(&error),
    }
}

/// # Safety
///
/// A non-null `value` must point to a live NUL-terminated byte string for the returned borrow.
unsafe fn cstr_arg_with_status<'a>(
    value: *const c_char,
    invalid_utf8_status: ServoStatus,
) -> Result<&'a str, ServoStatus> {
    if value.is_null() {
        return Err(ServoStatus::NullPointer);
    }

    CStr::from_ptr(value)
        .to_str()
        .map_err(|_| invalid_utf8_status)
}

/// # Safety
///
/// A non-null `value` must point to a live NUL-terminated byte string for the returned borrow.
unsafe fn optional_cstr_arg_with_status<'a>(
    value: *const c_char,
    invalid_utf8_status: ServoStatus,
) -> Result<Option<&'a str>, ServoStatus> {
    if value.is_null() {
        return Ok(None);
    }

    CStr::from_ptr(value)
        .to_str()
        .map(Some)
        .map_err(|_| invalid_utf8_status)
}

#[no_mangle]
/// Executes a token-scoped controller command envelope from another native library.
///
/// This is the generic C-callable transport for mounted React Native view commands and future
/// native adapters. The JSON envelope is Rust-owned and uses an opaque controller token.
///
/// # Safety
/// `token` and `command_json` must point to valid, NUL-terminated UTF-8 strings.
pub unsafe extern "C" fn servo_host_send_controller_command_with_token_ffi(
    token: *const c_char,
    command_json: *const c_char,
) -> ServoStatus {
    let token = match unsafe { cstr_arg_with_status(token, ServoStatus::InvalidControllerHandle) } {
        Ok(token) => token,
        Err(status) => return status,
    };
    let command_json =
        match unsafe { cstr_arg_with_status(command_json, ServoStatus::BackendError) } {
            Ok(command_json) => command_json,
            Err(status) => return status,
        };

    status_from_controller_result(servo_host_send_controller_command_json_with_token(
        token,
        command_json,
    ))
}

#[no_mangle]
/// Executes a token-scoped load-url command from another native library.
///
/// # Safety
/// `token` and `url` must point to valid, NUL-terminated UTF-8 strings.
pub unsafe extern "C" fn servo_host_load_url_with_token_ffi(
    token: *const c_char,
    url: *const c_char,
) -> ServoStatus {
    let token = match unsafe { cstr_arg_with_status(token, ServoStatus::InvalidControllerHandle) } {
        Ok(token) => token,
        Err(status) => return status,
    };
    let url = match unsafe { cstr_arg_with_status(url, ServoStatus::InvalidUrl) } {
        Ok(url) => url,
        Err(status) => return status,
    };

    let request = match NavigationRequest::new(url) {
        Ok(request) => request,
        Err(error) => {
            return controller_error_status(&ControllerCommandError::InvalidUrl(error.to_string()));
        }
    };

    status_from_controller_result(servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::LoadUrl(request),
    ))
}

#[no_mangle]
/// Executes a token-scoped reload command from another native library.
///
/// # Safety
/// `token` must point to a valid, NUL-terminated UTF-8 string.
pub unsafe extern "C" fn servo_host_reload_with_token_ffi(token: *const c_char) -> ServoStatus {
    let token = match unsafe { cstr_arg_with_status(token, ServoStatus::InvalidControllerHandle) } {
        Ok(token) => token,
        Err(status) => return status,
    };

    status_from_controller_result(servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::Reload,
    ))
}

#[no_mangle]
/// Executes a token-scoped go-back command from another native library.
///
/// # Safety
/// `token` must point to a valid, NUL-terminated UTF-8 string.
pub unsafe extern "C" fn servo_host_go_back_with_token_ffi(token: *const c_char) -> ServoStatus {
    let token = match unsafe { cstr_arg_with_status(token, ServoStatus::InvalidControllerHandle) } {
        Ok(token) => token,
        Err(status) => return status,
    };

    status_from_controller_result(servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::GoBack,
    ))
}

#[no_mangle]
/// Executes a token-scoped go-forward command from another native library.
///
/// # Safety
/// `token` must point to a valid, NUL-terminated UTF-8 string.
pub unsafe extern "C" fn servo_host_go_forward_with_token_ffi(token: *const c_char) -> ServoStatus {
    let token = match unsafe { cstr_arg_with_status(token, ServoStatus::InvalidControllerHandle) } {
        Ok(token) => token,
        Err(status) => return status,
    };

    status_from_controller_result(servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::GoForward,
    ))
}

#[no_mangle]
/// Executes a token-scoped focus command from another native library.
///
/// # Safety
/// `token` must point to a valid, NUL-terminated UTF-8 string.
pub unsafe extern "C" fn servo_host_focus_with_token_ffi(token: *const c_char) -> ServoStatus {
    let token = match unsafe { cstr_arg_with_status(token, ServoStatus::InvalidControllerHandle) } {
        Ok(token) => token,
        Err(status) => return status,
    };

    status_from_controller_result(servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::Focus,
    ))
}

#[no_mangle]
/// Executes a token-scoped blur command from another native library.
///
/// # Safety
/// `token` must point to a valid, NUL-terminated UTF-8 string.
pub unsafe extern "C" fn servo_host_blur_with_token_ffi(token: *const c_char) -> ServoStatus {
    let token = match unsafe { cstr_arg_with_status(token, ServoStatus::InvalidControllerHandle) } {
        Ok(token) => token,
        Err(status) => return status,
    };

    status_from_controller_result(servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::Blur,
    ))
}

#[no_mangle]
/// Queues a token-scoped simple-dialog resolution so it runs on the host-owned update loop.
///
/// # Safety
/// `token` and `dialog_id` must point to valid, NUL-terminated UTF-8 strings. `prompt_value`
/// may be null.
pub unsafe extern "C" fn servo_host_resolve_simple_dialog_with_token_ffi(
    token: *const c_char,
    dialog_id: *const c_char,
    confirmed: bool,
    prompt_value: *const c_char,
) -> ServoStatus {
    let token = match unsafe { cstr_arg_with_status(token, ServoStatus::InvalidControllerHandle) } {
        Ok(token) => token,
        Err(status) => return status,
    };
    let dialog_id = match unsafe { cstr_arg_with_status(dialog_id, ServoStatus::BackendError) } {
        Ok(dialog_id) => dialog_id,
        Err(status) => return status,
    };
    let prompt_value =
        match unsafe { optional_cstr_arg_with_status(prompt_value, ServoStatus::BackendError) } {
            Ok(prompt_value) => prompt_value,
            Err(status) => return status,
        };

    status_from_controller_result(servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::ResolveSimpleDialog {
            dialog_id: dialog_id.to_owned(),
            confirmed,
            prompt_value: prompt_value.map(str::to_owned),
        },
    ))
}

#[no_mangle]
/// Queues a token-scoped navigation-request resolution so it runs on the host-owned update loop.
///
/// # Safety
/// `token` and `navigation_id` must point to valid, NUL-terminated UTF-8 strings.
pub unsafe extern "C" fn servo_host_resolve_navigation_request_with_token_ffi(
    token: *const c_char,
    navigation_id: *const c_char,
    allow: bool,
) -> ServoStatus {
    let token = match unsafe { cstr_arg_with_status(token, ServoStatus::InvalidControllerHandle) } {
        Ok(token) => token,
        Err(status) => return status,
    };
    let navigation_id =
        match unsafe { cstr_arg_with_status(navigation_id, ServoStatus::BackendError) } {
            Ok(navigation_id) => navigation_id,
            Err(status) => return status,
        };

    status_from_controller_result(servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::ResolveNavigationRequest {
            navigation_id: navigation_id.to_owned(),
            allow,
        },
    ))
}

#[no_mangle]
/// Queues a token-scoped context-menu resolution so it runs on the host-owned update loop.
///
/// # Safety
/// `token`, `context_menu_id`, and `action` must point to valid, NUL-terminated UTF-8 strings.
pub unsafe extern "C" fn servo_host_resolve_context_menu_with_token_ffi(
    token: *const c_char,
    context_menu_id: *const c_char,
    action: *const c_char,
) -> ServoStatus {
    let token = match unsafe { cstr_arg_with_status(token, ServoStatus::InvalidControllerHandle) } {
        Ok(token) => token,
        Err(status) => return status,
    };
    let context_menu_id =
        match unsafe { cstr_arg_with_status(context_menu_id, ServoStatus::BackendError) } {
            Ok(context_menu_id) => context_menu_id,
            Err(status) => return status,
        };
    let action = match unsafe { cstr_arg_with_status(action, ServoStatus::BackendError) } {
        Ok(action) => action,
        Err(status) => return status,
    };
    let Some(action) = ContextMenuAction::from_str(action) else {
        return ServoStatus::BackendError;
    };

    status_from_controller_result(servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::ResolveContextMenu {
            context_menu_id: context_menu_id.to_owned(),
            action,
        },
    ))
}

#[no_mangle]
/// Queues a token-scoped context-menu dismissal so it runs on the host-owned update loop.
///
/// # Safety
/// `token` and `context_menu_id` must point to valid, NUL-terminated UTF-8 strings.
pub unsafe extern "C" fn servo_host_dismiss_context_menu_with_token_ffi(
    token: *const c_char,
    context_menu_id: *const c_char,
) -> ServoStatus {
    let token = match unsafe { cstr_arg_with_status(token, ServoStatus::InvalidControllerHandle) } {
        Ok(token) => token,
        Err(status) => return status,
    };
    let context_menu_id =
        match unsafe { cstr_arg_with_status(context_menu_id, ServoStatus::BackendError) } {
            Ok(context_menu_id) => context_menu_id,
            Err(status) => return status,
        };

    status_from_controller_result(servo_host_send_controller_command_with_token(
        token,
        ControllerCommand::DismissContextMenu {
            context_menu_id: context_menu_id.to_owned(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::register_live_host;
    use crate::{servo_host_free, servo_host_new, servo_host_perform_updates};
    use crate::{AndroidRenderBackend, NativeWindowHandle, SurfaceSize};
    use explorerkit_embedder::{HostEvent, TouchEventKind};
    use std::ffi::CString;
    use std::sync::{Arc, Mutex};

    type DialogCalls = Arc<Mutex<Vec<(String, bool, Option<String>)>>>;

    struct ControlResponseBackend {
        navigation_calls: Arc<Mutex<Vec<(String, bool)>>>,
        dialog_calls: DialogCalls,
    }

    impl AndroidRenderBackend for ControlResponseBackend {
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
            self.navigation_calls
                .lock()
                .expect("navigation-policy log should be available")
                .push((navigation_id.to_owned(), allow));
            Ok(vec![])
        }

        fn resolve_simple_dialog(
            &mut self,
            dialog_id: &str,
            confirmed: bool,
            prompt_value: Option<&str>,
        ) -> Result<Vec<HostEvent>, String> {
            self.dialog_calls
                .lock()
                .expect("dialog log should be available")
                .push((
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

    fn cstring(value: &str) -> CString {
        CString::new(value).expect("test strings should not contain interior NULs")
    }

    #[test]
    fn stop_loading_rejects_before_queueing_and_preserves_token_errors() {
        let host = servo_host_new(0);
        let token = servo_host_controller_handle_token(host).unwrap();
        let stop = || ControllerCommand::StopLoading;
        assert_eq!(
            servo_host_send_controller_command_with_token("bad", stop()),
            Err(ControllerCommandError::InvalidControllerHandle)
        );
        let error = servo_host_send_controller_command_with_token(&token, stop()).unwrap_err();
        assert!(matches!(error, ControllerCommandError::Backend(_)));
        assert_eq!(controller_error_status(&error), ServoStatus::BackendError);
        with_token_host_mut(&token, |handle| {
            assert!(handle.pending_commands.is_empty());
            assert!(handle
                .dispatch_webview_command_runtime(WebViewCommand::StopLoading)
                .is_err());
            assert!(handle.pending_commands.is_empty());
        })
        .unwrap();
        let token_c = cstring(&token);
        let command_c = cstring(r#"{"version":1,"command":"stopLoading"}"#);
        assert_eq!(
            unsafe {
                servo_host_send_controller_command_with_token_ffi(
                    token_c.as_ptr(),
                    command_c.as_ptr(),
                )
            },
            ServoStatus::BackendError
        );
        unsafe { servo_host_free(host) };
        assert_eq!(
            servo_host_send_controller_command_with_token(&token, stop()),
            Err(ControllerCommandError::ExpiredControllerHandle)
        );
    }

    #[test]
    fn typed_controller_command_transport_validates_request_ids() {
        let host = servo_host_new(0);
        let token = servo_host_controller_handle_token(host).expect("host token should exist");

        assert_eq!(
            servo_host_send_controller_command_with_token(
                &token,
                ControllerCommand::ResolveNavigationRequest {
                    navigation_id: "   ".to_owned(),
                    allow: true,
                },
            ),
            Err(ControllerCommandError::InvalidEnvelope(
                "navigationId must not be empty".to_owned(),
            ))
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn generic_controller_command_transport_queues_commands_for_updates() {
        let calls = Arc::new(Mutex::new(Vec::<(String, bool)>::new()));
        let dialog_calls = Arc::new(Mutex::new(Vec::<(String, bool, Option<String>)>::new()));
        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(ControlResponseBackend {
            navigation_calls: calls.clone(),
            dialog_calls,
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let token = servo_host_controller_handle_token(host).expect("host token should exist");

        servo_host_send_controller_command_json_with_token(
            &token,
            r#"{"command":"resolveNavigationRequest","navigationId":"navigation-1","allow":false}"#,
        )
        .expect("controller command should queue");
        assert!(calls
            .lock()
            .expect("navigation-policy log should be readable")
            .is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(
            *calls
                .lock()
                .expect("navigation-policy log should be readable"),
            vec![("navigation-1".to_owned(), false)]
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn generic_controller_command_transport_dispatches_dialog_responses_for_updates() {
        let navigation_calls = Arc::new(Mutex::new(Vec::<(String, bool)>::new()));
        let dialog_calls = Arc::new(Mutex::new(Vec::<(String, bool, Option<String>)>::new()));
        let mut handle = HostHandle::new();
        handle.surface = Some(SurfaceSize::new(640, 480));
        handle.native_window = Some(0xCAFEusize as NativeWindowHandle);
        handle.set_android_backend_for_tests(Box::new(ControlResponseBackend {
            navigation_calls,
            dialog_calls: dialog_calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let token = servo_host_controller_handle_token(host).expect("host token should exist");

        servo_host_send_controller_command_json_with_token(
            &token,
            r#"{"version":1,"command":"resolveSimpleDialog","dialogId":"dialog-1","confirmed":true,"promptValue":"Servo"}"#,
        )
        .expect("controller command should queue");
        assert!(dialog_calls
            .lock()
            .expect("dialog log should be readable")
            .is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(
            *dialog_calls.lock().expect("dialog log should be readable"),
            vec![("dialog-1".to_owned(), true, Some("Servo".to_owned()))]
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn evaluate_javascript_commands_queue_and_dispatch_through_the_android_backend() {
        let calls = Arc::new(Mutex::new(Vec::<(String, String)>::new()));

        struct JavaScriptEvaluationBackend {
            calls: Arc<Mutex<Vec<(String, String)>>>,
        }

        impl AndroidRenderBackend for JavaScriptEvaluationBackend {
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

            fn evaluate_javascript(
                &mut self,
                evaluation_id: &str,
                script: &str,
            ) -> Result<Vec<HostEvent>, String> {
                self.calls
                    .lock()
                    .unwrap()
                    .push((evaluation_id.to_owned(), script.to_owned()));
                Ok(vec![HostEvent::JavaScriptEvaluationResult {
                    evaluation_id: evaluation_id.to_owned(),
                    ok: true,
                    value_json: Some(r#"{"type":"number","value":2}"#.to_owned()),
                    error_type: None,
                }])
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
        handle.set_android_backend_for_tests(Box::new(JavaScriptEvaluationBackend {
            calls: calls.clone(),
        }));

        let host = Box::into_raw(Box::new(handle));
        register_live_host(host);
        let token = servo_host_controller_handle_token(host).expect("host token should exist");

        servo_host_send_controller_command_json_with_token(
            &token,
            r#"{"command":"evaluateJavaScript","evaluationId":"evaluation-1","script":"1 + 1"}"#,
        )
        .expect("controller command should queue");
        assert!(calls.lock().unwrap().is_empty());

        unsafe { servo_host_perform_updates(host) };

        assert_eq!(
            *calls.lock().unwrap(),
            vec![("evaluation-1".to_owned(), "1 + 1".to_owned())]
        );
        assert_eq!(
            unsafe { &mut *host }.events.pop_front(),
            Some(HostEvent::JavaScriptEvaluationResult {
                evaluation_id: "evaluation-1".to_owned(),
                ok: true,
                value_json: Some(r#"{"type":"number","value":2}"#.to_owned()),
                error_type: None,
            })
        );

        unsafe { servo_host_free(host) };
    }

    #[test]
    fn generic_controller_command_ffi_maps_statuses() {
        let host = servo_host_new(0);
        let token = servo_host_controller_handle_token(host).expect("host token should exist");
        let token = cstring(&token);
        let valid_command = cstring(r#"{"command":"reload"}"#);
        let invalid_url_command = cstring(r#"{"command":"loadUrl","url":"https:///"}"#);
        let malformed_command = cstring(r#"{"command":"unknown"}"#);

        assert_eq!(
            unsafe {
                servo_host_send_controller_command_with_token_ffi(
                    cstring("invalid").as_ptr(),
                    valid_command.as_ptr(),
                )
            },
            ServoStatus::InvalidControllerHandle
        );
        assert_eq!(
            unsafe {
                servo_host_send_controller_command_with_token_ffi(
                    token.as_ptr(),
                    invalid_url_command.as_ptr(),
                )
            },
            ServoStatus::InvalidUrl
        );
        assert_eq!(
            unsafe {
                servo_host_send_controller_command_with_token_ffi(
                    token.as_ptr(),
                    malformed_command.as_ptr(),
                )
            },
            ServoStatus::BackendError
        );

        let expired_token = token.clone();
        unsafe { servo_host_free(host) };

        assert_eq!(
            unsafe {
                servo_host_send_controller_command_with_token_ffi(
                    expired_token.as_ptr(),
                    valid_command.as_ptr(),
                )
            },
            ServoStatus::ExpiredControllerHandle
        );
    }
}
