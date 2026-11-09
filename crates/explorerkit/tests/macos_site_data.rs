//! Real AppKit acceptance for explicit site-data transfer through the Rust facade.
#![allow(deprecated)]

#[cfg(target_os = "macos")]
mod native {
    use std::{
        cell::{Cell, RefCell},
        collections::HashMap,
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        path::Path,
        process::Command,
        ptr::NonNull,
        rc::Rc,
        time::{Duration, Instant},
    };

    use block2::RcBlock;
    use cocoa::{
        appkit::{
            NSApp, NSApplication, NSBackingStoreBuffered, NSEventMask, NSView, NSWindow,
            NSWindowStyleMask,
        },
        base::{id, nil, NO, YES},
        foundation::{NSAutoreleasePool, NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize},
    };
    use explorerkit::{
        events::HostEvent,
        runtime::{Runtime, SessionHandle},
        site_data::*,
        surface::{
            HostSurface, NativeSurface, SurfaceDelegate, SurfaceError, SurfaceFrame,
            SurfaceHostOptions, SurfaceSize, SurfaceTarget, SurfaceViewport,
        },
        webview::{
            macos::{
                AppKitViewDelegate, MacOsViewHost, MacOsViewHostOptions, MacOsWebViewKind,
                MacOsWebViewOptions, WebKitDataStoreIdentifier,
            },
            WebViewHandle,
        },
    };
    use objc2::{rc::autoreleasepool, AnyThread, MainThreadMarker};
    use objc2_foundation::{NSError, NSUUID};
    use objc2_web_kit::WKWebsiteDataStore;
    use raw_window_handle::{AppKitWindowHandle, DisplayHandle, RawWindowHandle, WindowHandle};

    #[derive(Clone, Default)]
    struct Slots(Rc<RefCell<HashMap<String, id>>>);

    impl SurfaceDelegate for Slots {
        type Frame<'a> = SurfaceFrame<'a>;

