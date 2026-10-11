use std::collections::{HashSet, VecDeque};

use crate::{
    ContextMenuElementInformation, ContextMenuItem, FaviconImage, HostEvent, InputMethodKind,
    JavaScriptEvaluationErrorKind, LoadStatusKind, PopupRequestPolicy,
    SelectElementOptionOrOptgroup, SimpleDialogKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputMethodRequest {
    pub input_method_id: String,
    pub input_method_type: InputMethodKind,
    pub text: String,
    pub insertion_point: Option<u32>,
    pub multiline: bool,
    pub allow_virtual_keyboard: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectElementRequest {
    pub select_element_id: String,
    pub options: Vec<SelectElementOptionOrOptgroup>,
    pub selected_options: Vec<usize>,
    pub allow_select_multiple: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextMenuBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl ContextMenuBounds {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width: width.max(0) as u32,
            height: height.max(0) as u32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMenuRequest {
    pub context_menu_id: String,
    pub bounds: ContextMenuBounds,
    pub element_info: ContextMenuElementInformation,
    pub items: Vec<ContextMenuItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePickerRequest {
    pub file_picker_id: String,
    pub current_paths: Vec<String>,
    pub filter_patterns: Vec<String>,
    pub allow_select_multiple: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleDialogRequest {
    pub dialog_id: String,
    pub kind: SimpleDialogKind,
    pub message: String,
    pub default_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPermissionRequestInfo {
    pub permission: String,
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupRequest {
    pub request_id: String,
    pub parent_webview_id: String,
    pub parent_url: Option<String>,
    pub target_url: Option<String>,
    pub window_features: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupCreated {
    pub request_id: String,
    pub parent_webview_id: String,
    pub child_webview_id: String,
    pub parent_url: Option<String>,
    pub target_url: Option<String>,
    pub window_features: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HiddenEmbedderControl {
    InputMethod,
    SelectElement,
    FilePicker,
    ContextMenu,
    SimpleDialog,
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct ServoAdapterState {
    current_url: Option<String>,
    events: VecDeque<HostEvent>,
    needs_paint: bool,
    next_navigation_request_id: u64,
    pending_navigation_requests: HashSet<String>,
    pending_simple_dialogs: HashSet<String>,
    pending_select_elements: HashSet<String>,
    pending_context_menus: HashSet<String>,
    pending_file_pickers: HashSet<String>,
    pending_permission_request: Option<PendingPermissionRequestInfo>,
    visible_input_methods: HashSet<String>,
}

impl ServoAdapterState {
    pub fn current_url(&self) -> Option<&str> {
        self.current_url.as_deref()
    }

    pub fn request_paint(&mut self) {
        self.needs_paint = true;
    }

    pub fn take_needs_paint(&mut self) -> bool {
        let needs_paint = self.needs_paint;
        self.needs_paint = false;
        needs_paint
    }

    pub fn drain_events(&mut self) -> Vec<HostEvent> {
        self.events.drain(..).collect()
    }

    pub fn notify_url_changed(&mut self, url: &str) {
        self.current_url = Some(url.to_owned());
        self.events.push_back(HostEvent::UrlChanged {
            url: url.to_owned(),
        });
    }

    pub fn notify_page_title_changed(&mut self, title: Option<String>) {
        self.events.push_back(HostEvent::PageTitleChanged { title });
    }

    pub fn notify_favicon_changed(&mut self, favicon: Option<FaviconImage>) {
        self.events.push_back(HostEvent::FaviconChanged { favicon });
    }

    pub fn notify_status_text_changed(&mut self, status: Option<String>) {
        self.events
            .push_back(HostEvent::StatusTextChanged { status });
    }

    pub fn notify_focus_changed(&mut self, is_focused: bool) {
        self.events
            .push_back(HostEvent::FocusChanged { is_focused });
    }

    pub fn notify_load_status_changed(&mut self, status: LoadStatusKind) {
        self.events
            .push_back(HostEvent::LoadStatusChanged { status });
    }

    pub fn notify_cursor_changed(&mut self, cursor: &str) {
        self.events.push_back(HostEvent::CursorChanged {
            cursor: cursor.to_owned(),
        });
    }

    pub fn notify_history_changed(
        &mut self,
        entries: Vec<String>,
        current: usize,
        can_go_back: bool,
        can_go_forward: bool,
    ) {
        self.events.push_back(HostEvent::HistoryChanged {
            entries,
            current,
            can_go_back,
            can_go_forward,
        });
    }

    pub fn notify_closed(&mut self) {
        self.events.push_back(HostEvent::Closed);
    }

    pub fn notify_fullscreen_state_changed(&mut self, is_fullscreen: bool) {
        self.events
            .push_back(HostEvent::FullscreenChanged { is_fullscreen });
    }

    pub fn notify_surface_attached(&mut self, size: explorerkit_host::SurfaceSize) {
        self.events.push_back(HostEvent::SurfaceAttached { size });
    }

    pub fn notify_surface_resized(&mut self, size: explorerkit_host::SurfaceSize) {
        self.events.push_back(HostEvent::SurfaceResized { size });
    }

    pub fn notify_surface_detached(&mut self) {
        self.events.push_back(HostEvent::SurfaceDetached);
    }

    pub fn notify_crashed(
        &mut self,
        webview_url: Option<&str>,
        reason: String,
        backtrace: Option<String>,
    ) {
        self.events.push_back(HostEvent::Crashed {
            url: webview_url
                .map(str::to_owned)
                .or_else(|| self.current_url.clone()),
            reason,
            backtrace,
        });
    }

    pub fn notify_javascript_evaluation_result(
        &mut self,
        evaluation_id: String,
        ok: bool,
        value_json: Option<String>,
        error_type: Option<JavaScriptEvaluationErrorKind>,
    ) {
        self.events
            .push_back(HostEvent::JavaScriptEvaluationResult {
                evaluation_id,
                ok,
                value_json,
                error_type,
            });
    }

    pub fn request_navigation(&mut self, url: String) -> String {
        let navigation_id = self.next_navigation_request_id();
        self.request_navigation_with_id(navigation_id, url)
    }

    pub fn request_navigation_with_id(&mut self, navigation_id: String, url: String) -> String {
        self.pending_navigation_requests
            .insert(navigation_id.clone());
        self.events.push_back(HostEvent::NavigationRequested {
            navigation_id: navigation_id.clone(),
            url,
        });
        navigation_id
    }

    pub fn resolve_navigation_request(&mut self, navigation_id: &str) -> Result<(), String> {
        remove_pending(
            &mut self.pending_navigation_requests,
            navigation_id,
            "navigation request",
        )
    }

    pub fn request_popup(&mut self, request: PopupRequest) {
        self.events.push_back(HostEvent::PopupRequested {
            request_id: request.request_id,
            parent_webview_id: request.parent_webview_id,
            parent_url: request.parent_url,
            target_url: request.target_url,
            window_features: request.window_features,
            policy: PopupRequestPolicy::DefaultDeny,
        });
    }

    pub fn notify_popup_created(&mut self, created: PopupCreated) {
        self.events.push_back(HostEvent::PopupCreated {
            request_id: created.request_id,
            parent_webview_id: created.parent_webview_id,
            child_webview_id: created.child_webview_id,
            parent_url: created.parent_url,
            target_url: created.target_url,
            window_features: created.window_features,
            policy: PopupRequestPolicy::ManagedChild,
        });
    }

    pub fn request_permission(
        &mut self,
        permission: String,
        origin: String,
    ) -> Option<PendingPermissionRequestInfo> {
        let previous = self
            .pending_permission_request
            .replace(PendingPermissionRequestInfo {
                permission: permission.clone(),
                origin: origin.clone(),
            });
        self.events
            .push_back(HostEvent::PermissionRequested { permission, origin });
        previous
    }

    pub fn pending_permission_request(&self) -> Option<PendingPermissionRequestInfo> {
        self.pending_permission_request.clone()
    }

    pub fn resolve_permission(&mut self) -> Result<(), String> {
        self.pending_permission_request
            .take()
            .map(|_| ())
            .ok_or_else(|| "permission request is no longer pending".to_owned())
    }

    pub fn show_input_method(&mut self, request: InputMethodRequest) {
        self.visible_input_methods
            .insert(request.input_method_id.clone());
        self.events.push_back(HostEvent::InputMethodRequested {
            input_method_id: request.input_method_id,
            input_method_type: request.input_method_type,
            text: request.text,
            insertion_point: request.insertion_point,
            multiline: request.multiline,
            allow_virtual_keyboard: request.allow_virtual_keyboard,
        });
    }

    pub fn show_select_element(&mut self, request: SelectElementRequest) {
        self.pending_select_elements
            .insert(request.select_element_id.clone());
        self.events.push_back(HostEvent::SelectElementRequested {
            select_element_id: request.select_element_id,
            options: request.options,
            selected_options: request.selected_options,
            allow_select_multiple: request.allow_select_multiple,
        });
    }

    pub fn resolve_select_element(&mut self, select_element_id: &str) -> Result<(), String> {
        remove_pending(
            &mut self.pending_select_elements,
            select_element_id,
            "select element",
        )
    }

    pub fn show_context_menu(&mut self, request: ContextMenuRequest) {
        self.pending_context_menus
            .insert(request.context_menu_id.clone());
        self.events.push_back(HostEvent::ContextMenuRequested {
            context_menu_id: request.context_menu_id,
            x: request.bounds.x,
            y: request.bounds.y,
            width: request.bounds.width,
            height: request.bounds.height,
            element_info: request.element_info,
            items: request.items,
        });
    }

    pub fn resolve_context_menu(&mut self, context_menu_id: &str) -> Result<(), String> {
        remove_pending(
            &mut self.pending_context_menus,
            context_menu_id,
            "context menu",
        )
    }

    pub fn show_file_picker(&mut self, request: FilePickerRequest) {
        self.pending_file_pickers
            .insert(request.file_picker_id.clone());
        self.events.push_back(HostEvent::FilePickerRequested {
            file_picker_id: request.file_picker_id,
            current_paths: request.current_paths,
            filter_patterns: request.filter_patterns,
            allow_select_multiple: request.allow_select_multiple,
        });
    }

    pub fn resolve_file_picker(&mut self, file_picker_id: &str) -> Result<(), String> {
        remove_pending(
            &mut self.pending_file_pickers,
            file_picker_id,
            "file picker",
        )
    }

    pub fn show_simple_dialog(&mut self, request: SimpleDialogRequest) {
        self.pending_simple_dialogs
            .insert(request.dialog_id.clone());
        self.events.push_back(HostEvent::SimpleDialogRequested {
            dialog_id: request.dialog_id,
            kind: request.kind,
            message: request.message,
            default_value: request.default_value,
        });
    }

    pub fn resolve_simple_dialog(&mut self, dialog_id: &str) -> Result<(), String> {
        remove_pending(&mut self.pending_simple_dialogs, dialog_id, "simple dialog")
    }

    pub fn hide_embedder_control(&mut self, control_id: &str) -> HiddenEmbedderControl {
        if self.visible_input_methods.remove(control_id) {
            self.events.push_back(HostEvent::InputMethodDismissed {
                input_method_id: control_id.to_owned(),
            });
            return HiddenEmbedderControl::InputMethod;
        }

        if self.pending_select_elements.remove(control_id) {
            self.events.push_back(HostEvent::SelectElementDismissed {
                select_element_id: control_id.to_owned(),
            });
            return HiddenEmbedderControl::SelectElement;
        }

        if self.pending_file_pickers.remove(control_id) {
            self.events.push_back(HostEvent::FilePickerDismissed {
                file_picker_id: control_id.to_owned(),
            });
            return HiddenEmbedderControl::FilePicker;
        }

        if self.pending_context_menus.remove(control_id) {
            self.events.push_back(HostEvent::ContextMenuDismissed {
                context_menu_id: control_id.to_owned(),
            });
            return HiddenEmbedderControl::ContextMenu;
        }

        if self.pending_simple_dialogs.remove(control_id) {
            self.events.push_back(HostEvent::SimpleDialogDismissed {
                dialog_id: control_id.to_owned(),
            });
            return HiddenEmbedderControl::SimpleDialog;
        }

        HiddenEmbedderControl::Unknown
    }

    fn next_navigation_request_id(&mut self) -> String {
        self.next_navigation_request_id = self.next_navigation_request_id.wrapping_add(1);
        format!("navigation-{}", self.next_navigation_request_id)
    }
}

fn remove_pending(pending: &mut HashSet<String>, id: &str, label: &str) -> Result<(), String> {
    pending
        .remove(id)
        .then_some(())
        .ok_or_else(|| format!("{label} {id} is no longer pending"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContextMenuAction, SelectElementOption, SelectElementOptionOrOptgroup, SimpleDialogKind,
    };

    fn drain_one(state: &mut ServoAdapterState) -> HostEvent {
        let events = state.drain_events();
        assert_eq!(events.len(), 1);
        events.into_iter().next().unwrap()
    }

    #[test]
    fn tracks_paint_requests_without_exposing_rendering_objects() {
        let mut state = ServoAdapterState::default();

        assert!(!state.take_needs_paint());
        state.request_paint();
        assert!(state.take_needs_paint());
        assert!(!state.take_needs_paint());
    }

    #[test]
    fn translates_basic_delegate_notifications_in_order() {
        let mut state = ServoAdapterState::default();

        state.notify_url_changed("https://example.com/page");
        state.notify_page_title_changed(Some("Servo".to_owned()));
        state.notify_status_text_changed(None);
        state.notify_load_status_changed(LoadStatusKind::Complete);
        state.notify_history_changed(
            vec![
                "https://example.com/".to_owned(),
                "https://example.com/page".to_owned(),
            ],
            1,
            true,
            false,
        );
        state.notify_focus_changed(true);
        state.notify_cursor_changed("pointer");
        state.notify_fullscreen_state_changed(true);
        state.notify_crashed(
            Some("https://servo.test/page"),
            "renderer crashed".to_owned(),
            Some("trace".to_owned()),
        );
        state.notify_closed();

        assert_eq!(
            state.drain_events(),
            vec![
                HostEvent::UrlChanged {
                    url: "https://example.com/page".to_owned()
                },
                HostEvent::PageTitleChanged {
                    title: Some("Servo".to_owned())
                },
                HostEvent::StatusTextChanged { status: None },
                HostEvent::LoadStatusChanged {
                    status: LoadStatusKind::Complete,
                },
                HostEvent::HistoryChanged {
                    entries: vec![
                        "https://example.com/".to_owned(),
                        "https://example.com/page".to_owned(),
                    ],
                    current: 1,
                    can_go_back: true,
                    can_go_forward: false,
                },
                HostEvent::FocusChanged { is_focused: true },
                HostEvent::CursorChanged {
                    cursor: "pointer".to_owned()
                },
                HostEvent::FullscreenChanged {
                    is_fullscreen: true
                },
                HostEvent::Crashed {
                    url: Some("https://servo.test/page".to_owned()),
                    reason: "renderer crashed".to_owned(),
                    backtrace: Some("trace".to_owned()),
                },
                HostEvent::Closed,
            ]
        );
    }

    #[test]
    fn forwards_favicon_values_without_navigation_state() {
        let mut state = ServoAdapterState::default();
        let favicon = FaviconImage {
            width: 1,
            height: 1,
            format: crate::FaviconPixelFormat::RGBA8,
            bytes: vec![1, 2, 3, 4],
        };

        state.notify_favicon_changed(Some(favicon.clone()));
        state.notify_favicon_changed(None);

        assert_eq!(
            state.drain_events(),
            vec![
                HostEvent::FaviconChanged {
                    favicon: Some(favicon),
                },
                HostEvent::FaviconChanged { favicon: None },
            ]
        );
        assert_eq!(state.current_url(), None);
    }

    #[test]
    fn reports_confirmed_about_blank_without_a_pending_navigation() {
        let mut state = ServoAdapterState::default();

        state.notify_url_changed("about:blank");
        assert_eq!(
            drain_one(&mut state),
            HostEvent::UrlChanged {
                url: "about:blank".to_owned(),
            }
        );
        assert_eq!(state.current_url(), Some("about:blank"));
    }

    #[test]
    fn requested_navigation_does_not_advance_or_decorate_observed_url() {
        let mut state = ServoAdapterState::default();

        state.request_navigation("https://example.com/requested".to_owned());
        assert_eq!(state.current_url(), None);
        assert_eq!(
            drain_one(&mut state),
            HostEvent::NavigationRequested {
                navigation_id: "navigation-1".to_owned(),
                url: "https://example.com/requested".to_owned(),
            }
        );

        state.notify_load_status_changed(LoadStatusKind::Started);
        assert_eq!(
            drain_one(&mut state),
            HostEvent::LoadStatusChanged {
                status: LoadStatusKind::Started,
            }
        );
        state.notify_crashed(None, "before history".to_owned(), None);
        assert_eq!(
            drain_one(&mut state),
            HostEvent::Crashed {
                url: None,
                reason: "before history".to_owned(),
                backtrace: None,
            }
        );
        assert_eq!(state.current_url(), None);
    }

    #[test]
    fn crash_context_prefers_webview_snapshot_then_confirmed_url() {
        let mut state = ServoAdapterState::default();

        state.notify_url_changed("https://example.com/confirmed");
        let _ = drain_one(&mut state);
        state.notify_crashed(
            Some("https://servo.test/current"),
            "snapshot crash".to_owned(),
            None,
        );
        assert_eq!(
            drain_one(&mut state),
            HostEvent::Crashed {
                url: Some("https://servo.test/current".to_owned()),
                reason: "snapshot crash".to_owned(),
                backtrace: None,
            }
        );
        assert_eq!(state.current_url(), Some("https://example.com/confirmed"));
        state.notify_crashed(None, "cached crash".to_owned(), Some("trace".to_owned()));
        assert_eq!(
            drain_one(&mut state),
            HostEvent::Crashed {
                url: Some("https://example.com/confirmed".to_owned()),
                reason: "cached crash".to_owned(),
                backtrace: Some("trace".to_owned()),
            }
        );
    }

    #[test]
    fn tracks_navigation_policy_identity_and_stale_responses() {
        let mut state = ServoAdapterState::default();

        let first = state.request_navigation("https://example.com/first".to_owned());
        let second = state.request_navigation("https://example.com/second".to_owned());

        assert_eq!(first, "navigation-1");
        assert_eq!(second, "navigation-2");
        assert_eq!(
            state.drain_events(),
            vec![
                HostEvent::NavigationRequested {
                    navigation_id: "navigation-1".to_owned(),
                    url: "https://example.com/first".to_owned(),
                },
                HostEvent::NavigationRequested {
                    navigation_id: "navigation-2".to_owned(),
                    url: "https://example.com/second".to_owned(),
                },
            ]
        );

        state.resolve_navigation_request(&first).unwrap();
        assert_eq!(
            state.resolve_navigation_request(&first),
            Err("navigation request navigation-1 is no longer pending".to_owned())
        );
        state.resolve_navigation_request(&second).unwrap();
    }

    #[test]
    fn records_default_denied_popup_requests_without_pending_child_state() {
        let mut state = ServoAdapterState::default();

        state.request_popup(PopupRequest {
            request_id: "request-1".to_owned(),
            parent_webview_id: "parent-1".to_owned(),
            parent_url: Some("https://parent.test/".to_owned()),
            target_url: None,
            window_features: None,
        });

        assert_eq!(
            state.drain_events(),
            vec![HostEvent::PopupRequested {
                request_id: "request-1".to_owned(),
                parent_webview_id: "parent-1".to_owned(),
                parent_url: Some("https://parent.test/".to_owned()),
                target_url: None,
                window_features: None,
                policy: PopupRequestPolicy::DefaultDeny,
            }]
        );
        assert!(state.drain_events().is_empty());
    }

    #[test]
    fn records_managed_popup_created_events_with_parent_and_child_ids() {
        let mut state = ServoAdapterState::default();

        state.notify_popup_created(PopupCreated {
            request_id: "request-1".to_owned(),
            parent_webview_id: "parent-1".to_owned(),
            child_webview_id: "child-1".to_owned(),
            parent_url: Some("https://parent.test/".to_owned()),
            target_url: None,
            window_features: None,
        });

        assert_eq!(
            state.drain_events(),
            vec![HostEvent::PopupCreated {
                request_id: "request-1".to_owned(),
                parent_webview_id: "parent-1".to_owned(),
                child_webview_id: "child-1".to_owned(),
                parent_url: Some("https://parent.test/".to_owned()),
                target_url: None,
                window_features: None,
                policy: PopupRequestPolicy::ManagedChild,
            }]
        );
        assert!(state.drain_events().is_empty());
    }

    #[test]
    fn translates_embedder_controls_and_hide_events_at_the_boundary() {
        let mut state = ServoAdapterState::default();
        let element_info = ContextMenuElementInformation {
            is_link: true,
            is_image: false,
            is_editable_text: false,
            has_selection: false,
            link_url: Some("https://example.com/".to_owned()),
            image_url: None,
            context_type: "link".to_owned(),
        };
        let context_items = vec![
            ContextMenuItem::Item {
                label: "Copy link".to_owned(),
                action: ContextMenuAction::CopyLink,
                enabled: true,
            },
            ContextMenuItem::Separator,
        ];

        state.show_input_method(InputMethodRequest {
            input_method_id: "ime-1".to_owned(),
            input_method_type: InputMethodKind::Email,
            text: "hello".to_owned(),
            insertion_point: Some(2),
            multiline: false,
            allow_virtual_keyboard: true,
        });
        state.show_select_element(SelectElementRequest {
            select_element_id: "select-1".to_owned(),
            options: vec![
                SelectElementOptionOrOptgroup::Option(SelectElementOption {
                    id: 7,
                    label: "Servo".to_owned(),
                    is_disabled: false,
                }),
                SelectElementOptionOrOptgroup::Option(SelectElementOption {
                    id: 11,
                    label: "Gecko".to_owned(),
                    is_disabled: false,
                }),
            ],
            selected_options: vec![7, 11],
            allow_select_multiple: true,
        });
        state.show_context_menu(ContextMenuRequest {
            context_menu_id: "context-menu-1".to_owned(),
            bounds: ContextMenuBounds::new(12, 24, -1, 48),
            element_info: element_info.clone(),
            items: context_items.clone(),
        });
        state.show_file_picker(FilePickerRequest {
            file_picker_id: "file-picker-1".to_owned(),
            current_paths: vec!["/tmp/one.txt".to_owned()],
            filter_patterns: vec!["txt".to_owned()],
            allow_select_multiple: true,
        });
        state.show_simple_dialog(SimpleDialogRequest {
            dialog_id: "dialog-1".to_owned(),
            kind: SimpleDialogKind::Prompt,
            message: "Name?".to_owned(),
            default_value: Some("Servo".to_owned()),
        });

        assert_eq!(
            state.drain_events(),
            vec![
                HostEvent::InputMethodRequested {
                    input_method_id: "ime-1".to_owned(),
                    input_method_type: InputMethodKind::Email,
                    text: "hello".to_owned(),
                    insertion_point: Some(2),
                    multiline: false,
                    allow_virtual_keyboard: true,
                },
                HostEvent::SelectElementRequested {
                    select_element_id: "select-1".to_owned(),
                    options: vec![
                        SelectElementOptionOrOptgroup::Option(SelectElementOption {
                            id: 7,
                            label: "Servo".to_owned(),
                            is_disabled: false,
                        }),
                        SelectElementOptionOrOptgroup::Option(SelectElementOption {
                            id: 11,
                            label: "Gecko".to_owned(),
                            is_disabled: false,
                        }),
                    ],
                    selected_options: vec![7, 11],
                    allow_select_multiple: true,
                },
                HostEvent::ContextMenuRequested {
                    context_menu_id: "context-menu-1".to_owned(),
                    x: 12,
                    y: 24,
                    width: 0,
                    height: 48,
                    element_info,
                    items: context_items,
                },
                HostEvent::FilePickerRequested {
                    file_picker_id: "file-picker-1".to_owned(),
                    current_paths: vec!["/tmp/one.txt".to_owned()],
                    filter_patterns: vec!["txt".to_owned()],
                    allow_select_multiple: true,
                },
                HostEvent::SimpleDialogRequested {
                    dialog_id: "dialog-1".to_owned(),
                    kind: SimpleDialogKind::Prompt,
                    message: "Name?".to_owned(),
                    default_value: Some("Servo".to_owned()),
                },
            ]
        );

        assert_eq!(
            state.hide_embedder_control("ime-1"),
            HiddenEmbedderControl::InputMethod
        );
        assert_eq!(
            state.hide_embedder_control("select-1"),
            HiddenEmbedderControl::SelectElement
        );
        assert_eq!(
            state.hide_embedder_control("file-picker-1"),
            HiddenEmbedderControl::FilePicker
        );
        assert_eq!(
            state.hide_embedder_control("context-menu-1"),
            HiddenEmbedderControl::ContextMenu
        );
        assert_eq!(
            state.hide_embedder_control("dialog-1"),
            HiddenEmbedderControl::SimpleDialog
        );
        assert_eq!(
            state.hide_embedder_control("unknown"),
            HiddenEmbedderControl::Unknown
        );
        assert_eq!(
            state.drain_events(),
            vec![
                HostEvent::InputMethodDismissed {
                    input_method_id: "ime-1".to_owned(),
                },
                HostEvent::SelectElementDismissed {
                    select_element_id: "select-1".to_owned(),
                },
                HostEvent::FilePickerDismissed {
                    file_picker_id: "file-picker-1".to_owned(),
                },
                HostEvent::ContextMenuDismissed {
                    context_menu_id: "context-menu-1".to_owned(),
                },
                HostEvent::SimpleDialogDismissed {
                    dialog_id: "dialog-1".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn resolved_embedder_controls_are_not_dismissed_again() {
        let mut state = ServoAdapterState::default();

        state.show_simple_dialog(SimpleDialogRequest {
            dialog_id: "dialog-1".to_owned(),
            kind: SimpleDialogKind::Confirm,
            message: "Continue?".to_owned(),
            default_value: None,
        });
        state.drain_events();

        state.resolve_simple_dialog("dialog-1").unwrap();
        assert_eq!(
            state.resolve_simple_dialog("dialog-1"),
            Err("simple dialog dialog-1 is no longer pending".to_owned())
        );
        assert_eq!(
            state.hide_embedder_control("dialog-1"),
            HiddenEmbedderControl::Unknown
        );
        assert!(state.drain_events().is_empty());
    }

    #[test]
    fn tracks_permission_replacement_and_resolution() {
        let mut state = ServoAdapterState::default();

        assert!(state
            .request_permission("geolocation".to_owned(), "https://a.test".to_owned())
            .is_none());
        assert_eq!(
            state.request_permission("notifications".to_owned(), "https://b.test".to_owned()),
            Some(PendingPermissionRequestInfo {
                permission: "geolocation".to_owned(),
                origin: "https://a.test".to_owned(),
            })
        );
        assert_eq!(
            state.pending_permission_request(),
            Some(PendingPermissionRequestInfo {
                permission: "notifications".to_owned(),
                origin: "https://b.test".to_owned(),
            })
        );
        assert_eq!(
            state.drain_events(),
            vec![
                HostEvent::PermissionRequested {
                    permission: "geolocation".to_owned(),
                    origin: "https://a.test".to_owned(),
                },
                HostEvent::PermissionRequested {
                    permission: "notifications".to_owned(),
                    origin: "https://b.test".to_owned(),
                },
            ]
        );

        state.resolve_permission().unwrap();
        assert_eq!(
            state.resolve_permission(),
            Err("permission request is no longer pending".to_owned())
        );
    }
}
