//! Guarded system-WebKit SPI. Standard command semantics remain native.
use super::{dialogs, HostError, State, WebKitAutomationContext};
use block2::{DynBlock, RcBlock};
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyClass, Sel},
    sel, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};
#[allow(deprecated)]
use objc2_web_kit::{WKProcessPool, WKUIDelegate, WKWebView, WKWebViewConfiguration};
use serde_json::json;
use std::{
    cell::Cell,
    ffi::{c_void, CStr},
    rc::{Rc, Weak},
};

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, name: *const std::ffi::c_char) -> *mut c_void;
}
type SetControlled = unsafe extern "C" fn(*const c_void, bool);
fn setter() -> Option<SetControlled> {
    // SAFETY: This exact WebKit C API has (WKPageRef, bool) ABI. Resolve instead
    // of weak-linking a private symbol so ordinary browsing remains available.
    let symbol = unsafe {
        dlsym(
            (-2isize) as *mut c_void,
            c"WKPageSetControlledByAutomation".as_ptr(),
        )
    };
    (!symbol.is_null())
        .then(|| unsafe { std::mem::transmute::<*mut c_void, SetControlled>(symbol) })
}
pub(in crate::webview::macos) fn check_available() -> Result<(), HostError> {
    MainThreadMarker::new()
        .ok_or_else(|| HostError::new("WebKit automation requires AppKit's main thread"))?;
    let required: &[(&CStr, &[Sel])] = &[
        (
            c"_WKAutomationSession",
            &[
                sel!(initWithConfiguration:),
                sel!(setSessionIdentifier:),
                sel!(setDelegate:),
                sel!(_dispatchMessageFromRemoteForTesting:),
                sel!(_setMessageToFrontendHandlerForTesting:),
                sel!(_registerWebViewForTesting:),
            ],
        ),
        (
            c"_WKAutomationSessionConfiguration",
            &[sel!(init), sel!(setAcceptInsecureCertificates:)],
        ),
        (c"WKProcessPool", &[sel!(_setAutomationSession:)]),
        (c"WKWebView", &[sel!(_pageForTesting)]),
        (
            c"WKWebViewConfiguration",
            &[sel!(_setControlledByAutomation:)],
        ),
    ];
    for (name, methods) in required {
        let class = AnyClass::get(name).ok_or_else(|| {
            HostError::new(format!(
                "WebKit automation SPI unavailable: {}",
                name.to_string_lossy()
            ))
        })?;
        for method in *methods {
            if class.instance_method(*method).is_none() {
                return Err(HostError::new(format!(
                    "WebKit automation SPI unavailable: {method}"
                )));
            }
        }
    }
    setter().ok_or_else(|| HostError::new("WKPageSetControlledByAutomation is unavailable"))?;
    Ok(())
}
pub(in crate::webview::macos) fn configure(configuration: &WKWebViewConfiguration, enabled: bool) {
    // SAFETY: Only called for enabled views after the SPI preflight.
    unsafe {
        let _: () = msg_send![configuration, _setControlledByAutomation: enabled];
    }
}
pub(in crate::webview::macos) fn set_eligible(view: &WKWebView, enabled: bool) {
    unsafe {
        let page: *const c_void = msg_send![view, _pageForTesting];
        setter().expect("SPI was checked")(page, enabled);
    }
}
pub(in crate::webview::macos) fn retire(view: &WKWebView) {
    // Reuse the SDK's existing native-close sink/registrar, never fabricate a
    // protocol reply. The host pump drops the native owner before later dispatch.
    unsafe {
        if let Some(delegate) = view.UIDelegate() {
            delegate.webViewDidClose(view);
        }
    }
}
pub(in crate::webview::macos) fn version() -> String {
    unsafe {
        let bundle_class = AnyClass::get(c"NSBundle").expect("Foundation NSBundle");
        let bundle: Retained<NSObject> =
            msg_send![bundle_class, bundleForClass: WKWebView::class()];
        let value: Option<Retained<NSString>> = msg_send![&*bundle, objectForInfoDictionaryKey: &*NSString::from_str("CFBundleVersion")];
        value.map(|v| v.to_string()).unwrap_or_default()
    }
}

