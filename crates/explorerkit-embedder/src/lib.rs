mod controller_command;
mod host_event_bridge;
#[cfg(all(
    feature = "servo",
    any(
        target_os = "android",
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    )
))]
mod javascript_evaluation;
mod portable_controller;
mod runtime;
#[cfg(all(
    feature = "servo",
    any(
        target_os = "android",
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    )
))]
mod rustls_crypto_provider;
mod servo_adapter;
#[cfg(all(
    feature = "servo",
    any(
        target_os = "android",
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    )
))]
mod servo_webview;
#[cfg(all(
    feature = "servo",
    any(
        target_os = "android",
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    )
))]
mod servo_webview_adapter;

use thiserror::Error;
use url::{ParseError, Url};

pub use controller_command::{ControllerCommand, ControllerCommandError};
pub use explorerkit_host::{HostSurface, SurfacePoint, SurfaceSize, SurfaceViewport};
pub use host_event_bridge::encode_host_event_bridge;
#[cfg(all(
    feature = "servo",
    any(
        target_os = "android",
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    )
))]
pub use javascript_evaluation::{
    host_event_from_javascript_evaluation_result, serialize_javascript_value_json,
};
pub use portable_controller::{
    PortableController, PortableControllerError, PortableControllerResult,
};
pub use runtime::{
    ChildWebViewRegistration, ConfigurableHost, ExplorerKitEvent, Host, HostCall, HostError,
    MockHost, NewWebViewDecision, NewWebViewRequest, PlaceholderHost, Runtime, RuntimeError,
    SessionHandle, WebViewCommand, WebViewHandle, WebViewRegistrar,
};
#[cfg(all(
    feature = "servo",
    any(
        target_os = "android",
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    )
))]
pub use rustls_crypto_provider::{
    ensure_default_rustls_crypto_provider, install_rustls_crypto_provider,
    RustlsCryptoProviderStatus,
};
pub use servo_adapter::{
    ContextMenuBounds, ContextMenuRequest, FilePickerRequest, HiddenEmbedderControl,
    InputMethodRequest, PendingPermissionRequestInfo, PopupCreated, PopupRequest,
    SelectElementRequest, ServoAdapterState, SimpleDialogRequest,
};
#[cfg(all(
    feature = "servo",
    any(
        target_os = "android",
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    )
))]
pub use servo_webview::{ServoRuntime, ServoWebView, ServoWebViewInit};
#[cfg(all(
    feature = "servo",
    any(
        target_os = "android",
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    )
))]
pub use servo_webview_adapter::{
    ManagedChildRenderingContextFactory, ManagedChildWebViewFactory, PreparedManagedChild,
    ServoWebViewAdapter, ServoWebViewEvent,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationRequest {
    pub url: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NavigationError {
    #[error("url must not be empty")]
    Empty,
    #[error("invalid url: {0}")]
    Invalid(String),
}

impl NavigationRequest {
    pub fn new(input: &str) -> Result<Self, NavigationError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(NavigationError::Empty);
        }

        let normalized = match Url::parse(trimmed) {
            Ok(parsed) => parsed.to_string(),
            Err(ParseError::RelativeUrlWithoutBase) => {
                let normalized = format!("https://{trimmed}");
                Url::parse(&normalized)
                    .map_err(|error| NavigationError::Invalid(error.to_string()))?
                    .to_string()
            }
            Err(error) => return Err(NavigationError::Invalid(error.to_string())),
        };

        Ok(Self { url: normalized })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JavaScriptEvaluationErrorKind {
    DocumentNotFound,
    CompilationFailure,
    EvaluationFailure,
    InternalError,
    WebViewNotReady,
    SerializationError,
}

impl JavaScriptEvaluationErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DocumentNotFound => "DocumentNotFound",
            Self::CompilationFailure => "CompilationFailure",
            Self::EvaluationFailure => "EvaluationFailure",
            Self::InternalError => "InternalError",
            Self::WebViewNotReady => "WebViewNotReady",
            Self::SerializationError => "SerializationError",
        }
    }
}

/// Pixel layouts used by owned favicon images.
///
/// Servo favicon pixel formats are preserved without conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaviconPixelFormat {
    /// Luminance channel only.
    K8,
    /// Luminance and alpha channels.
    KA8,
    /// Red, green, and blue channels.
    RGB8,
    /// Red, green, blue, and alpha channels.
    RGBA8,
    /// Blue, green, red, and alpha channels.
    BGRA8,
}

