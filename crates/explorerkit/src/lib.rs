#![deny(missing_docs)]

//! Public Rust facade for native ExplorerKit callers.
//!
//! `explorerkit` exposes a curated public surface with namespaced modules for the
//! runtime, webview, surface, controls, events, input, and host paths. Lower
//! crates remain free to reorganize internally without widening the facade by
//! accident.

#[cfg(all(
    feature = "servo",
    any(
        target_os = "android",
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    )
))]
mod surface_host;

pub mod controls;
pub mod events;
pub mod host;
pub mod input;
pub mod runtime;
pub mod surface;
pub mod webview;

pub use events::{ExplorerKitEvent, FaviconImage, FaviconPixelFormat, HostEvent, LoadStatusKind};
pub use input::HostInputEvent;
pub use runtime::{
    ExplorerKitError, NewWebViewDecision, NewWebViewRequest, Runtime, SessionHandle,
};
pub use surface::{HostSurface, SurfacePoint, SurfaceSize, SurfaceViewport};
pub use webview::WebViewHandle;

#[cfg(test)]
mod tests {
    use crate::{
        controls::{
            ContextMenuAction, ContextMenuElementInformation, ContextMenuItem, InputMethodKind,
            SelectElementOption, SelectElementOptionOrOptgroup, SimpleDialogKind,
        },
        events::{ExplorerKitEvent, HostEvent, LoadStatusKind, PopupRequestPolicy},
        host::{HostCall, HostError, MockHost, PlaceholderHost},
        input::{
            HostInputEvent, KeyboardInputEvent, KeyboardInputKey, KeyboardInputState,
            KeyboardNamedKey, PointerButton, PointerButtonAction, PointerInputEvent,
            PointerScrollMode,
        },
        runtime::{Runtime, SessionHandle},
        surface::{HostSurface, SurfacePoint, SurfaceSize, SurfaceViewport},
        webview::{NavigationRequest, WebViewCommand, WebViewHandle},
    };

    #[test]
    fn facade_exposes_namespaced_modules() {
        let mut runtime = Runtime::new(PlaceholderHost::default());
        let session: SessionHandle = runtime.create_session();
        let webview: WebViewHandle = runtime.create_webview(session).unwrap();
        let surface = HostSurface::new("non-winit-layout-slot");
        let viewport =
            SurfaceViewport::with_origin(SurfacePoint::new(8, 16), SurfaceSize::new(320, 240), 2.0);

        let navigation = NavigationRequest::new("example.com").unwrap();
        let command = WebViewCommand::LoadUrl(navigation.clone());
        let pointer = PointerInputEvent::button(
            PointerButtonAction::Pressed,
            PointerButton::Primary,
            8.0,
            16.0,
        );
        let keyboard = KeyboardInputEvent::new(
            KeyboardInputKey::Named(KeyboardNamedKey::Enter),
            KeyboardInputState::Pressed,
        );
        let input = HostInputEvent::Pointer(pointer);
        let _keyboard_input = HostInputEvent::Keyboard(keyboard);
        let _scroll = PointerInputEvent::wheel(0.0, 1.0, PointerScrollMode::Lines, 8.0, 16.0);
        let _host_call = HostCall::DispatchWebViewCommand {
            webview,
            command: command.clone(),
        };
        let _host_error = HostError::new("boom");
        let _mock: MockHost = PlaceholderHost::default();
        let _dialog = SimpleDialogKind::Prompt;
        let _input_method = InputMethodKind::Text;
        let _context_info = ContextMenuElementInformation {
            is_link: true,
            is_image: false,
            is_editable_text: false,
            has_selection: false,
            link_url: Some("https://example.com/".to_owned()),
            image_url: None,
            context_type: "link".to_owned(),
        };
        let _context_item = ContextMenuItem::Item {
            label: "Copy".to_owned(),
            action: ContextMenuAction::Copy,
            enabled: true,
        };
        let _select_option = SelectElementOptionOrOptgroup::Option(SelectElementOption {
            id: 1,
            label: "One".to_owned(),
            is_disabled: false,
        });

        runtime.load_url(webview, &navigation.url).unwrap();
        runtime
            .attach_surface_with_viewport(webview, surface.clone(), viewport)
            .unwrap();
        runtime.dispatch_input_event(webview, input).unwrap();
        runtime.perform_updates(webview).unwrap();

        let events = runtime.drain_events();
        let _first_event: Option<ExplorerKitEvent> = events.first().cloned();

        assert!(events.iter().any(|event| {
            matches!(
                &event.event,
                HostEvent::LoadStatusChanged { status }
                    if *status == LoadStatusKind::Started
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                &event.event,
                HostEvent::UrlChanged { url } if url == "https://example.com/"
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                &event.event,
                HostEvent::SurfaceAttached { size } if *size == SurfaceSize::new(320, 240)
            )
        }));
    }

