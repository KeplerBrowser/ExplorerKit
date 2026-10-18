use crate::{
    ContextMenuElementInformation, ContextMenuItem, FaviconImage, FaviconPixelFormat, HostEvent,
    SelectElementOption, SelectElementOptionOrOptgroup,
};
use std::fmt::Write;

pub fn encode_host_event_bridge(event: &HostEvent) -> String {
    let mut output = String::new();
    output.push('{');

    let mut first = true;
    write_string_field(&mut output, &mut first, "name", event_name(event));
    write_value_field(&mut output, &mut first, "payload", |output| {
        write_payload(output, event);
    });

    output.push('}');
    output
}

fn event_name(event: &HostEvent) -> &'static str {
    match event {
        HostEvent::NavigationRequested { .. } => "navigationRequested",
        HostEvent::PopupRequested { .. } => "popupRequested",
        HostEvent::PopupCreated { .. } => "popupCreated",
        HostEvent::PopupCreationFailed { .. } => "popupCreationFailed",
        HostEvent::UrlChanged { .. } => "urlChanged",
        HostEvent::PageTitleChanged { .. } => "pageTitleChanged",
        HostEvent::FaviconChanged { .. } => "faviconChanged",
        HostEvent::StatusTextChanged { .. } => "statusTextChanged",
        HostEvent::LoadStatusChanged { .. } => "loadStatusChanged",
        HostEvent::HistoryChanged { .. } => "historyChanged",
        HostEvent::NavigationStateChanged { .. } => "navigationStateChanged",
        HostEvent::Closed => "closed",
        HostEvent::Crashed { .. } => "crashed",
        HostEvent::Error { .. } => "error",
        HostEvent::JavaScriptEvaluationResult { .. } => "javascriptEvaluationResult",
        HostEvent::SimpleDialogRequested { .. } => "simpleDialogRequested",
        HostEvent::SimpleDialogDismissed { .. } => "simpleDialogDismissed",
        HostEvent::InputMethodRequested { .. } => "inputMethodRequested",
        HostEvent::InputMethodDismissed { .. } => "inputMethodDismissed",
        HostEvent::SelectElementRequested { .. } => "selectElementRequested",
        HostEvent::SelectElementDismissed { .. } => "selectElementDismissed",
        HostEvent::ContextMenuRequested { .. } => "contextMenuRequested",
        HostEvent::ContextMenuDismissed { .. } => "contextMenuDismissed",
        HostEvent::FilePickerRequested { .. } => "filePickerRequested",
        HostEvent::FilePickerDismissed { .. } => "filePickerDismissed",
        HostEvent::PermissionRequested { .. } => "permissionRequested",
        HostEvent::FocusChanged { .. } => "focusChanged",
        HostEvent::CursorChanged { .. } => "cursorChanged",
        HostEvent::FullscreenChanged { .. } => "fullscreenChanged",
        HostEvent::SurfaceAttached { .. } => "surfaceAttached",
        HostEvent::SurfaceResized { .. } => "surfaceResized",
        HostEvent::SurfaceDetached => "surfaceDetached",
    }
}

