use std::{
    ptr::NonNull,
    rc::Rc,
    time::{Duration, Instant},
};

use block2::RcBlock;
use explorerkit::{
    events::{ExplorerKitEvent, HostEvent, LoadStatusKind},
    runtime::{NewWebViewDecision, Runtime},
    surface::{
        HostSurface, NativeSurface, SurfaceDelegate, SurfaceError, SurfaceFrame,
        SurfaceHostOptions, SurfaceSize, SurfaceTarget, SurfaceViewport,
    },
    webview::{
        macos::{
            AppKitNewWebViewDestination, AppKitViewDelegate, MacOsViewHost, MacOsViewHostOptions,
            MacOsWebViewKind, MacOsWebViewOptions, WebKitDataStoreIdentifier,
            WebKitInactiveSchedulingPolicy,
        },
        WebViewHandle,
    },
};
use objc2::{rc::autoreleasepool, rc::Retained, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSEventMask, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDate, NSDefaultRunLoopMode, NSError, NSPoint, NSProcessInfo, NSRect, NSSize, NSString, NSUUID,
};
use objc2_web_kit::{
    WKInactiveSchedulingPolicy, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore,
};
use raw_window_handle::{
    AppKitWindowHandle, DisplayHandle, HandleError, HasWindowHandle, RawWindowHandle, WindowHandle,
};

struct Parent(Retained<NSView>);

impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let view = NonNull::from(&*self.0).cast();
        // SAFETY: Parent retains the AppKit view for the borrowed lifetime.
        Ok(unsafe {
            WindowHandle::borrow_raw(RawWindowHandle::AppKit(AppKitWindowHandle::new(view)))
        })
    }
}

impl SurfaceDelegate for Parent {
    type Frame<'a> = SurfaceFrame<'a>;

    fn render_target(
        &mut self,
        surface: &HostSurface,
        viewport: SurfaceViewport,
    ) -> Result<SurfaceTarget<'_>, SurfaceError> {
        self.appkit_parent(surface, viewport).map(Into::into)
    }
}

impl AppKitViewDelegate for Parent {
    fn appkit_parent(
        &mut self,
        surface: &HostSurface,
        _: SurfaceViewport,
    ) -> Result<NativeSurface<'_>, SurfaceError> {
        if surface.id() == "missing" {
            return Err(SurfaceError::new("fixture slot is unavailable"));
        }
        Ok(NativeSurface::new(
            DisplayHandle::appkit(),
            Parent::window_handle(self).unwrap(),
        ))
    }
}

fn last_native(parent: &NSView) -> Retained<WKWebView> {
    parent
        .subviews()
        .lastObject()
        .expect("attached SDK container")
        .subviews()
        .firstObject()
        .expect("native child view")
        .downcast::<WKWebView>()
        .expect("WKWebView")
}

fn appkit_turn() {
    let mtm = MainThreadMarker::new().unwrap();
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: NSDefaultRunLoopMode is the process-wide AppKit mode constant.
        let mode = unsafe { NSDefaultRunLoopMode };
        if let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&NSDate::distantPast()),
            mode,
            true,
        ) {
            app.sendEvent(&event);
        }
    });
}

