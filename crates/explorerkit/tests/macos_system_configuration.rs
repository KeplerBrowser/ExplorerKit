#[cfg(target_os = "macos")]
#[path = "../src/webview/macos/configuration.rs"]
mod configuration;

#[cfg(target_os = "macos")]
#[path = "macos_system_configuration/scheduling.rs"]
mod scheduling;

#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn main() {
    use objc2::{rc::autoreleasepool, MainThreadMarker};
    use objc2_web_kit::{WKWebViewConfiguration, WKWebsiteDataStore};

    let main_thread =
        MainThreadMarker::new().expect("native configuration test runs on main thread");
    autoreleasepool(|_| {
        let mut args = std::env::args().skip(1);
        if args.next().as_deref() == Some("--inactive-scheduling-child") {
            scheduling::run_child(main_thread, &args.next().expect("fixture store identifier"));
            return;
        }
        // SAFETY: The test is on the process main thread.
        let first = unsafe { WKWebViewConfiguration::new(main_thread) };
        // SAFETY: Only an ephemeral store is used, on the process main thread.
        unsafe {
            first.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(main_thread))
        };
        let context = configuration::SystemWebViewContext::from_configuration(&first);
        let second = context.configuration();
        let third = context.configuration();
        assert!(!std::ptr::eq(&*first, &*second));
        assert!(!std::ptr::eq(&*second, &*third));
        // SAFETY: These native objects are all retained on the process main thread.
        unsafe {
            assert!(std::ptr::eq(&*first.processPool(), &*second.processPool()));
            assert!(std::ptr::eq(&*second.processPool(), &*third.processPool()));
            assert!(std::ptr::eq(
                &*first.websiteDataStore(),
                &*second.websiteDataStore()
            ));
            assert!(!std::ptr::eq(&*first.preferences(), &*second.preferences()));
            assert!(!std::ptr::eq(&*second.preferences(), &*third.preferences()));
            assert!(!std::ptr::eq(
                &*first.userContentController(),
                &*second.userContentController()
            ));
            assert!(!std::ptr::eq(
                &*second.userContentController(),
                &*third.userContentController()
            ));
        }
        scheduling::run(main_thread);
    });
}

#[cfg(not(target_os = "macos"))]
fn main() {}