fn write_payload(output: &mut String, event: &HostEvent) {
    output.push('{');
    let mut first = true;

    match event {
        HostEvent::NavigationRequested { navigation_id, url } => {
            write_string_field(output, &mut first, "navigationId", navigation_id);
            write_string_field(output, &mut first, "url", url);
        }
        HostEvent::PopupRequested {
            request_id,
            parent_webview_id,
            parent_url,
            target_url,
            window_features,
            wants_popup,
            policy,
        } => {
            write_string_field(output, &mut first, "requestId", request_id);
            write_string_field(output, &mut first, "parentWebViewId", parent_webview_id);
            write_optional_string_field(output, &mut first, "parentUrl", parent_url.as_deref());
            write_optional_string_field(output, &mut first, "targetUrl", target_url.as_deref());
            write_optional_string_field(
                output,
                &mut first,
                "windowFeatures",
                window_features.as_deref(),
            );
            write_optional_bool_field(output, &mut first, "wantsPopup", *wants_popup);
            write_string_field(output, &mut first, "policy", policy.as_str());
        }
        HostEvent::PopupCreated {
            request_id,
            parent_webview_id,
            child_webview_id,
            parent_url,
            target_url,
            window_features,
            wants_popup,
            policy,
        } => {
            write_string_field(output, &mut first, "requestId", request_id);
            write_string_field(output, &mut first, "parentWebViewId", parent_webview_id);
            write_string_field(output, &mut first, "childWebViewId", child_webview_id);
            write_optional_string_field(output, &mut first, "parentUrl", parent_url.as_deref());
            write_optional_string_field(output, &mut first, "targetUrl", target_url.as_deref());
            write_optional_string_field(
                output,
                &mut first,
                "windowFeatures",
                window_features.as_deref(),
            );
            write_optional_bool_field(output, &mut first, "wantsPopup", *wants_popup);
            write_string_field(output, &mut first, "policy", policy.as_str());
        }
        HostEvent::PopupCreationFailed {
            request_id,
            error_message,
        } => {
            write_string_field(output, &mut first, "requestId", request_id);
            write_string_field(output, &mut first, "errorMessage", error_message);
        }
        HostEvent::UrlChanged { url } => {
            write_string_field(output, &mut first, "url", url);
        }
        HostEvent::PageTitleChanged { title } => {
            write_optional_string_field(output, &mut first, "title", title.as_deref());
        }
        HostEvent::FaviconChanged { favicon } => {
            write_value_field(output, &mut first, "favicon", |output| {
                write_optional_favicon(output, favicon.as_ref());
            });
        }
        HostEvent::StatusTextChanged { status } => {
            write_optional_string_field(output, &mut first, "status", status.as_deref());
        }
        HostEvent::LoadStatusChanged { status } => {
            write_string_field(output, &mut first, "status", status.as_str());
        }
        HostEvent::HistoryChanged {
            entries,
            current,
            can_go_back,
            can_go_forward,
        } => {
            write_value_field(output, &mut first, "entries", |output| {
                write_string_array(output, entries);
            });
            write_usize_field(output, &mut first, "current", *current);
            write_bool_field(output, &mut first, "canGoBack", *can_go_back);
            write_bool_field(output, &mut first, "canGoForward", *can_go_forward);
        }
        HostEvent::NavigationStateChanged {
            can_go_back,
            can_go_forward,
        } => {
            write_bool_field(output, &mut first, "canGoBack", *can_go_back);
            write_bool_field(output, &mut first, "canGoForward", *can_go_forward);
        }
        HostEvent::Closed => {}
        HostEvent::Crashed {
            url,
            reason,
            backtrace,
        } => {
            write_optional_string_field(output, &mut first, "url", url.as_deref());
            write_string_field(output, &mut first, "reason", reason);
            write_optional_string_field(output, &mut first, "backtrace", backtrace.as_deref());
        }
        HostEvent::Error { url, code, message } => {
            write_optional_string_field(output, &mut first, "url", url.as_deref());
            write_i32_field(output, &mut first, "code", *code);
            write_string_field(output, &mut first, "message", message);
        }
        HostEvent::JavaScriptEvaluationResult {
            evaluation_id,
            ok,
            value_json,
            error_type,
        } => {
            write_string_field(output, &mut first, "evaluationId", evaluation_id);
            write_bool_field(output, &mut first, "ok", *ok);
            write_optional_string_field(output, &mut first, "valueJson", value_json.as_deref());
            write_optional_string_field(
                output,
                &mut first,
                "errorType",
                error_type.map(|error_type| error_type.as_str()),
            );
        }
        HostEvent::SimpleDialogRequested {
            dialog_id,
            kind,
            message,
            default_value,
        } => {
            write_string_field(output, &mut first, "dialogId", dialog_id);
            write_string_field(output, &mut first, "kind", kind.as_str());
            write_string_field(output, &mut first, "message", message);
            write_optional_string_field(
                output,
                &mut first,
                "defaultValue",
                default_value.as_deref(),
            );
        }
        HostEvent::SimpleDialogDismissed { dialog_id } => {
            write_string_field(output, &mut first, "dialogId", dialog_id);
        }
        HostEvent::InputMethodRequested {
            input_method_id,
            input_method_type,
            text,
            insertion_point,
            multiline,
            allow_virtual_keyboard,
        } => {
            write_string_field(output, &mut first, "inputMethodId", input_method_id);
            write_string_field(output, &mut first, "type", input_method_type.as_str());
            write_string_field(output, &mut first, "text", text);
            write_optional_u32_field(output, &mut first, "insertionPoint", *insertion_point);
            write_bool_field(output, &mut first, "multiline", *multiline);
            write_bool_field(
                output,
                &mut first,
                "allowVirtualKeyboard",
                *allow_virtual_keyboard,
            );
        }
        HostEvent::InputMethodDismissed { input_method_id } => {
            write_string_field(output, &mut first, "inputMethodId", input_method_id);
        }
        HostEvent::SelectElementRequested {
            select_element_id,
            options,
            selected_options,
            allow_select_multiple,
        } => {
            write_string_field(output, &mut first, "selectElementId", select_element_id);
            write_value_field(output, &mut first, "options", |output| {
                write_select_element_options(output, options);
            });
            write_value_field(output, &mut first, "selectedOptions", |output| {
                write_usize_array(output, selected_options);
            });
            write_bool_field(
                output,
                &mut first,
                "allowSelectMultiple",
                *allow_select_multiple,
            );
        }
        HostEvent::SelectElementDismissed { select_element_id } => {
            write_string_field(output, &mut first, "selectElementId", select_element_id);
        }
        HostEvent::ContextMenuRequested {
            context_menu_id,
            x,
            y,
            width,
            height,
            element_info,
            items,
        } => {
            write_string_field(output, &mut first, "contextMenuId", context_menu_id);
            write_i32_field(output, &mut first, "x", *x);
            write_i32_field(output, &mut first, "y", *y);
            write_u32_field(output, &mut first, "width", *width);
            write_u32_field(output, &mut first, "height", *height);
            write_value_field(output, &mut first, "elementInfo", |output| {
                write_context_menu_element_info(output, element_info);
            });
            write_value_field(output, &mut first, "items", |output| {
                write_context_menu_items(output, items);
            });
        }
        HostEvent::ContextMenuDismissed { context_menu_id } => {
            write_string_field(output, &mut first, "contextMenuId", context_menu_id);
        }
        HostEvent::FilePickerRequested {
            file_picker_id,
            current_paths,
            filter_patterns,
            allow_select_multiple,
        } => {
            write_string_field(output, &mut first, "filePickerId", file_picker_id);
            write_value_field(output, &mut first, "currentPaths", |output| {
                write_string_array(output, current_paths);
            });
            write_value_field(output, &mut first, "filterPatterns", |output| {
                write_string_array(output, filter_patterns);
            });
            write_bool_field(
                output,
                &mut first,
                "allowSelectMultiple",
                *allow_select_multiple,
            );
        }
        HostEvent::FilePickerDismissed { file_picker_id } => {
            write_string_field(output, &mut first, "filePickerId", file_picker_id);
        }
        HostEvent::PermissionRequested { permission, origin } => {
            write_string_field(output, &mut first, "permission", permission);
            write_string_field(output, &mut first, "origin", origin);
        }
        HostEvent::FocusChanged { is_focused } => {
            write_bool_field(output, &mut first, "isFocused", *is_focused);
        }
        HostEvent::CursorChanged { cursor } => {
            write_string_field(output, &mut first, "cursor", cursor);
        }
        HostEvent::FullscreenChanged { is_fullscreen } => {
            write_bool_field(output, &mut first, "isFullscreen", *is_fullscreen);
        }
        HostEvent::SurfaceAttached { size } | HostEvent::SurfaceResized { size } => {
            write_u32_field(output, &mut first, "width", size.width);
            write_u32_field(output, &mut first, "height", size.height);
        }
        HostEvent::SurfaceDetached => {}
    }

    output.push('}');
}