fn wait<T>(label: &str, mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        appkit_turn();
        if let Some(value) = poll() {
            return value;
        }
        assert!(Instant::now() < deadline, "{label} timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn await_event<T>(
    runtime: &mut Runtime<MacOsViewHost<Parent>>,
    label: &str,
    mut extract: impl FnMut(ExplorerKitEvent) -> Option<T>,
) -> T {
    wait(label, || {
        runtime.perform_all_updates().unwrap();
        runtime.drain_events().into_iter().find_map(|event| {
            match &event.event {
                HostEvent::Crashed { reason, .. } => panic!("fixture view crashed: {reason}"),
                HostEvent::PopupCreationFailed { error_message, .. } => {
                    panic!("fixture popup failed: {error_message}");
                }
                _ => {}
            }
            extract(event)
        })
    })
}

fn await_load(runtime: &mut Runtime<MacOsViewHost<Parent>>, view: WebViewHandle) {
    await_event(runtime, "document load", |event| {
        (event.webview == view
            && matches!(
                event.event,
                HostEvent::LoadStatusChanged {
                    status: LoadStatusKind::Complete
                }
            ))
        .then_some(())
    });
}

fn evaluate(
    runtime: &mut Runtime<MacOsViewHost<Parent>>,
    view: WebViewHandle,
    script: &str,
) -> serde_json::Value {
    runtime.evaluate_javascript(view, "check", script).unwrap();
    await_event(runtime, "JavaScript result", |event| match event.event {
        HostEvent::JavaScriptEvaluationResult {
            evaluation_id,
            ok,
            value_json,
            error_type,
        } if event.webview == view && evaluation_id == "check" => {
            assert!(ok, "{script}: {error_type:?}");
            let result: serde_json::Value = serde_json::from_str(&value_json.unwrap()).unwrap();
            Some(result["value"].clone())
        }
        _ => None,
    })
}

fn exercise_snapshot(
    runtime: &mut Runtime<MacOsViewHost<Parent>>,
    session: explorerkit::SessionHandle,
    viewport: SurfaceViewport,
) {
    use base64::Engine;
    use explorerkit::{runtime::RuntimeError, webview::macos::WebKitSnapshot};

    fn verify(snapshot: WebKitSnapshot, width: u32) {
        let height = (width as f64 * 0.75).round() as u32;
        assert_eq!((snapshot.width, snapshot.height), (width, height));
        assert_eq!(snapshot.rgba.len(), (width * height * 4) as usize);
        for (x, y, expected) in [
            (width / 4, height / 4, [255, 0, 0, 255]),
            (3 * width / 4, height / 4, [0, 255, 0, 255]),
            (width / 4, 3 * height / 4, [0, 0, 255, 255]),
            (3 * width / 4, 3 * height / 4, [255, 255, 0, 255]),
        ] {
            let index = ((y * width + x) * 4) as usize;
            let pixel = &snapshot.rgba[index..index + 4];
            assert!(
                pixel.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 3),
                "unexpected snapshot pixel at {x},{y}: {pixel:?}"
            );
        }
    }

    let view = runtime
        .create_webview_with_options(
            session,
            MacOsWebViewOptions::new(MacOsWebViewKind::SystemWebView),
        )
        .unwrap();
    assert_eq!(
        runtime.snapshot_system_webview(view, 960).unwrap_err(),
        RuntimeError::SurfaceNotAttached(view)
    );
    runtime
        .attach_surface_with_viewport(view, HostSurface::new("snapshot"), viewport)
        .unwrap();
    assert!(runtime.snapshot_system_webview(view, 0).is_err());
    let html = r#"<!doctype html><title>ExplorerKit snapshot fixture</title><style>
        html,body{width:100%;height:100%;margin:0}
        body{display:grid;grid-template:1fr 1fr/1fr 1fr}
        i:nth-child(1){background:red} i:nth-child(2){background:lime}
        i:nth-child(3){background:blue} i:nth-child(4){background:yellow}
        </style><i></i><i></i><i></i><i></i>"#;
    let encoded = base64::engine::general_purpose::STANDARD.encode(html);
    runtime
        .load_url(view, format!("data:text/html;base64,{encoded}"))
        .unwrap();
    assert!(runtime.snapshot_system_webview(view, u32::MAX).is_err());
    await_event(runtime, "snapshot fixture document", |event| {
        (event.webview == view
            && matches!(event.event, HostEvent::PageTitleChanged { title: Some(title) }
                if title == "ExplorerKit snapshot fixture"))
        .then_some(())
    });
    let ready = evaluate(runtime, view, "document.querySelectorAll('i').length");
    assert_eq!(ready.as_f64(), Some(4.0));
    for width in [960, 959] {
        let mut request = runtime.snapshot_system_webview(view, width).unwrap();
        verify(
            wait("responsive WebKit snapshot", || request.try_recv().unwrap()).unwrap(),
            width,
        );
    }
    let mut tiny = runtime.snapshot_system_webview(view, 1).unwrap();
    let tiny = wait("one-pixel WebKit snapshot", || tiny.try_recv().unwrap()).unwrap();
    assert_eq!((tiny.width, tiny.height, tiny.rgba.len()), (1, 1, 4));

    runtime
        .evaluate_javascript(
            view,
            "snapshot-stall",
            "const end = performance.now() + 1100; while (performance.now() < end) {}",
        )
        .unwrap();
    let start = Instant::now();
    let mut request = runtime.snapshot_system_webview(view, 960).unwrap();
    while start.elapsed() < Duration::from_millis(250) {
        appkit_turn();
        assert!(
            request.try_recv().unwrap().is_none(),
            "snapshot completed during the WebContent stall"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    verify(
        wait("WebKit snapshot after WebContent recovers", || {
            request.try_recv().unwrap()
        })
        .unwrap(),
        960,
    );
    println!(
        "WebKit snapshot pixels and recovery passed (stall completion after {:?})",
        start.elapsed()
    );

    runtime.detach_surface(view).unwrap();
    assert_eq!(
        runtime.snapshot_system_webview(view, 960).unwrap_err(),
        RuntimeError::SurfaceNotAttached(view)
    );
    runtime.destroy_webview(view).unwrap();
    assert_eq!(
        runtime.snapshot_system_webview(view, 960).unwrap_err(),
        RuntimeError::UnknownWebView(view)
    );
}

fn assert_configuration(view: &WKWebView, store: &NSUUID, expected: WKInactiveSchedulingPolicy) {
    // SAFETY: These objects belong to the process main thread, on macOS 14 or later.
    unsafe {
        let configuration = view.configuration();
        assert_eq!(
            configuration.preferences().inactiveSchedulingPolicy(),
            expected
        );
        assert_eq!(
            configuration.websiteDataStore().identifier().as_deref(),
            Some(store)
        );
    }
}

fn exercise(store: &NSUUID, mtm: MainThreadMarker) {
    NSApplication::sharedApplication(mtm).finishLaunching();
    // SAFETY: This window belongs to the process main thread and its Retained owns the release.
    let window = unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(100., 100.), NSSize::new(320., 240.)),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setReleasedWhenClosed(false);
        window
    };
    window.orderFront(None);
    let parent = window.contentView().unwrap();
    let viewport = SurfaceViewport::new(SurfaceSize::new(320, 240), 1.0);
    let destination = Rc::new(Parent(parent.clone()));
    let options = MacOsViewHostOptions::new(
        SurfaceHostOptions::default(),
        WebKitDataStoreIdentifier(store.as_bytes()),
    )
    .with_new_webview_destination_handler(Rc::new(move |_| {
        Ok(AppKitNewWebViewDestination::new(
            HostSurface::new("popup"),
            viewport,
            destination.clone(),
        ))
    }));
    let mut runtime = Runtime::new(MacOsViewHost::new(Parent(parent.clone()), options));
    let session = runtime.create_session();
    let root_options = || MacOsWebViewOptions::new(MacOsWebViewKind::SystemWebView);
    use WebKitInactiveSchedulingPolicy::{Suspend, Throttle};

    exercise_snapshot(&mut runtime, session, viewport);

    if NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        < 14
    {
        let error = runtime
            .create_webview_with_options(
                session,
                root_options().with_webkit_inactive_scheduling_policy(Throttle),
            )
            .unwrap_err();
        assert!(error.to_string().contains("requires macOS 14 or later"));
        let default = runtime
            .create_webview_with_options(session, root_options())
            .unwrap();
        runtime.destroy_webview(default).unwrap();
    } else {
        // SAFETY: Read WebKit's untouched default on its owning thread.
        let native_default = unsafe {
            WKWebViewConfiguration::new(mtm)
                .preferences()
                .inactiveSchedulingPolicy()
        };
        let mut roots = Vec::new();
        for (index, (setting, expected)) in [
            (Some(Throttle), WKInactiveSchedulingPolicy::Throttle),
            (
                Some(WebKitInactiveSchedulingPolicy::None),
                WKInactiveSchedulingPolicy::None,
            ),
            (Some(Suspend), WKInactiveSchedulingPolicy::Suspend),
            (None, native_default),
        ]
        .into_iter()
        .enumerate()
        {
            let mut options = root_options();
            if let Some(setting) = setting {
                options = options.with_webkit_inactive_scheduling_policy(setting);
            }
            if index == 0 {
                options = options.with_webkit_automation(cfg!(feature = "macos-webkit-automation"));
            }
            let view = runtime
                .create_webview_with_options(session, options)
                .unwrap();
            if index == 0 {
                runtime
                    .load_url(view, "data:text/html,<title>Root</title>")
                    .unwrap();
                let result = runtime.attach_surface_with_viewport(
                    view,
                    HostSurface::new("missing"),
                    viewport,
                );
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("fixture slot is unavailable"));
            }
            runtime
                .attach_surface_with_viewport(
                    view,
                    HostSurface::new(format!("root-{index}")),
                    viewport,
                )
                .unwrap();
            let native = last_native(&parent);
            assert_configuration(&native, store, expected);
            roots.push((view, native, expected));
        }

        let (root, native, expected) = &roots[0];
        await_load(&mut runtime, *root);
        assert_eq!(
            evaluate(
                &mut runtime,
                *root,
                r#"
            window.nonce = "retained";
            document.body.innerHTML = '<input id="draft">';
            document.querySelector('#draft').value = "edit";
            window.nonce;
        "#
            ),
            "retained"
        );

        runtime
            .set_new_webview_handler(*root, Some(Rc::new(|_| NewWebViewDecision::Create)))
            .unwrap();
        runtime
            .evaluate_javascript(
                *root,
                "open",
                "Boolean(window.open('about:blank', 'child'))",
            )
            .unwrap();
        let popup = await_event(&mut runtime, "native popup", |event| {
            if event.webview == *root && matches!(event.event, HostEvent::PopupCreated { .. }) {
                event.created_webview
            } else {
                None
            }
        });
        assert_configuration(&last_native(&parent), store, *expected);
        runtime
            .load_url(popup, "data:text/html,<title>Child</title>")
            .unwrap();
        await_load(&mut runtime, popup);
        assert_eq!(
            evaluate(&mut runtime, popup, "window.opener !== null"),
            true
        );

        runtime.detach_surface(*root).unwrap();
        assert!(native.window().is_none());
        runtime
            .attach_surface_with_viewport(*root, HostSurface::new("reattach"), viewport)
            .unwrap();
        assert!(std::ptr::eq(&*last_native(&parent), &**native));
        assert_eq!(
            evaluate(
                &mut runtime,
                *root,
                r#"window.nonce + ":" + document.querySelector('#draft').value"#
            ),
            "retained:edit"
        );
        for (_, native, expected) in &roots {
            assert_configuration(native, store, *expected);
        }

        runtime.destroy_webview(popup).unwrap();
    }

    drop(runtime);
    window.close();
}

