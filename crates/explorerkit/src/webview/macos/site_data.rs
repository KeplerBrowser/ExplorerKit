//! Read-only capture through the retained native store and private origin helpers.

use std::{
    cell::RefCell,
    collections::VecDeque,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use block2::RcBlock;
use explorerkit_embedder::site_data::{
    backend::{operation_pair, SiteDataCompletion},
    SiteDataCookie, SiteDataError, SiteDataErrorKind, SiteDataLimitation, SiteDataLocalStorage,
    SiteDataMutation, SiteDataOperation, SiteDataOrigin, SiteDataSameSite, SiteDataSelection,
    SiteDataSnapshot, SiteDataStage,
};
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
    DefinedClass, MainThreadOnly,
};
use objc2_foundation::{
    NSArray, NSError, NSHTTPCookie, NSObject, NSObjectProtocol, NSRect, NSString, NSURL,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationDelegate, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore,
};

use crate::surface::SurfaceEventLoopWaker;

const BLANK: &str = "<!doctype html><meta http-equiv='Content-Security-Policy' content=\"default-src 'none'; base-uri 'none'; form-action 'none'\">";
const READ_STORAGE: &str = r#"(() => {
    const entries = [];
    for (let index = 0; index < localStorage.length; index++) {
        const key = localStorage.key(index);
        const value = localStorage.getItem(key);
        if (key === null || value === null) throw new Error();
        entries.push([key, value]);
    }
    return JSON.stringify([location.origin, entries]);
})()"#;

enum Reply {
    Cookies(Vec<SiteDataCookie>),
    Loaded,
    Storage(String),
    Failed,
}

#[derive(Clone)]
struct ReplySink {
    inbox: Weak<RefCell<Option<Reply>>>,
    wake: SurfaceEventLoopWaker,
}

impl ReplySink {
    fn send(&self, reply: Reply) {
        if let Some(inbox) = self.inbox.upgrade() {
            *inbox.borrow_mut() = Some(reply);
            (self.wake)();
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ReplySink]
    struct CaptureNavigation;

    unsafe impl NSObjectProtocol for CaptureNavigation {}
    unsafe impl WKNavigationDelegate for CaptureNavigation {
        #[unsafe(method(webView:didFinishNavigation:))]
        fn finished(&self, _: &WKWebView, _: Option<&WKNavigation>) {
            self.ivars().send(Reply::Loaded);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed_provisional(&self, _: &WKWebView, _: Option<&WKNavigation>, _: &NSError) {
            self.ivars().send(Reply::Failed);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn failed(&self, _: &WKWebView, _: Option<&WKNavigation>, _: &NSError) {
            self.ivars().send(Reply::Failed);
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, _: &WKWebView) {
            self.ivars().send(Reply::Failed);
        }
    }
);

struct Helper {
    view: Retained<WKWebView>,
    _navigation: Retained<CaptureNavigation>,
    origin: SiteDataOrigin,
}

impl Drop for Helper {
    fn drop(&mut self) {
        // SAFETY: The helper is private and is released on AppKit's thread.
        unsafe {
            self.view.setNavigationDelegate(None);
            self.view.stopLoading();
        }
    }
}

pub(super) struct Capture {
    store: Retained<WKWebsiteDataStore>,
    completion: Option<SiteDataCompletion<SiteDataSnapshot>>,
    snapshot: SiteDataSnapshot,
    origins: VecDeque<SiteDataOrigin>,
    inbox: Rc<RefCell<Option<Reply>>>,
    wake: SurfaceEventLoopWaker,
    helper: Option<Helper>,
    stage: SiteDataStage,
}

impl Capture {
    pub(super) fn new(
        store: Retained<WKWebsiteDataStore>,
        selection: SiteDataSelection,
        wake: SurfaceEventLoopWaker,
    ) -> (SiteDataOperation<SiteDataSnapshot>, Option<Self>) {
        let (operation, completion) = operation_pair(wake.clone());
        let mut capture = Self {
            store,
            completion: Some(completion),
            snapshot: SiteDataSnapshot::default(),
            origins: selection.local_storage_origins.into(),
            inbox: Rc::default(),
            wake,
            helper: None,
            stage: SiteDataStage::CaptureLocalStorage,
        };
        if selection.include_cookies {
            capture.stage = SiteDataStage::CaptureCookies;
            capture.snapshot.limitations = vec![
                SiteDataLimitation::SameSiteUnspecifiedOrNone,
                SiteDataLimitation::PartitionMetadataUnavailable,
            ];
            let sink = capture.sink();
            let callback = RcBlock::new(move |array: NonNull<NSArray<NSHTTPCookie>>| {
                // SAFETY: WebKit supplies this array for the duration of its main-thread callback.
                if sink.inbox.strong_count() != 0 {
                    let cookies = unsafe { array.as_ref() }
                        .iter()
                        .map(|cookie| read_cookie(&cookie))
                        .collect();
                    sink.send(Reply::Cookies(cookies));
                }
            });
            // SAFETY: The selected store and callback belong to AppKit's thread.
            unsafe { capture.store.httpCookieStore().getAllCookies(&callback) };
        } else {
            capture.next_origin();
        }
        let pending = capture.completion.is_some().then_some(capture);
        (operation, pending)
    }

    fn sink(&self) -> ReplySink {
        ReplySink {
            inbox: Rc::downgrade(&self.inbox),
            wake: self.wake.clone(),
        }
    }

    fn next_origin(&mut self) {
        self.stage = SiteDataStage::CaptureLocalStorage;
        self.inbox = Rc::default();
        self.helper.take();
        let Some(origin) = self.origins.pop_front() else {
            let snapshot = std::mem::take(&mut self.snapshot);
            self.finish(Ok(snapshot));
            return;
        };
        let Some(url) = NSURL::URLWithString(&NSString::from_str(&origin.serialize())) else {
            self.fail(SiteDataErrorKind::NativeRead);
            return;
        };
        let mtm = self.store.mtm();
        // SAFETY: All objects are created and used on AppKit's thread.
        let (view, navigation) = unsafe {
            let configuration = WKWebViewConfiguration::new(mtm);
            configuration.setWebsiteDataStore(&self.store);
            let view = WKWebView::initWithFrame_configuration(
                WKWebView::alloc(mtm),
                NSRect::default(),
                &configuration,
            );
            let navigation = CaptureNavigation::alloc(mtm).set_ivars(self.sink());
            let navigation: Retained<CaptureNavigation> = msg_send![super(navigation), init];
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*navigation)));
            (view, navigation)
        };
        let submitted =
            unsafe { view.loadHTMLString_baseURL(&NSString::from_str(BLANK), Some(&url)) }
                .is_some();
        self.helper = Some(Helper {
            view,
            _navigation: navigation,
            origin,
        });
        if !submitted {
            self.fail(SiteDataErrorKind::NativeRead);
        }
    }