fn write_optional_favicon(output: &mut String, favicon: Option<&FaviconImage>) {
    let Some(favicon) = favicon else {
        output.push_str("null");
        return;
    };

    output.push('{');
    let mut first = true;
    write_u32_field(output, &mut first, "width", favicon.width);
    write_u32_field(output, &mut first, "height", favicon.height);
    write_string_field(
        output,
        &mut first,
        "format",
        match favicon.format {
            FaviconPixelFormat::K8 => "K8",
            FaviconPixelFormat::KA8 => "KA8",
            FaviconPixelFormat::RGB8 => "RGB8",
            FaviconPixelFormat::RGBA8 => "RGBA8",
            FaviconPixelFormat::BGRA8 => "BGRA8",
        },
    );
    write_value_field(output, &mut first, "bytes", |output| {
        write_u8_array(output, &favicon.bytes);
    });
    output.push('}');
}

fn write_context_menu_element_info(
    output: &mut String,
    element_info: &ContextMenuElementInformation,
) {
    output.push('{');
    let mut first = true;
    write_bool_field(output, &mut first, "isLink", element_info.is_link);
    write_bool_field(output, &mut first, "isImage", element_info.is_image);
    write_bool_field(
        output,
        &mut first,
        "isEditableText",
        element_info.is_editable_text,
    );
    write_bool_field(
        output,
        &mut first,
        "hasSelection",
        element_info.has_selection,
    );
    write_optional_string_field(
        output,
        &mut first,
        "linkUrl",
        element_info.link_url.as_deref(),
    );
    write_optional_string_field(
        output,
        &mut first,
        "imageUrl",
        element_info.image_url.as_deref(),
    );
    write_string_field(
        output,
        &mut first,
        "contextType",
        &element_info.context_type,
    );
    output.push('}');
}