        fn render_target(
            &mut self,
            surface: &HostSurface,
            viewport: SurfaceViewport,
        ) -> Result<SurfaceTarget<'_>, SurfaceError> {
            self.appkit_parent(surface, viewport).map(Into::into)
        }
    }

    impl AppKitViewDelegate for Slots {
        fn appkit_parent(
            &mut self,
            surface: &HostSurface,
            _: SurfaceViewport,
        ) -> Result<NativeSurface<'_>, SurfaceError> {
            let slot = self.0.borrow()[surface.id()];
            // The AppKit window retains the slot through runtime destruction.
            Ok(unsafe {
                NativeSurface::new(
                    DisplayHandle::appkit(),
                    WindowHandle::borrow_raw(RawWindowHandle::AppKit(AppKitWindowHandle::new(
                        NonNull::new(slot.cast()).unwrap(),
                    ))),
                )
            })
        }
    }

    struct App {
        runtime: Runtime<MacOsViewHost<Slots>>,
        slots: Slots,
        window: id,
        session: SessionHandle,
        titles: HashMap<WebViewHandle, String>,
        urls: HashMap<WebViewHandle, String>,
    }

    impl App {
        fn new(window: id, uuid: &NSUUID, profile: &Path) -> Self {
            let slots = Slots::default();
            let options = MacOsViewHostOptions::new(
                SurfaceHostOptions::default().with_config_directory(profile),
                WebKitDataStoreIdentifier(uuid.as_bytes()),
            );
            let mut runtime = Runtime::new(MacOsViewHost::new(slots.clone(), options));
            let session = runtime.create_session();
            Self {
                runtime,
                slots,
                window,
                session,
                titles: HashMap::new(),
                urls: HashMap::new(),
            }
        }

        fn create(&mut self, kind: MacOsWebViewKind) -> WebViewHandle {
            self.runtime
                .create_webview_with_options(self.session, MacOsWebViewOptions::new(kind))
                .unwrap()
        }

        fn create_source(&mut self) -> WebViewHandle {
            let options = MacOsWebViewOptions::new(MacOsWebViewKind::SystemWebView)
                .with_webkit_automation(cfg!(feature = "macos-webkit-automation"));
            self.runtime
                .create_webview_with_options(self.session, options)
                .unwrap()
        }

        fn attach(&mut self, view: WebViewHandle) {
            let surface = HostSurface::new(view.raw().to_string());
            let slot = unsafe {
                let slot = NSView::alloc(nil)
                    .initWithFrame_(NSRect::new(NSPoint::new(0., 0.), NSSize::new(320., 240.)))
                    .autorelease();
                slot.setWantsLayer(YES);
                slot.setWantsBestResolutionOpenGLSurface_(YES);
                self.window.contentView().addSubview_(slot);
                slot
            };
            self.slots.0.borrow_mut().insert(surface.id().into(), slot);
            self.runtime
                .attach_surface_with_viewport(
                    view,
                    surface,
                    SurfaceViewport::new(SurfaceSize::new(320, 240), 1.0),
                )
                .unwrap();
        }

        fn pump(&mut self) {
            appkit_turn();
            self.runtime.perform_all_updates().unwrap();
            for event in self.runtime.drain_events() {
                match event.event {
                    HostEvent::NavigationRequested { navigation_id, .. } => {
                        self.runtime
                            .resolve_navigation_request(event.webview, navigation_id, true)
                            .unwrap();
                    }
                    HostEvent::PageTitleChanged { title: Some(title) } => {
                        self.titles.insert(event.webview, title);
                    }
                    HostEvent::UrlChanged { url } => {
                        self.urls.insert(event.webview, url);
                    }
                    HostEvent::Crashed { reason, .. } => panic!("fixture view crashed: {reason}"),
                    _ => {}
                }
            }
        }

        fn navigate(&mut self, view: WebViewHandle, url: &str) -> Vec<String> {
            self.titles.remove(&view);
            self.runtime.load_url(view, url).unwrap();
            wait("fixture navigation", || {
                self.pump();
                let title = self.titles.get(&view)?.strip_prefix("observed:")?;
                Some(serde_json::from_str(title).unwrap())
            })
        }

        fn complete<T>(&mut self, mut operation: SiteDataOperation<T>) -> Result<T, SiteDataError> {
            wait("site-data completion", || {
                self.pump();
                operation.try_take()
            })
        }

        fn capture(
            &mut self,
            view: WebViewHandle,
            selection: SiteDataSelection,
        ) -> SiteDataSnapshot {
            let operation = self.runtime.capture_site_data(view, selection).unwrap();
            self.complete(operation).unwrap()
        }

        fn committed(
            &mut self,
            view: WebViewHandle,
            origin: &SiteDataOrigin,
            expected: &[(&str, &str)],
        ) {
            wait("committed WebKit rows", || {
                let snapshot = self.capture(view, storage_selection(origin.clone()));
                let mut rows = snapshot.local_storage[0]
                    .entries
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .collect::<Vec<_>>();
                rows.sort_unstable();
                (rows == expected).then_some(())
            });
        }
    }

    fn storage_selection(origin: SiteDataOrigin) -> SiteDataSelection {
        SiteDataSelection {
            include_cookies: false,
            local_storage_origins: vec![origin],
        }
    }

    fn wait<T>(label: &str, mut poll: impl FnMut() -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            if let Some(value) = poll() {
                return value;
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "{label} timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn appkit_turn() {
        autoreleasepool(|_| unsafe {
            let app = NSApp();
            loop {
                let event = app.nextEventMatchingMask_untilDate_inMode_dequeue_(
                    NSEventMask::NSAnyEventMask.bits(),
                    NSDate::distantPast(nil),
                    NSDefaultRunLoopMode,
                    YES,
                );
                if event == nil {
                    break;
                }
                app.sendEvent_(event);
            }
        });
    }

    fn fixture_server(port: u16) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut lines = BufReader::new(&stream).lines().map_while(Result::ok);
                let Some(line) = lines.next() else { continue };
                let path = line.split_ascii_whitespace().nth(1).unwrap();
                let mut cookie = String::new();
                for header in lines.take_while(|line| !line.is_empty()) {
                    if let Some((name, value)) = header.split_once(':') {
                        if name.eq_ignore_ascii_case("cookie") {
                            cookie = value.trim().to_owned();
                        }
                    }
                }
                assert_ne!(
                    path, "/__explorerkit_site_data__",
                    "private helper used the network"
                );
                let (seed, headers) = match path {
                    "/account/source" => ("localStorage.setItem('token','source'); localStorage.setItem('getItem','shadow');",
                        "Set-Cookie: auth=source; Path=/account; HttpOnly; Max-Age=3600; SameSite=Lax\r\n"),
                    "/second-source" => ("localStorage.setItem('token','second');", ""),
                    "/replace-source" => (r#"localStorage.setItem('token','雪🧪\u0000'); localStorage.setItem('',''); localStorage.removeItem('getItem');"#, ""),
                    "/retain" => ("localStorage.setItem('retain','destination');", ""),
                    "/change" => ("localStorage.setItem('token','changed');", ""),
                    _ => ("", ""),
                };
                let cookie = serde_json::to_string(&cookie).unwrap();
                let body = format!(
                    "<!doctype html><meta charset=utf-8><script>{seed}document.title = 'observed:' + JSON.stringify([{cookie}, document.cookie, localStorage.getItem('token') ?? '', localStorage.getItem('retain') ?? '', localStorage.getItem('getItem') ?? '']);</script>"
                );
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nCache-Control: no-store\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len());
            }
        });
        port
    }

    fn error_kind<T>(result: Result<T, SiteDataError>) -> SiteDataErrorKind {
        result.err().expect("expected site-data error").kind
    }

    fn exercise(first: &NSUUID, other: &NSUUID, profile: &Path) {
        let first_port = fixture_server(0);
        let second_port = fixture_server(0);
        let main = format!("http://127.0.0.1:{first_port}");
        let second = format!("http://127.0.0.1:{second_port}");
        let missing = format!("http://localhost:{second_port}");
        let origin = |url: &str| SiteDataOrigin::parse(url).unwrap();
        let selection = || SiteDataSelection {
            include_cookies: true,
            local_storage_origins: vec![
                origin(&main),
                origin(&second),
                origin(&missing),
                origin(&main),
            ],
        };

        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let window = NSWindow::alloc(nil).initWithContentRect_styleMask_backing_defer_(
                NSRect::new(NSPoint::new(100., 100.), NSSize::new(320., 240.)),
                NSWindowStyleMask::NSTitledWindowMask,
                NSBackingStoreBuffered,
                NO,
            );
            window.orderFront_(nil);
            let mut app = App::new(window, first, profile);
            let source = app.create_source();
            assert_eq!(
                app.runtime.site_data_scope(source).unwrap(),
                SiteDataScope::HostStore
            );
            assert_eq!(
                error_kind(app.runtime.capture_site_data(source, selection())),
                SiteDataErrorKind::NotReady
            );
            app.attach(source);
            // Validate the selected native store before loading any fixture state.
            for _ in 0..2 {
                let mut operation = app
                    .runtime
                    .capture_site_data(source, SiteDataSelection::default())
                    .unwrap();
                assert!(operation.try_take().unwrap().unwrap().cookies.is_empty());
            }
            let source_url = format!("{main}/account/source");
            assert_eq!(app.navigate(source, &source_url)[2], "source");
            assert_eq!(
                app.navigate(source, &format!("{second}/second-source"))[2],
                "second"
            );
            app.committed(
                source,
                &origin(&main),
                &[("getItem", "shadow"), ("token", "source")],
            );
            app.committed(source, &origin(&second), &[("token", "second")]);
            let untouched = app.urls[&source].clone();

            let mut isolated = App::new(window, other, profile);
            let empty = isolated.create(MacOsWebViewKind::SystemWebView);
            isolated.attach(empty);
            assert!(isolated
                .capture(empty, storage_selection(origin(&main)))
                .local_storage[0]
                .entries
                .is_empty());
            let isolated_snapshot = isolated.capture(empty, selection());
            assert!(isolated_snapshot.cookies.is_empty());
            assert!(isolated_snapshot
                .local_storage
                .iter()
                .all(|entry| entry.entries.is_empty()));
            let operation = isolated
                .runtime
                .capture_site_data(empty, storage_selection(origin(&main)))
                .unwrap();
            isolated.runtime.destroy_webview(empty).unwrap();
            assert_eq!(
                error_kind(isolated.complete(operation)),
                SiteDataErrorKind::OwnerClosed
            );
            drop(isolated); // Keep the process-wide Servo engine available.

            let mut without_cookies = selection();
            without_cookies.include_cookies = false;
            assert!(app.capture(source, without_cookies).cookies.is_empty());
            let mut snapshot = app.capture(source, selection());
            snapshot.local_storage[0].entries.sort();
            assert_eq!(app.urls[&source], untouched);
            assert_eq!(snapshot.local_storage.len(), 3);
            assert_eq!(
                snapshot.local_storage[0].entries,
                [
                    ("getItem".into(), "shadow".into()),
                    ("token".into(), "source".into())
                ]
            );
            assert_eq!(
                snapshot.local_storage[1].entries,
                [("token".into(), "second".into())]
            );
            assert!(snapshot.local_storage[2].entries.is_empty());
            assert_eq!(snapshot.cookies.len(), 1);
            let auth = &snapshot.cookies[0];
            assert_eq!(
                (&*auth.name, &*auth.value, &*auth.path),
                ("auth", "source", "/account")
            );
            assert!(auth.host_only && auth.http_only && auth.expires_unix_seconds.is_some());
            assert_eq!(auth.same_site, Some(SiteDataSameSite::Lax));

            let mut cancelled = app
                .runtime
                .capture_site_data(source, storage_selection(origin(&main)))
                .unwrap();
            cancelled.cancel();
            assert_eq!(
                error_kind(app.complete(cancelled)),
                SiteDataErrorKind::Cancelled
            );

            #[cfg(feature = "servo")]
            servo(&mut app, snapshot, &main, &second);
            #[cfg(not(feature = "servo"))]
            assert!(app
                .runtime
                .create_webview_with_options(
                    app.session,
                    MacOsWebViewOptions::new(MacOsWebViewKind::Servo),
                )
                .is_err());

            let mut retained = app.capture(source, selection());
            retained.local_storage[0].entries.sort();
            assert_eq!(
                retained.local_storage[0].entries,
                [
                    ("getItem".into(), "shadow".into()),
                    ("token".into(), "source".into())
                ]
            );
            assert_eq!(retained.cookies[0].value, "source");
            assert_eq!(app.urls[&source], untouched);

            let page = app.navigate(source, &format!("{main}/replace-source"));
            assert_eq!(page[2], "雪🧪\0");
            assert_eq!(page[4], "");
            // WebKit controls the commit. A capture may see either commit at
            // this boundary, but the final saved state must reflect the delete.
            let mut rows = app
                .capture(source, storage_selection(origin(&main)))
                .local_storage
                .remove(0)
                .entries;
            rows.sort();
            assert!(
                rows == [
                    ("getItem".into(), "shadow".into()),
                    ("token".into(), "source".into())
                ] || rows == [("".into(), "".into()), ("token".into(), "雪🧪\0".into())]
            );
            app.committed(source, &origin(&main), &[("", ""), ("token", "雪🧪\0")]);
            std::fs::write(profile.join("source-port"), first_port.to_string()).unwrap();
            app.runtime.shutdown().unwrap();
            window.close();
            pool.drain();
        }
    }

    fn reopen_saved(first: &NSUUID, profile: &Path) {
        let port: u16 = std::fs::read_to_string(profile.join("source-port"))
            .unwrap()
            .parse()
            .unwrap();
        let origin = format!("http://127.0.0.1:{port}");
        let cold_servo = profile.join("cold-servo");
        std::fs::create_dir(&cold_servo).unwrap();
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let window = NSWindow::alloc(nil).initWithContentRect_styleMask_backing_defer_(
                NSRect::new(NSPoint::new(100., 100.), NSSize::new(320., 240.)),
                NSWindowStyleMask::NSTitledWindowMask,
                NSBackingStoreBuffered,
                NO,
            );
            window.orderFront_(nil);
            let mut app = App::new(window, first, &cold_servo);
            let source = app.create_source();
            app.attach(source);
            let mut snapshot = app.capture(
                source,
                storage_selection(SiteDataOrigin::parse(&origin).unwrap()),
            );
            snapshot.local_storage[0].entries.sort();
            assert_eq!(
                snapshot.local_storage[0].entries,
                [("".into(), "".into()), ("token".into(), "雪🧪\0".into())]
            );
            #[cfg(feature = "servo")]
            {
                let target = app.create(MacOsWebViewKind::Servo);
                app.attach(target);
                let operation = app.runtime.initialize_site_data(target, snapshot).unwrap();
                assert_eq!(app.complete(operation).unwrap().local_storage_entries, 2);
                assert_eq!(fixture_server(port), port);
                assert_eq!(
                    app.navigate(target, &format!("{origin}/account/observe")),
                    ["", "", "雪🧪\0", "", ""]
                );
            }
            app.runtime.shutdown().unwrap();
            window.close();
            pool.drain();
        }
    }

    #[cfg(feature = "servo")]
    fn servo(app: &mut App, mut snapshot: SiteDataSnapshot, main: &str, second: &str) {
        let destination = app.create(MacOsWebViewKind::Servo);
        let sibling = app.create(MacOsWebViewKind::Servo);
        let target = format!("{main}/account/observe");
        assert_eq!(
            app.runtime.site_data_scope(destination).unwrap(),
            SiteDataScope::ProcessStore
        );
        assert_eq!(
            error_kind(
                app.runtime
                    .initialize_site_data(destination, SiteDataSnapshot::default())
            ),
            SiteDataErrorKind::NotReady
        );
        app.attach(destination);
        app.attach(sibling);
        assert_eq!(app.navigate(sibling, &target), ["", "", "", "", ""]);
        app.navigate(sibling, &format!("{main}/retain"));

        let mut invalid = snapshot.cookies[0].clone();
        invalid.name = "invalid".into();
        invalid.expires_unix_seconds = Some(i64::MAX);
        let mut malformed = snapshot.cookies[0].clone();
        malformed.name = "malformed".into();
        malformed.value = "one; injected=two".into();
        for invalid in [invalid, malformed, snapshot.cookies[0].clone()] {
            let invalid_snapshot = SiteDataSnapshot {
                cookies: vec![snapshot.cookies[0].clone(), invalid],
                ..SiteDataSnapshot::default()
            };
            let error = app
                .runtime
                .initialize_site_data(destination, invalid_snapshot)
                .err()
                .unwrap();
            assert_eq!(error.kind, SiteDataErrorKind::InvalidInput);
            assert_eq!(error.mutation, SiteDataMutation::Unchanged);
        }
        assert_eq!(
            app.navigate(sibling, &target),
            ["", "", "", "destination", ""]
        );

        for (name, host_only, secure) in [
            ("host", true, false),
            ("domain", false, false),
            ("secure", false, true),
        ] {
            snapshot.cookies.push(SiteDataCookie {
                host: "login.site-data.test".into(),
                host_only,
                path: "/account".into(),
                name: name.into(),
                value: "fixture".into(),
                secure,
                http_only: false,
                expires_unix_seconds: None,
                same_site: None,
            });
        }
        let mut expired = snapshot.cookies[0].clone();
        expired.name = "expired".into();
        expired.expires_unix_seconds = Some(1);
        snapshot.cookies.push(expired);
        let cancel_cookie = snapshot.cookies[0].clone();
        let operation = app
            .runtime
            .initialize_site_data(destination, snapshot)
            .unwrap();
        assert!(app
            .runtime
            .load_url(destination, &target)
            .unwrap_err()
            .to_string()
            .contains("Busy"));
        assert!(app
            .runtime
            .evaluate_javascript(destination, "try-nav", "location.href = '/account/observe'")
            .unwrap_err()
            .to_string()
            .contains("Busy"));
        let competitor = app.create(MacOsWebViewKind::Servo);
        app.attach(competitor);
        assert_eq!(
            error_kind(
                app.runtime
                    .initialize_site_data(competitor, SiteDataSnapshot::default())
            ),
            SiteDataErrorKind::Busy
        );

        let summary = app.complete(operation).unwrap();
        assert_eq!(
            (summary.cookies_applied, summary.expired_cookies_skipped),
            (4, 1)
        );
        assert_eq!(
            (summary.local_storage_origins, summary.local_storage_entries),
            (3, 3)
        );
        assert_eq!(
            app.navigate(destination, &target),
            ["auth=source", "", "source", "destination", "shadow"]
        );
        assert_eq!(
            app.navigate(sibling, &target),
            ["auth=source", "", "source", "destination", "shadow"]
        );
        assert_eq!(
            app.navigate(destination, &format!("{second}/observe"))[2],
            "second"
        );
        let port = main.rsplit_once(':').unwrap().1;
        let scoped = |host, path| format!("http://{host}:{port}{path}");
        let root = app.navigate(
            destination,
            &scoped("login.site-data.test", "/account/observe"),
        );
        assert!(root[0].contains("host=fixture") && root[0].contains("domain=fixture"));
        assert!(!root[0].contains("secure=fixture"));
        let child = app.navigate(
            destination,
            &scoped("child.login.site-data.test", "/account/observe"),
        );
        assert_eq!(child[0], "domain=fixture");
        assert!(app.navigate(
            destination,
            &scoped("sibling.site-data.test", "/account/observe")
        )[0]
        .is_empty());
        assert!(app.navigate(destination, &scoped("login.site-data.test", "/other"))[0].is_empty());
        app.navigate(destination, &format!("{main}/change"));

        let mut cancelled = app
            .runtime
            .initialize_site_data(
                competitor,
                SiteDataSnapshot {
                    cookies: vec![cancel_cookie],
                    ..SiteDataSnapshot::default()
                },
            )
            .unwrap();
        app.pump(); // Dispatch the first native write before requesting cancellation.
        cancelled.cancel();
        let error = app.complete(cancelled).unwrap_err();
        assert_eq!(error.kind, SiteDataErrorKind::Cancelled);
        assert_eq!(error.mutation, SiteDataMutation::MayHaveChanged);
        assert!(app
            .runtime
            .reload(competitor)
            .unwrap_err()
            .to_string()
            .contains("Busy"));
        app.runtime.destroy_webview(competitor).unwrap();

        let closing = app.create(MacOsWebViewKind::Servo);
        app.attach(closing);
        let operation = app
            .runtime
            .initialize_site_data(closing, SiteDataSnapshot::default())
            .unwrap();
        app.runtime.destroy_webview(closing).unwrap();
        assert_eq!(
            error_kind(app.complete(operation)),
            SiteDataErrorKind::OwnerClosed
        );

        let over_capacity = app.create(MacOsWebViewKind::Servo);
        app.attach(over_capacity);
        let cookies = (0..151)
            .map(|index| SiteDataCookie {
                host: "capacity.site-data.test".into(),
                host_only: true,
                path: "/".into(),
                name: format!("capacity{index}"),
                value: "fixture".into(),
                secure: false,
                http_only: false,
                expires_unix_seconds: None,
                same_site: None,
            })
            .collect();
        let operation = app
            .runtime
            .initialize_site_data(
                over_capacity,
                SiteDataSnapshot {
                    cookies,
                    ..SiteDataSnapshot::default()
                },
            )
            .unwrap();
        let error = app.complete(operation).unwrap_err();
        assert_eq!(error.kind, SiteDataErrorKind::VerificationFailed);
        assert_eq!(error.mutation, SiteDataMutation::MayHaveChanged);
        assert!(app
            .runtime
            .reload(over_capacity)
            .unwrap_err()
            .to_string()
            .contains("Busy"));
        app.runtime.destroy_webview(over_capacity).unwrap();
    }

    pub fn run() {
        let mtm = MainThreadMarker::new().unwrap();
        let arguments = std::env::args().skip(1).collect::<Vec<_>>();
        if let [mode, first, other, profile] = &arguments[..] {
            unsafe { NSApp().finishLaunching() };
            let uuid = |value: &str| {
                NSUUID::initWithUUIDString(
                    NSUUID::alloc(),
                    &objc2_foundation::NSString::from_str(value),
                )
                .unwrap()
            };
            let first = uuid(first);
            let other = uuid(other);
            if mode == "--child" {
                exercise(&first, &other, Path::new(profile));
            } else if mode == "--saved" {
                reopen_saved(&first, Path::new(profile));
            } else {
                assert_eq!(mode, "--cleanup");
                for uuid in [&first, &other] {
                    // Initialize WebKit's main run loop before invoking static cleanup.
                    drop(unsafe { WKWebsiteDataStore::dataStoreForIdentifier(uuid, mtm) });
                    let done = Rc::new(Cell::new(false));
                    let reply = done.clone();
                    let callback = RcBlock::new(move |error: *mut NSError| {
                        assert!(error.is_null(), "synthetic WebKit store cleanup failed");
                        reply.set(true);
                    });
                    unsafe {
                        WKWebsiteDataStore::removeDataStoreForIdentifier_completionHandler(
                            uuid, &callback, mtm,
                        )
                    };
                    wait("WebKit store cleanup", || {
                        appkit_turn();
                        done.get().then_some(())
                    });
                }
            }
            return;
        }
        let first = NSUUID::new().UUIDString().to_string();
        let other = NSUUID::new().UUIDString().to_string();
        let profile = std::env::temp_dir().join(format!("explorerkit-site-data-{first}"));
        std::fs::create_dir(&profile).unwrap();
        let hosts = profile.join("hosts");
        std::fs::write(
            &hosts,
            "127.0.0.1 login.site-data.test child.login.site-data.test sibling.site-data.test\n",
        )
        .unwrap();
        let run = |mode| {
            Command::new(std::env::current_exe().unwrap())
                .arg(mode)
                .arg(&first)
                .arg(&other)
                .arg(&profile)
                .env("HOST_FILE", &hosts)
                .status()
                .unwrap()
        };
        let exercise = run("--child");
        let saved = exercise.success().then(|| run("--saved"));
        let cleanup = run("--cleanup");
        std::fs::remove_dir_all(profile).unwrap();
        assert!(exercise.success(), "native site-data acceptance failed");
        assert!(
            saved.is_some_and(|status| status.success()),
            "cold saved-data acceptance failed"
        );
        assert!(cleanup.success(), "native site-data fixture cleanup failed");
        println!(
            "AppKit site-data acceptance passed (Servo compiled: {})",
            cfg!(feature = "servo")
        );
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    native::run();
}