    pub(super) fn pump(&mut self) -> bool {
        let Some(completion) = &self.completion else {
            return true;
        };
        if completion.cancellation_requested() {
            self.fail(SiteDataErrorKind::Cancelled);
            return true;
        }
        let reply = self.inbox.borrow_mut().take();
        match reply {
            None => {}
            Some(Reply::Cookies(cookies)) => {
                self.snapshot.cookies = cookies;
                self.next_origin();
            }
            Some(Reply::Loaded) => {
                let helper = self.helper.as_ref().expect("load belongs to active helper");
                let sink = self.sink();
                let callback = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
                    // SAFETY: WebKit owns these callback arguments on AppKit's thread.
                    let value = unsafe { value.as_ref() }
                        .and_then(|value| value.downcast_ref::<NSString>());
                    sink.send(match value {
                        Some(value) if error.is_null() => Reply::Storage(value.to_string()),
                        _ => Reply::Failed,
                    });
                });
                // SAFETY: A fixed script reads the private helper's own origin on AppKit's thread.
                unsafe {
                    helper.view.evaluateJavaScript_completionHandler(
                        &NSString::from_str(READ_STORAGE),
                        Some(&callback),
                    )
                };
            }
            Some(Reply::Storage(data)) => {
                let helper = self.helper.as_ref().expect("read belongs to active helper");
                let decoded = serde_json::from_str::<(String, Vec<(String, String)>)>(&data);
                match decoded {
                    Ok((origin, entries)) if origin == helper.origin.serialize() => {
                        self.snapshot.local_storage.push(SiteDataLocalStorage {
                            origin: helper.origin.clone(),
                            entries,
                        });
                        self.next_origin();
                    }
                    _ => self.fail(SiteDataErrorKind::NativeRead),
                }
            }
            Some(Reply::Failed) => self.fail(SiteDataErrorKind::NativeRead),
        }
        self.completion.is_none()
    }

    fn fail(&mut self, kind: SiteDataErrorKind) {
        self.finish(Err(SiteDataError {
            kind,
            stage: self.stage,
            mutation: SiteDataMutation::Unchanged,
        }));
    }

    fn finish(&mut self, result: Result<SiteDataSnapshot, SiteDataError>) {
        // Retire the callback address before making the result observable.
        self.inbox = Rc::default();
        self.helper.take();
        self.snapshot = SiteDataSnapshot::default();
        if let Some(completion) = self.completion.take() {
            completion.complete(result);
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if self.completion.is_some() {
            self.fail(SiteDataErrorKind::OwnerClosed);
        }
    }
}

fn read_cookie(cookie: &NSHTTPCookie) -> SiteDataCookie {
    let domain = cookie.domain().to_string();
    SiteDataCookie {
        host: domain.strip_prefix('.').unwrap_or(&domain).to_owned(),
        host_only: !domain.starts_with('.'),
        path: cookie.path().to_string(),
        name: cookie.name().to_string(),
        value: cookie.value().to_string(),
        secure: cookie.isSecure(),
        http_only: cookie.isHTTPOnly(),
        expires_unix_seconds: cookie
            .expiresDate()
            .map(|date| date.timeIntervalSince1970() as i64),
        same_site: cookie
            .sameSitePolicy()
            .and_then(|policy| match policy.to_string().as_str() {
                "lax" => Some(SiteDataSameSite::Lax),
                "strict" => Some(SiteDataSameSite::Strict),
                "none" => Some(SiteDataSameSite::None),
                _ => None,
            }),
    }
}