fn write_context_menu_items(output: &mut String, items: &[ContextMenuItem]) {
    output.push('[');
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push('{');
        let mut first = true;
        match item {
            ContextMenuItem::Item {
                label,
                action,
                enabled,
            } => {
                write_string_field(output, &mut first, "type", "item");
                write_string_field(output, &mut first, "label", label);
                write_string_field(output, &mut first, "action", action.as_str());
                write_bool_field(output, &mut first, "enabled", *enabled);
            }
            ContextMenuItem::Separator => {
                write_string_field(output, &mut first, "type", "separator");
            }
        }
        output.push('}');
    }
    output.push(']');
}

fn write_select_element_options(output: &mut String, options: &[SelectElementOptionOrOptgroup]) {
    output.push('[');
    for (index, option_or_optgroup) in options.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push('{');
        let mut first = true;
        match option_or_optgroup {
            SelectElementOptionOrOptgroup::Option(option) => {
                write_string_field(output, &mut first, "type", "option");
                write_select_element_option_fields(output, &mut first, option);
            }
            SelectElementOptionOrOptgroup::Optgroup { label, options } => {
                write_string_field(output, &mut first, "type", "optgroup");
                write_string_field(output, &mut first, "label", label);
                write_value_field(output, &mut first, "options", |output| {
                    write_select_element_option_array(output, options);
                });
            }
        }
        output.push('}');
    }
    output.push(']');
}