/// An owned decoded first-frame favicon image.
///
/// Servo views copy their first frame without conversion. macOS system views
/// decode WebKit-provided image data into straight RGBA8 pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaviconImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Pixel layout used by [`Self::bytes`].
    pub format: FaviconPixelFormat,
    /// Unmodified bytes from Servo's first image frame, or decoded straight
    /// RGBA8 bytes for a macOS system view.
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    NavigationRequested {
        navigation_id: String,
        url: String,
    },
    PopupRequested {
        request_id: String,
        parent_webview_id: String,
        parent_url: Option<String>,
        target_url: Option<String>,
        window_features: Option<String>,
        wants_popup: Option<bool>,
        policy: PopupRequestPolicy,
    },
    PopupCreated {
        request_id: String,
        parent_webview_id: String,
        child_webview_id: String,
        parent_url: Option<String>,
        target_url: Option<String>,
        window_features: Option<String>,
        wants_popup: Option<bool>,
        policy: PopupRequestPolicy,
    },
    /// A permitted native child could not be prepared before native acceptance.
    PopupCreationFailed {
        /// The original creation attempt's opaque identifier.
        request_id: String,
        /// The ordinary preparation or construction error.
        error_message: String,
    },
    UrlChanged {
        url: String,
    },
    PageTitleChanged {
        title: Option<String>,
    },
    /// A new favicon value for this event's webview.
    FaviconChanged {
        /// The current first-frame image, or `None` when no image is available.
        ///
        /// Servo forwards its engine value. A macOS system view with native icon
        /// support also emits `None` at document commit when no retained image is
        /// available; a later native load may supply an image.
        favicon: Option<FaviconImage>,
    },
    StatusTextChanged {
        status: Option<String>,
    },
    LoadStatusChanged {
        status: LoadStatusKind,
    },
    HistoryChanged {
        entries: Vec<String>,
        current: usize,
        can_go_back: bool,
        can_go_forward: bool,
    },
    NavigationStateChanged {
        can_go_back: bool,
        can_go_forward: bool,
    },
    Closed,
    Crashed {
        url: Option<String>,
        reason: String,
        backtrace: Option<String>,
    },
    Error {
        url: Option<String>,
        code: i32,
        message: String,
    },
    JavaScriptEvaluationResult {
        evaluation_id: String,
        ok: bool,
        value_json: Option<String>,
        error_type: Option<JavaScriptEvaluationErrorKind>,
    },
    SimpleDialogRequested {
        dialog_id: String,
        kind: SimpleDialogKind,
        message: String,
        default_value: Option<String>,
    },
    SimpleDialogDismissed {
        dialog_id: String,
    },
    InputMethodRequested {
        input_method_id: String,
        input_method_type: InputMethodKind,
        text: String,
        insertion_point: Option<u32>,
        multiline: bool,
        allow_virtual_keyboard: bool,
    },
    InputMethodDismissed {
        input_method_id: String,
    },
    SelectElementRequested {
        select_element_id: String,
        options: Vec<SelectElementOptionOrOptgroup>,
        selected_options: Vec<usize>,
        allow_select_multiple: bool,
    },
    SelectElementDismissed {
        select_element_id: String,
    },
    ContextMenuRequested {
        context_menu_id: String,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        element_info: ContextMenuElementInformation,
        items: Vec<ContextMenuItem>,
    },
    ContextMenuDismissed {
        context_menu_id: String,
    },
    FilePickerRequested {
        file_picker_id: String,
        current_paths: Vec<String>,
        filter_patterns: Vec<String>,
        allow_select_multiple: bool,
    },
    FilePickerDismissed {
        file_picker_id: String,
    },
    PermissionRequested {
        permission: String,
        origin: String,
    },
    FocusChanged {
        is_focused: bool,
    },
    CursorChanged {
        cursor: String,
    },
    FullscreenChanged {
        is_fullscreen: bool,
    },
    SurfaceAttached {
        size: SurfaceSize,
    },
    SurfaceResized {
        size: SurfaceSize,
    },
    SurfaceDetached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchEventKind {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostInputEvent {
    Pointer(PointerInputEvent),
    Keyboard(KeyboardInputEvent),
    ImeCommit { text: String },
    Focus { is_focused: bool },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerInputEvent {
    Moved {
        x: f32,
        y: f32,
    },
    Button {
        action: PointerButtonAction,
        button: PointerButton,
        x: f32,
        y: f32,
    },
    Wheel {
        delta_x: f64,
        delta_y: f64,
        mode: PointerScrollMode,
        x: f32,
        y: f32,
    },
    LeftViewport,
}

impl PointerInputEvent {
    pub fn moved(x: f32, y: f32) -> Self {
        Self::Moved { x, y }
    }

    pub fn left_viewport() -> Self {
        Self::LeftViewport
    }

    pub fn button(action: PointerButtonAction, button: PointerButton, x: f32, y: f32) -> Self {
        Self::Button {
            action,
            button,
            x,
            y,
        }
    }

    pub fn wheel(delta_x: f64, delta_y: f64, mode: PointerScrollMode, x: f32, y: f32) -> Self {
        Self::Wheel {
            delta_x,
            delta_y,
            mode,
            x,
            y,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButtonAction {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerScrollMode {
    Lines,
    Pixels,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
    Back,
    Forward,
    Other(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardInputEvent {
    pub key: KeyboardInputKey,
    pub state: KeyboardInputState,
    pub repeat: bool,
    pub is_composing: bool,
}

impl KeyboardInputEvent {
    pub fn new(key: KeyboardInputKey, state: KeyboardInputState) -> Self {
        Self {
            key,
            state,
            repeat: false,
            is_composing: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyboardInputKey {
    Character(String),
    Named(KeyboardNamedKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardNamedKey {
    Backspace,
    Delete,
    Enter,
    Tab,
    Escape,
    Space,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardInputState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadStatusKind {
    Started,
    HeadParsed,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PopupRequestPolicy {
    #[default]
    DefaultDeny,
    ManagedChild,
}

impl PopupRequestPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DefaultDeny => "default-deny",
            Self::ManagedChild => "managed-child",
        }
    }
}

impl LoadStatusKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "Started",
            Self::HeadParsed => "HeadParsed",
            Self::Complete => "Complete",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimpleDialogKind {
    Alert,
    Confirm,
    Prompt,
}

impl SimpleDialogKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alert => "alert",
            Self::Confirm => "confirm",
            Self::Prompt => "prompt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectElementOption {
    pub id: usize,
    pub label: String,
    pub is_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectElementOptionOrOptgroup {
    Option(SelectElementOption),
    Optgroup {
        label: String,
        options: Vec<SelectElementOption>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMethodKind {
    Color,
    Date,
    DatetimeLocal,
    Email,
    Month,
    Number,
    Password,
    Search,
    Tel,
    Text,
    Time,
    Url,
    Week,
}

impl InputMethodKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Date => "date",
            Self::DatetimeLocal => "datetime-local",
            Self::Email => "email",
            Self::Month => "month",
            Self::Number => "number",
            Self::Password => "password",
            Self::Search => "search",
            Self::Tel => "tel",
            Self::Text => "text",
            Self::Time => "time",
            Self::Url => "url",
            Self::Week => "week",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuItem {
    Item {
        label: String,
        action: ContextMenuAction,
        enabled: bool,
    },
    Separator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMenuElementInformation {
    pub is_link: bool,
    pub is_image: bool,
    pub is_editable_text: bool,
    pub has_selection: bool,
    pub link_url: Option<String>,
    pub image_url: Option<String>,
    pub context_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuAction {
    GoBack,
    GoForward,
    Reload,
    CopyLink,
    OpenLinkInNewWebView,
    CopyImageLink,
    OpenImageInNewView,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

impl ContextMenuAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GoBack => "go-back",
            Self::GoForward => "go-forward",
            Self::Reload => "reload",
            Self::CopyLink => "copy-link",
            Self::OpenLinkInNewWebView => "open-link-in-new-webview",
            Self::CopyImageLink => "copy-image-link",
            Self::OpenImageInNewView => "open-image-in-new-view",
            Self::Cut => "cut",
            Self::Copy => "copy",
            Self::Paste => "paste",
            Self::SelectAll => "select-all",
        }
    }

    #[allow(
        clippy::should_implement_trait,
        reason = "Preserve the shared command interface's existing optional parser."
    )]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "go-back" => Some(Self::GoBack),
            "go-forward" => Some(Self::GoForward),
            "reload" => Some(Self::Reload),
            "copy-link" => Some(Self::CopyLink),
            "open-link-in-new-webview" => Some(Self::OpenLinkInNewWebView),
            "copy-image-link" => Some(Self::CopyImageLink),
            "open-image-in-new-view" => Some(Self::OpenImageInNewView),
            "cut" => Some(Self::Cut),
            "copy" => Some(Self::Copy),
            "paste" => Some(Self::Paste),
            "select-all" => Some(Self::SelectAll),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImeCompositionState {
    Update,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardKey {
    Backspace,
    Delete,
    Enter,
}

impl HostEvent {
    pub fn error_code(&self) -> Option<i32> {
        match self {
            Self::Error { code, .. } => Some(*code),
            _ => None,
        }
    }

    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Error { message, .. } => Some(message),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_urls_and_rejects_empty_input() {
        assert_eq!(
            NavigationRequest::new("example.com").unwrap().url,
            "https://example.com/"
        );
        assert_eq!(
            NavigationRequest::new("data:text/plain,hello").unwrap().url,
            "data:text/plain,hello"
        );
        assert!(NavigationRequest::new("   ").is_err());
    }
}
