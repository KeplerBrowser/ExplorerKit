//! Native state notifications for one concrete AppKit view lifetime.

use std::ffi::c_void;

use objc2::{
    define_class, msg_send,
    rc::{Retained, Weak},
    runtime::AnyObject,
    DefinedClass, MainThreadOnly,
};
use objc2_foundation::{
    NSDictionary, NSKeyValueChangeKey, NSKeyValueObservingOptions, NSObject,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSString,
};
use objc2_web_kit::WKWebView;
use wry::{WebView, WebViewExtMacOS};

use super::SystemCallbackSink;

pub(super) struct Observation {
    sink: SystemCallbackSink,
    // WebKit can retain callback objects; the sink and observer never own the view.
    view: Weak<WKWebView>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Observation]
    pub(super) struct Observer;

    unsafe impl NSObjectProtocol for Observer {}

    impl Observer {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe(
            &self,
            _key: Option<&NSString>,
            _object: Option<&AnyObject>,
            _change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
            _context: *mut c_void,
        ) {
            let state = self.ivars();
            if !state.sink.is_live() {
                return;
            }
            let Some(view) = state.view.load() else { return; };
            // SAFETY: WKWebView declares these properties KVO compliant. Both
            // prior and post notifications run on its AppKit owning thread.
            let (url, navigation) = unsafe {
                (
                    view.URL().and_then(|url| url.absoluteString()).map(|url| url.to_string()),
                    (view.canGoBack(), view.canGoForward()),
                )
            };
            // Reconcile synchronously, before a later commit can change lineage.
            // Publication releases the sink borrow before issuing a deferred wake.
            state.sink.native_state_observed(url, navigation);
        }
    }
);

const KEYS: [&str; 4] = ["URL", "loading", "canGoBack", "canGoForward"];

pub(super) struct Registration {
    observer: Retained<Observer>,
    view: Retained<WKWebView>,
}

impl Registration {
    pub(super) fn attach(native: &WebView, sink: SystemCallbackSink) -> Self {
        let view = native.webview().into_super();
        let observer = Observer::alloc(view.mtm()).set_ivars(Observation {
            sink,
            view: Weak::new(&view),
        });
        // SAFETY: NSObject initialization for our main-thread observer class.
        let observer: Retained<Observer> = unsafe { msg_send![super(observer), init] };
        let options = NSKeyValueObservingOptions::Initial
            | NSKeyValueObservingOptions::New
            | NSKeyValueObservingOptions::Prior;
        for key in KEYS {
            // SAFETY: these public WKWebView properties support KVO. The host
            // retains both objects until the matching owning-thread removal.
            unsafe {
                view.addObserver_forKeyPath_options_context(
                    &observer,
                    &NSString::from_str(key),
                    options,
                    std::ptr::null_mut(),
                );
            }
        }
        Self { observer, view }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        for key in KEYS {
            // SAFETY: each registration is removed once while both objects live,
            // before the host releases the native owner; detach keeps this owner.
            unsafe {
                self.view
                    .removeObserver_forKeyPath(&self.observer, &NSString::from_str(key));
            }
        }
    }
}