fn write_select_element_option_array(output: &mut String, options: &[SelectElementOption]) {
    output.push('[');
    for (index, option) in options.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push('{');
        let mut first = true;
        write_select_element_option_fields(output, &mut first, option);
        output.push('}');
    }
    output.push(']');
}

fn write_select_element_option_fields(
    output: &mut String,
    first: &mut bool,
    option: &SelectElementOption,
) {
    write_usize_field(output, first, "id", option.id);
    write_string_field(output, first, "label", &option.label);
    write_bool_field(output, first, "isDisabled", option.is_disabled);
}

fn write_string_array(output: &mut String, values: &[String]) {
    output.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write_json_string(output, value);
    }
    output.push(']');
}

fn write_usize_array(output: &mut String, values: &[usize]) {
    output.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write!(output, "{value}").expect("writing to String should not fail");
    }
    output.push(']');
}

fn write_u8_array(output: &mut String, values: &[u8]) {
    output.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write!(output, "{value}").expect("writing to String should not fail");
    }
    output.push(']');
}

fn write_string_field(output: &mut String, first: &mut bool, name: &str, value: &str) {
    write_field_name(output, first, name);
    write_json_string(output, value);
}

fn write_optional_string_field(
    output: &mut String,
    first: &mut bool,
    name: &str,
    value: Option<&str>,
) {
    write_field_name(output, first, name);
    match value {
        Some(value) => write_json_string(output, value),
        None => output.push_str("null"),
    }
}

fn write_optional_u32_field(output: &mut String, first: &mut bool, name: &str, value: Option<u32>) {
    write_field_name(output, first, name);
    match value {
        Some(value) => write!(output, "{value}").expect("writing to String should not fail"),
        None => output.push_str("null"),
    }
}

fn write_optional_bool_field(
    output: &mut String,
    first: &mut bool,
    name: &str,
    value: Option<bool>,
) {
    write_field_name(output, first, name);
    output.push_str(match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    });
}

fn write_bool_field(output: &mut String, first: &mut bool, name: &str, value: bool) {
    write_field_name(output, first, name);
    output.push_str(if value { "true" } else { "false" });
}

fn write_i32_field(output: &mut String, first: &mut bool, name: &str, value: i32) {
    write_field_name(output, first, name);
    write!(output, "{value}").expect("writing to String should not fail");
}

fn write_u32_field(output: &mut String, first: &mut bool, name: &str, value: u32) {
    write_field_name(output, first, name);
    write!(output, "{value}").expect("writing to String should not fail");
}

fn write_usize_field(output: &mut String, first: &mut bool, name: &str, value: usize) {
    write_field_name(output, first, name);
    write!(output, "{value}").expect("writing to String should not fail");
}

fn write_value_field(
    output: &mut String,
    first: &mut bool,
    name: &str,
    write_value: impl FnOnce(&mut String),
) {
    write_field_name(output, first, name);
    write_value(output);
}

fn write_field_name(output: &mut String, first: &mut bool, name: &str) {
    if !*first {
        output.push(',');
    }
    *first = false;
    write_json_string(output, name);
    output.push(':');
}