    #[test]
    fn facade_keeps_root_happy_path_small_and_usable() {
        let mut runtime = crate::Runtime::new(PlaceholderHost::default());
        let session: crate::SessionHandle = runtime.create_session();
        let webview: crate::WebViewHandle = runtime.create_webview(session).unwrap();
        let surface = crate::HostSurface::new("root-happy-path-surface");
        let viewport = crate::SurfaceViewport::new(crate::SurfaceSize::new(320, 240), 2.0);

        runtime
            .attach_surface_with_viewport(webview, surface, viewport)
            .unwrap();
        runtime
            .dispatch_input_event(
                webview,
                crate::HostInputEvent::Pointer(PointerInputEvent::moved(8.0, 16.0)),
            )
            .unwrap();
        runtime.perform_updates(webview).unwrap();

        let events = runtime.drain_events();
        let _event: Option<crate::ExplorerKitEvent> = events.first().cloned();
        let _result: Result<(), crate::ExplorerKitError> = Ok(());

        assert!(events.iter().any(|event| {
            matches!(
                &event.event,
                crate::HostEvent::SurfaceAttached { size }
                    if *size == crate::SurfaceSize::new(320, 240)
            )
        }));
    }

    #[test]
    fn facade_popup_child_surface_adoption_keeps_child_events_tagged() {
        let child_webview_id = "servo-child-1".to_owned();
        let popup_created = HostEvent::PopupCreated {
            request_id: "request-1".to_owned(),
            parent_webview_id: "servo-parent-1".to_owned(),
            child_webview_id: child_webview_id.clone(),
            parent_url: Some("https://parent.test/".to_owned()),
            target_url: None,
            window_features: None,
            policy: PopupRequestPolicy::ManagedChild,
        };
        let mut runtime = Runtime::new(PlaceholderHost::with_update_events([popup_created]));
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let initial = SurfaceViewport::new(SurfaceSize::new(320, 240), 2.0);
        let resized = SurfaceViewport::new(SurfaceSize::new(480, 320), 2.0);

        runtime.perform_updates(webview).unwrap();
        runtime
            .attach_managed_child_surface(
                webview,
                child_webview_id.clone(),
                HostSurface::new("popup-child-surface"),
                initial,
            )
            .unwrap();
        runtime
            .update_managed_child_surface_viewport(webview, &child_webview_id, resized)
            .unwrap();
        runtime
            .detach_managed_child_surface(webview, &child_webview_id)
            .unwrap();

        let events = runtime.drain_events();
        assert!(events.iter().any(|event| {
            event.managed_child_webview_id.as_deref() == Some(child_webview_id.as_str())
                && matches!(
                    event.event,
                    HostEvent::SurfaceAttached { size } if size == initial.size
                )
        }));
        assert!(events.iter().any(|event| {
            event.managed_child_webview_id.as_deref() == Some(child_webview_id.as_str())
                && matches!(
                    event.event,
                    HostEvent::SurfaceResized { size } if size == resized.size
                )
        }));
        assert!(events.iter().any(|event| {
            event.managed_child_webview_id.as_deref() == Some(child_webview_id.as_str())
                && matches!(event.event, HostEvent::SurfaceDetached)
        }));
        assert!(events.iter().all(|event| {
            event.managed_child_webview_id.is_some()
                || !matches!(
                    event.event,
                    HostEvent::SurfaceAttached { .. }
                        | HostEvent::SurfaceResized { .. }
                        | HostEvent::SurfaceDetached
                )
        }));
    }

    #[cfg(all(
        feature = "servo",
        any(
            target_os = "android",
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        )
    ))]
    #[test]
    fn surface_module_exposes_surface_host_facade_for_non_winit_layouts() {
        use crate::surface::{
            SurfaceDelegate, SurfaceError, SurfaceFrame, SurfaceHost, SurfaceTarget,
        };

        struct LayoutHostServices;

        impl SurfaceDelegate for LayoutHostServices {
            type Frame<'a> = SurfaceFrame<'a>;

            fn render_target(
                &mut self,
                surface: &HostSurface,
                viewport: SurfaceViewport,
            ) -> Result<SurfaceTarget<'_>, SurfaceError> {
                assert_eq!(surface.id(), "gpui-layout-slot");
                assert_eq!(viewport.origin, SurfacePoint::new(8, 16));
                assert_eq!(viewport.size, SurfaceSize::new(320, 240));
                assert_eq!(viewport.scale_factor, 2.0);
                Err(SurfaceError::new(
                    "test host intentionally does not expose native handles",
                ))
            }
        }

        let mut runtime = Runtime::new(SurfaceHost::with_default_options(LayoutHostServices));
        let session = runtime.create_session();
        let webview = runtime.create_webview(session).unwrap();
        let viewport =
            SurfaceViewport::with_origin(SurfacePoint::new(8, 16), SurfaceSize::new(320, 240), 2.0);

        let error = runtime
            .attach_surface_with_viewport(webview, HostSurface::new("gpui-layout-slot"), viewport)
            .expect_err("test service should stop before creating a real Servo target");
        assert!(matches!(error, crate::ExplorerKitError::Host(_)));
    }
}