pub(super) fn run_child(mtm: MainThreadMarker, uuid: &str) {
    let uuid = NSUUID::initWithUUIDString(NSUUID::alloc(), &NSString::from_str(uuid)).unwrap();
    exercise(&uuid, mtm);
}

pub(super) fn run(mtm: MainThreadMarker) {
    let uuid = NSUUID::new();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--inactive-scheduling-child")
        .arg(uuid.UUIDString().to_string())
        .status()
        .unwrap();
    if NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        >= 14
    {
        NSApplication::sharedApplication(mtm).finishLaunching();
        // SAFETY: Only this test's fresh store is created and removed on the main thread.
        unsafe {
            drop(WKWebsiteDataStore::dataStoreForIdentifier(&uuid, mtm));
            let done = Rc::new(std::cell::Cell::new(false));
            let completed = done.clone();
            let reply = RcBlock::new(move |error: *mut NSError| {
                assert!(error.is_null(), "fixture store cleanup failed");
                completed.set(true);
            });
            WKWebsiteDataStore::removeDataStoreForIdentifier_completionHandler(&uuid, &reply, mtm);
            wait("fixture store cleanup", || done.get().then_some(()));
        }
    }
    assert!(
        status.success(),
        "WebKit inactive scheduling fixture failed"
    );
    println!("WebKit inactive scheduling passed");
}