fn write_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character < ' ' => {
                write!(output, "\\u{:04x}", character as u32)
                    .expect("writing to String should not fail");
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContextMenuAction, ContextMenuElementInformation, ContextMenuItem, HostEvent,
        InputMethodKind, JavaScriptEvaluationErrorKind, LoadStatusKind, PopupRequestPolicy,
        SelectElementOption, SelectElementOptionOrOptgroup, SimpleDialogKind, SurfaceSize,
    };

    #[test]
    fn encodes_owned_favicon_payloads() {
        assert_eq!(
            encode_host_event_bridge(&HostEvent::FaviconChanged {
                favicon: Some(FaviconImage {
                    width: 2,
                    height: 1,
                    format: FaviconPixelFormat::BGRA8,
                    bytes: vec![3, 2, 1, 255],
                }),
            }),
            r#"{"name":"faviconChanged","payload":{"favicon":{"width":2,"height":1,"format":"BGRA8","bytes":[3,2,1,255]}}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::FaviconChanged { favicon: None }),
            r#"{"name":"faviconChanged","payload":{"favicon":null}}"#
        );
    }

    #[test]
    fn encodes_context_menu_requested_as_a_bridge_envelope() {
        let event = HostEvent::ContextMenuRequested {
            context_menu_id: "context-menu-1".to_owned(),
            x: 12,
            y: 24,
            width: 32,
            height: 48,
            element_info: ContextMenuElementInformation {
                is_link: true,
                is_image: false,
                is_editable_text: false,
                has_selection: false,
                link_url: Some("https://example.com/".to_owned()),
                image_url: None,
                context_type: "link".to_owned(),
            },
            items: vec![
                ContextMenuItem::Item {
                    label: "Copy link".to_owned(),
                    action: ContextMenuAction::CopyLink,
                    enabled: true,
                },
                ContextMenuItem::Separator,
            ],
        };

        assert_eq!(
            encode_host_event_bridge(&event),
            concat!(
                r#"{"name":"contextMenuRequested","payload":{"#,
                r#""contextMenuId":"context-menu-1","x":12,"y":24,"width":32,"height":48,"#,
                r#""elementInfo":{"isLink":true,"isImage":false,"isEditableText":false,"#,
                r#""hasSelection":false,"linkUrl":"https://example.com/","imageUrl":null,"#,
                r#""contextType":"link"},"items":[{"type":"item","label":"Copy link","#,
                r#""action":"copy-link","enabled":true},{"type":"separator"}]}}"#
            )
        );
    }

    #[test]
    fn encodes_absent_optional_strings_as_json_null() {
        assert_eq!(
            encode_host_event_bridge(&HostEvent::PageTitleChanged { title: None }),
            r#"{"name":"pageTitleChanged","payload":{"title":null}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::Crashed {
                url: None,
                reason: "boom".to_owned(),
                backtrace: None,
            }),
            r#"{"name":"crashed","payload":{"url":null,"reason":"boom","backtrace":null}}"#
        );
    }

    #[test]
    fn escapes_strings_for_json() {
        assert_eq!(
            encode_host_event_bridge(&HostEvent::StatusTextChanged {
                status: Some("quote \" slash \\ line\nnul \0".to_owned()),
            }),
            r#"{"name":"statusTextChanged","payload":{"status":"quote \" slash \\ line\nnul \u0000"}}"#
        );
    }

    #[test]
    fn encodes_nested_select_and_file_picker_payloads() {
        assert_eq!(
            encode_host_event_bridge(&HostEvent::SelectElementRequested {
                select_element_id: "select-1".to_owned(),
                options: vec![
                    SelectElementOptionOrOptgroup::Option(SelectElementOption {
                        id: 1,
                        label: "Servo".to_owned(),
                        is_disabled: false,
                    }),
                    SelectElementOptionOrOptgroup::Optgroup {
                        label: "Engines".to_owned(),
                        options: vec![SelectElementOption {
                            id: 4,
                            label: "Layout".to_owned(),
                            is_disabled: true,
                        }],
                    },
                ],
                selected_options: vec![1, 4],
                allow_select_multiple: true,
            }),
            concat!(
                r#"{"name":"selectElementRequested","payload":{"selectElementId":"select-1","#,
                r#""options":[{"type":"option","id":1,"label":"Servo","isDisabled":false},"#,
                r#"{"type":"optgroup","label":"Engines","options":[{"id":4,"label":"Layout","#,
                r#""isDisabled":true}]}],"selectedOptions":[1,4],"allowSelectMultiple":true}}"#
            )
        );

        assert_eq!(
            encode_host_event_bridge(&HostEvent::FilePickerRequested {
                file_picker_id: "file-picker-1".to_owned(),
                current_paths: vec!["/tmp/one.txt".to_owned()],
                filter_patterns: vec!["txt".to_owned()],
                allow_select_multiple: false,
            }),
            r#"{"name":"filePickerRequested","payload":{"filePickerId":"file-picker-1","currentPaths":["/tmp/one.txt"],"filterPatterns":["txt"],"allowSelectMultiple":false}}"#
        );
    }

    #[test]
    fn popup_receipts_preserve_three_state_engine_preference() {
        let mut state = crate::ServoAdapterState::default();
        for wants_popup in [Some(true), Some(false), None] {
            state.request_popup(crate::PopupRequest {
                request_id: "request-1".into(),
                parent_webview_id: "source".into(),
                parent_url: None,
                target_url: None,
                window_features: None,
                wants_popup,
            });
            state.notify_popup_created(crate::PopupCreated {
                request_id: "request-1".into(),
                parent_webview_id: "source".into(),
                child_webview_id: "child".into(),
                parent_url: None,
                target_url: None,
                window_features: None,
                wants_popup,
            });
            for event in state.drain_events() {
                let json = encode_host_event_bridge(&event);
                let expected = match wants_popup {
                    Some(true) => "true",
                    Some(false) => "false",
                    None => "null",
                };
                assert!(
                    json.contains(&format!("\"wantsPopup\":{expected}")),
                    "{json}"
                );
            }
        }
    }

    #[test]
    fn encodes_remaining_payload_shapes() {
        assert_eq!(
            encode_host_event_bridge(&HostEvent::NavigationRequested {
                navigation_id: "navigation-1".to_owned(),
                url: "https://example.com/".to_owned(),
            }),
            r#"{"name":"navigationRequested","payload":{"navigationId":"navigation-1","url":"https://example.com/"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::PopupRequested {
                request_id: "request-1".to_owned(),
                parent_webview_id: "parent-1".to_owned(),
                parent_url: Some("https://parent.test/".to_owned()),
                target_url: None,
                window_features: None,
                wants_popup: None,
                policy: PopupRequestPolicy::DefaultDeny,
            }),
            r#"{"name":"popupRequested","payload":{"requestId":"request-1","parentWebViewId":"parent-1","parentUrl":"https://parent.test/","targetUrl":null,"windowFeatures":null,"wantsPopup":null,"policy":"default-deny"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::PopupCreated {
                request_id: "request-1".to_owned(),
                parent_webview_id: "parent-1".to_owned(),
                child_webview_id: "child-1".to_owned(),
                parent_url: Some("https://parent.test/".to_owned()),
                target_url: None,
                window_features: None,
                wants_popup: None,
                policy: PopupRequestPolicy::ManagedChild,
            }),
            r#"{"name":"popupCreated","payload":{"requestId":"request-1","parentWebViewId":"parent-1","childWebViewId":"child-1","parentUrl":"https://parent.test/","targetUrl":null,"windowFeatures":null,"wantsPopup":null,"policy":"managed-child"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::PopupCreationFailed {
                request_id: "request-1".to_owned(),
                error_message: "renderer unavailable".to_owned(),
            }),
            r#"{"name":"popupCreationFailed","payload":{"requestId":"request-1","errorMessage":"renderer unavailable"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::UrlChanged {
                url: "https://example.com/".to_owned(),
            }),
            r#"{"name":"urlChanged","payload":{"url":"https://example.com/"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::LoadStatusChanged {
                status: LoadStatusKind::Complete,
            }),
            r#"{"name":"loadStatusChanged","payload":{"status":"Complete"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::HistoryChanged {
                entries: vec![
                    "https://a.example/".to_owned(),
                    "https://b.example/".to_owned()
                ],
                current: 1,
                can_go_back: true,
                can_go_forward: false,
            }),
            r#"{"name":"historyChanged","payload":{"entries":["https://a.example/","https://b.example/"],"current":1,"canGoBack":true,"canGoForward":false}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::NavigationStateChanged {
                can_go_back: true,
                can_go_forward: false,
            }),
            r#"{"name":"navigationStateChanged","payload":{"canGoBack":true,"canGoForward":false}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::Closed),
            r#"{"name":"closed","payload":{}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::Error {
                url: Some("https:///".to_owned()),
                code: 1,
                message: "invalid url".to_owned(),
            }),
            r#"{"name":"error","payload":{"url":"https:///","code":1,"message":"invalid url"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::JavaScriptEvaluationResult {
                evaluation_id: "evaluation-1".to_owned(),
                ok: false,
                value_json: None,
                error_type: Some(JavaScriptEvaluationErrorKind::WebViewNotReady),
            }),
            r#"{"name":"javascriptEvaluationResult","payload":{"evaluationId":"evaluation-1","ok":false,"valueJson":null,"errorType":"WebViewNotReady"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::SimpleDialogRequested {
                dialog_id: "dialog-1".to_owned(),
                kind: SimpleDialogKind::Prompt,
                message: "Name?".to_owned(),
                default_value: Some("Servo".to_owned()),
            }),
            r#"{"name":"simpleDialogRequested","payload":{"dialogId":"dialog-1","kind":"prompt","message":"Name?","defaultValue":"Servo"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::SimpleDialogDismissed {
                dialog_id: "dialog-1".to_owned(),
            }),
            r#"{"name":"simpleDialogDismissed","payload":{"dialogId":"dialog-1"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::InputMethodRequested {
                input_method_id: "ime-1".to_owned(),
                input_method_type: InputMethodKind::Email,
                text: "hello".to_owned(),
                insertion_point: Some(3),
                multiline: false,
                allow_virtual_keyboard: true,
            }),
            r#"{"name":"inputMethodRequested","payload":{"inputMethodId":"ime-1","type":"email","text":"hello","insertionPoint":3,"multiline":false,"allowVirtualKeyboard":true}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::InputMethodDismissed {
                input_method_id: "ime-1".to_owned(),
            }),
            r#"{"name":"inputMethodDismissed","payload":{"inputMethodId":"ime-1"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::SelectElementDismissed {
                select_element_id: "select-1".to_owned(),
            }),
            r#"{"name":"selectElementDismissed","payload":{"selectElementId":"select-1"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::ContextMenuDismissed {
                context_menu_id: "context-menu-1".to_owned(),
            }),
            r#"{"name":"contextMenuDismissed","payload":{"contextMenuId":"context-menu-1"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::FilePickerDismissed {
                file_picker_id: "file-picker-1".to_owned(),
            }),
            r#"{"name":"filePickerDismissed","payload":{"filePickerId":"file-picker-1"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::PermissionRequested {
                permission: "geolocation".to_owned(),
                origin: "http://127.0.0.1:3000".to_owned(),
            }),
            r#"{"name":"permissionRequested","payload":{"permission":"geolocation","origin":"http://127.0.0.1:3000"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::FocusChanged { is_focused: true }),
            r#"{"name":"focusChanged","payload":{"isFocused":true}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::CursorChanged {
                cursor: "pointer".to_owned(),
            }),
            r#"{"name":"cursorChanged","payload":{"cursor":"pointer"}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::FullscreenChanged {
                is_fullscreen: true,
            }),
            r#"{"name":"fullscreenChanged","payload":{"isFullscreen":true}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::SurfaceAttached {
                size: SurfaceSize::new(640, 480),
            }),
            r#"{"name":"surfaceAttached","payload":{"width":640,"height":480}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::SurfaceResized {
                size: SurfaceSize::new(800, 600),
            }),
            r#"{"name":"surfaceResized","payload":{"width":800,"height":600}}"#
        );
        assert_eq!(
            encode_host_event_bridge(&HostEvent::SurfaceDetached),
            r#"{"name":"surfaceDetached","payload":{}}"#
        );
    }
}
