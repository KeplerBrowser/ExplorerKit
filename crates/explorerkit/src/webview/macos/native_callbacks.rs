//! Per-view native child callbacks, retaining Wry's existing UI responsibilities.

use std::cell::Cell;

use block2::DynBlock;
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{ProtocolObject, Sel},
    sel, DefinedClass, MainThreadOnly,
};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSURL};
use objc2_web_kit::{
    WKFrameInfo, WKMediaCaptureType, WKNavigationAction, WKOpenPanelParameters,
    WKPermissionDecision, WKSecurityOrigin, WKUIDelegate, WKWebView, WKWebViewConfiguration,
    WKWindowFeatures,
};
use wry::{WebView, WebViewExtMacOS};

use super::{HostError, SystemNewWebView};

pub(super) struct DelegateState {
    original: Retained<ProtocolObject<dyn WKUIDelegate>>,
    callbacks: SystemNewWebView,
    native_identity: usize,
    depth: Cell<usize>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateState]
    pub(super) struct SystemUiDelegate;

    unsafe impl NSObjectProtocol for SystemUiDelegate {
        #[unsafe(method(respondsToSelector:))]
        fn responds_to_selector(&self, selector: Sel) -> bool {
            if selector
                == sel!(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:)
                || selector
                    == sel!(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:)
            {
                self.ivars().original.respondsToSelector(selector)
            } else {
                // SAFETY: Preserve NSObject's advertisement for our own methods.
                unsafe { msg_send![super(self), respondsToSelector: selector] }
            }
        }
    }

    unsafe impl WKUIDelegate for SystemUiDelegate {
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create(
            &self,
            view: &WKWebView,
            configuration: &WKWebViewConfiguration,
            action: &WKNavigationAction,
            features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            let _callback = self.enter();
            if view as *const WKWebView as usize == self.ivars().native_identity {
                self.ivars()
                    .callbacks
                    .request(view, configuration, action, features)
            } else {
                None
            }
        }

        #[unsafe(method(webViewDidClose:))]
        fn closed(&self, view: &WKWebView) {
            let _callback = self.enter();
            if view as *const WKWebView as usize == self.ivars().native_identity {
                self.ivars().callbacks.closed();
            }
        }

        #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
        fn open_panel(
            &self,
            view: &WKWebView,
            parameters: &WKOpenPanelParameters,
            frame: &WKFrameInfo,
            completion: &DynBlock<dyn Fn(*mut NSArray<NSURL>)>,
        ) {
            let _callback = self.enter();
            // SAFETY: Advertised only when Wry implements this exact native method.
            // Forward the original arguments and block without changing its answer.
            unsafe {
                self.ivars()
                    .original
                    .webView_runOpenPanelWithParameters_initiatedByFrame_completionHandler(
                        view, parameters, frame, completion,
                    );
            }
        }

        #[unsafe(method(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:))]
        fn media_permission(
            &self,
            view: &WKWebView,
            origin: &WKSecurityOrigin,
            frame: &WKFrameInfo,
            kind: WKMediaCaptureType,
            completion: &DynBlock<dyn Fn(WKPermissionDecision)>,
        ) {
            let _callback = self.enter();
            // SAFETY: Match the original delegate's optional-selector availability.
            unsafe {
                self.ivars().original.webView_requestMediaCapturePermissionForOrigin_initiatedByFrame_type_decisionHandler(
                    view, origin, frame, kind, completion,
                );
            }
        }
    }
);

impl SystemUiDelegate {
    fn enter(&self) -> CallbackGuard {
        // SAFETY: A native message is currently executing on this live object.
        let delegate = unsafe { Retained::retain(self as *const Self as *mut Self) }
            .expect("the active UI delegate is live");
        self.ivars().depth.set(self.ivars().depth.get() + 1);
        CallbackGuard(delegate)
    }
}

struct CallbackGuard(Retained<SystemUiDelegate>);

impl Drop for CallbackGuard {
    fn drop(&mut self) {
        self.0.ivars().depth.set(self.0.ivars().depth.get() - 1);
    }
}

pub(super) struct SystemUiDelegateRegistration {
    view: Retained<WKWebView>,
    delegate: Retained<SystemUiDelegate>,
}

impl SystemUiDelegateRegistration {
    pub(super) fn attach(native: &WebView, callbacks: SystemNewWebView) -> Result<Self, HostError> {
        let view = native.webview().into_super();
        // SAFETY: The Wry-owned native view is live on its AppKit thread.
        let original = unsafe { view.UIDelegate() }
            .ok_or_else(|| HostError::new("Wry has no native UI delegate"))?;
        let delegate = SystemUiDelegate::alloc(view.mtm()).set_ivars(DelegateState {
            original,
            callbacks,
            native_identity: Retained::as_ptr(&view) as usize,
            depth: Cell::new(0),
        });
        // SAFETY: NSObject initialization; this registration retains the delegate
        // for the entire lifetime of WebKit's weak property.
        let delegate: Retained<SystemUiDelegate> = unsafe { msg_send![super(delegate), init] };
        unsafe { view.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
        Ok(Self { view, delegate })
    }

    pub(super) fn is_active(&self) -> bool {
        self.delegate.ivars().depth.get() != 0
    }
}

impl Drop for SystemUiDelegateRegistration {
    fn drop(&mut self) {
        // SAFETY: Clear only our weak property while the retained native view is
        // live; Wry's complete owner is disposed after this registration.
        unsafe {
            if self.view.UIDelegate().is_some_and(|current| {
                Retained::as_ptr(&current).cast::<()>()
                    == Retained::as_ptr(&self.delegate).cast::<()>()
            }) {
                self.view.setUIDelegate(None);
            }
        }
    }
}