pub(in crate::webview::macos) struct DelegateState {
    owner: Weak<State>,
    candidate: Option<Retained<WKWebView>>,
    consumed: Cell<bool>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateState]
    struct AutomationDelegate;
    unsafe impl NSObjectProtocol for AutomationDelegate {
        #[unsafe(method(respondsToSelector:))]
        fn responds(&self, selector: Sel) -> bool {
            if selector == sel!(_automationSession:requestNewWebViewWithOptions:completionHandler:) {
                self.ivars().candidate.is_some()
            } else { unsafe { msg_send![super(self), respondsToSelector: selector] } }
        }
    }
    impl AutomationDelegate {
        #[unsafe(method(_automationSession:requestNewWebViewWithOptions:completionHandler:))]
        fn create(&self, _: &NSObject, _: usize, completion: &DynBlock<dyn Fn(*mut WKWebView)>) {
            // The selector is advertised only with a retained real candidate.
            // Fallible work and all Rust registry borrows ended before dispatch.
            let view = self.ivars().candidate.as_ref().expect("prepared native candidate");
            self.ivars().consumed.set(true);
            completion.call((Retained::as_ptr(view).cast_mut(),));
        }
        #[unsafe(method(_automationSession:requestSwitchToWebView:completionHandler:))]
        fn select(&self, _: &NSObject, view: &WKWebView, completion: &DynBlock<dyn Fn()>) {
            if let Some(owner) = self.ivars().owner.upgrade() {
                let handle = owner.sink_for_native(view).and_then(|sink| owner.callbacks.borrow().views.get(&sink.target).and_then(|state| state.automation.handle));
                if let (Some(handler), Some(handle)) = (&owner.selection, handle) { handler(handle); }
            }
            completion.call(());
        }
        #[unsafe(method(_automationSessionDidDisconnectFromRemote:))]
        fn disconnected(&self, _: &NSObject) { if let Some(owner) = self.ivars().owner.upgrade() { owner.native_disconnected(); } }
        #[unsafe(method(_automationSession:isShowingJavaScriptDialogForWebView:))]
        fn showing(&self, _: &NSObject, view: &WKWebView) -> bool { self.dialog(view).is_some() }
        #[unsafe(method(_automationSession:typeOfCurrentJavaScriptDialogForWebView:))]
        fn dialog_type(&self, _: &NSObject, view: &WKWebView) -> isize {
            use explorerkit_embedder::SimpleDialogKind::*;
            self.dialog(view).map(|d| match d.kind { Alert => 2, Confirm => 3, Prompt => 4 }).unwrap_or(1)
        }
        #[unsafe(method_id(_automationSession:messageOfCurrentJavaScriptDialogForWebView:))]
        fn dialog_message(&self, _: &NSObject, view: &WKWebView) -> Option<Retained<NSString>> { self.dialog(view).map(|d| NSString::from_str(&d.message)) }
        #[unsafe(method_id(_automationSession:defaultTextOfCurrentJavaScriptDialogForWebView:))]
        fn default_text(&self, _: &NSObject, view: &WKWebView) -> Option<Retained<NSString>> { self.dialog(view).and_then(|d| d.default_value.map(|v| NSString::from_str(&v))) }
        #[unsafe(method_id(_automationSession:userInputOfCurrentJavaScriptDialogForWebView:))]
        fn user_input(&self, _: &NSObject, view: &WKWebView) -> Option<Retained<NSString>> {
            self.with_sink(view, |sink| dialogs::current_input(sink).map(|s| NSString::from_str(&s))).flatten()
        }
        #[unsafe(method(_automationSession:setUserInput:forCurrentJavaScriptDialogForWebView:))]
        fn set_input(&self, _: &NSObject, input: &NSString, view: &WKWebView) { self.with_sink(view, |sink| dialogs::set_input(sink, input.to_string())); }
        #[unsafe(method(_automationSession:acceptCurrentJavaScriptDialogForWebView:))]
        fn accept(&self, _: &NSObject, view: &WKWebView) { self.answer(view, true); }
        #[unsafe(method(_automationSession:dismissCurrentJavaScriptDialogForWebView:))]
        fn dismiss(&self, _: &NSObject, view: &WKWebView) { self.answer(view, false); }
    }
);
impl AutomationDelegate {
    fn new(owner: &Rc<State>, candidate: Option<&WKWebView>) -> Retained<Self> {
        let this =
            Self::alloc(MainThreadMarker::new().expect("AppKit thread")).set_ivars(DelegateState {
                owner: Rc::downgrade(owner),
                candidate: candidate.map(|v| unsafe {
                    Retained::retain(v as *const WKWebView as *mut WKWebView).unwrap()
                }),
                consumed: Cell::new(false),
            });
        unsafe { msg_send![super(this), init] }
    }
    fn with_sink<T>(
        &self,
        view: &WKWebView,
        f: impl FnOnce(&super::SystemCallbackSink) -> T,
    ) -> Option<T> {
        let owner = self.ivars().owner.upgrade()?;
        let sink = owner.sink_for_native(view)?;
        Some(f(&sink))
    }
    fn dialog(&self, view: &WKWebView) -> Option<explorerkit_embedder::SimpleDialogRequest> {
        self.with_sink(view, dialogs::pending).flatten()
    }
    fn answer(&self, view: &WKWebView, confirmed: bool) {
        self.with_sink(view, |sink| {
            if let Some(dialog) = dialogs::pending(sink) {
                let input = dialogs::current_input(sink);
                let _ = dialogs::resolve(sink, &dialog.dialog_id, confirmed, input.as_deref());
            }
        });
    }
}

#[allow(deprecated)]
pub(in crate::webview::macos) struct Session {
    native: Retained<NSObject>,
    pool: Retained<WKProcessPool>,
    delegate: Retained<AutomationDelegate>,
    owner: Weak<State>,
    pub(in crate::webview::macos) id: String,
}
#[allow(deprecated)]
impl Session {
    pub(in crate::webview::macos) fn new(
        owner: &Rc<State>,
        view: &WKWebView,
        id: &str,
        insecure: bool,
    ) -> Result<Self, HostError> {
        unsafe {
            let config_class = AnyClass::get(c"_WKAutomationSessionConfiguration").unwrap();
            let configuration: Retained<NSObject> = msg_send![config_class, new];
            let _: () = msg_send![&*configuration, setAcceptInsecureCertificates: insecure];
            let session_class = AnyClass::get(c"_WKAutomationSession").unwrap();
            let allocated: objc2::rc::Allocated<NSObject> = msg_send![session_class, alloc];
            let native: Retained<NSObject> =
                msg_send![allocated, initWithConfiguration: &*configuration];
            let _: () = msg_send![&*native, setSessionIdentifier: &*NSString::from_str(id)];
            let delegate = AutomationDelegate::new(owner, None);
            let _: () = msg_send![&*native, setDelegate: &*delegate];
            let pool = view.configuration().processPool();
            let _: () = msg_send![&*pool, _setAutomationSession: &*native];
            Ok(Self {
                native,
                pool,
                delegate,
                owner: Rc::downgrade(owner),
                id: id.into(),
            })
        }
    }
    pub(in crate::webview::macos) fn same_pool(&self, view: &WKWebView) -> bool {
        unsafe { std::ptr::eq(&*view.configuration().processPool(), &*self.pool) }
    }
    pub(in crate::webview::macos) fn register(&self, view: &WKWebView) -> WebKitAutomationContext {
        let handle: Retained<NSString> =
            unsafe { msg_send![&*self.native, _registerWebViewForTesting: view] };
        WebKitAutomationContext {
            session_id: self.id.clone(),
            handle: handle.to_string(),
        }
    }
    pub(in crate::webview::macos) fn pair(&self, owner: &Rc<State>) {
        let weak = Rc::downgrade(owner);
        let epoch = owner.data.borrow().epoch;
        let handler = RcBlock::new(move |message: *mut NSString| {
            if let Some(owner) = weak.upgrade().filter(|owner| owner.is_epoch(epoch)) {
                let message = unsafe { &*message }.to_string();
                owner.send(json!({"event":"SendMessageToFrontend","connectionID":1,"targetID":1,"message":message}));
            }
        });
        unsafe {
            let _: () = msg_send![&*self.native, _setMessageToFrontendHandlerForTesting: &*handler];
        }
    }
    pub(in crate::webview::macos) fn dispatch(
        &self,
        original: &str,
        candidate: Option<&WKWebView>,
    ) -> bool {
        let prepared = candidate.and_then(|view| {
            self.owner
                .upgrade()
                .map(|owner| AutomationDelegate::new(&owner, Some(view)))
        });
        unsafe {
            if let Some(delegate) = &prepared {
                let _: () = msg_send![&*self.native, setDelegate: &**delegate];
            }
            let _: () = msg_send![&*self.native, _dispatchMessageFromRemoteForTesting: &*NSString::from_str(original)];
            // Refresh cached selector flags only after native entry returned.
            if prepared.is_some() {
                let _: () = msg_send![&*self.native, setDelegate: &*self.delegate];
            }
        }
        prepared.is_some_and(|delegate| delegate.ivars().consumed.get())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![&*self.native, _setMessageToFrontendHandlerForTesting: std::ptr::null::<DynBlock<dyn Fn(*mut NSString)>>()];
            let _: () = msg_send![&*self.pool, _setAutomationSession: Option::<&NSObject>::None];
            let _: () = msg_send![&*self.native, setDelegate: Option::<&NSObject>::None];
        }
    }
}
