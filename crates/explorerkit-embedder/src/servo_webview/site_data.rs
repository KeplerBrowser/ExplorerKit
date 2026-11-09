//! Explicit merge into the process Servo store. The application's view stays blank.

use std::{
    cell::{Cell, RefCell},
    collections::{HashSet, VecDeque},
    rc::Rc,
    sync::Arc,
};

use cookie::{time::OffsetDateTime, Cookie, SameSite};
use servo::{
    CookieSource, EventLoopWaker, JSValue, LoadStatus, RenderingContext, Servo,
    SoftwareRenderingContext, WebResourceLoad, WebResourceResponse, WebView, WebViewBuilder,
    WebViewDelegate,
};
use url::{Host, Url};

use crate::site_data::{
    backend::{operation_pair, SiteDataCompletion},
    SiteDataError, SiteDataErrorKind, SiteDataMutation, SiteDataOperation, SiteDataSameSite,
    SiteDataSnapshot, SiteDataStage, SiteDataWriteSummary,
};

use super::{ProcessServoRuntimeLease, ServoWebView};

#[derive(Clone, Copy, PartialEq, Eq)]
enum NavigationState {
    Blank,
    Navigated,
    Initializing,
    Failed,
}

pub(super) struct NavigationGate(Rc<Cell<NavigationState>>);

impl NavigationGate {
    pub(super) fn new(navigated: bool) -> Self {
        Self(Rc::new(Cell::new(if navigated {
            NavigationState::Navigated
        } else {
            NavigationState::Blank
        })))
    }

    pub(super) fn accept_navigation(&self) -> Result<(), String> {
        self.check_navigation()?;
        self.0.set(NavigationState::Navigated);
        Ok(())
    }

    pub(super) fn check_navigation(&self) -> Result<(), String> {
        match self.0.get() {
            NavigationState::Initializing => {
                Err("site data Busy: destination initialization is active".into())
            }
            NavigationState::Failed => {
                Err("site data Busy: destroy the destination after failed initialization".into())
            }
            NavigationState::Blank | NavigationState::Navigated => Ok(()),
        }
    }

    pub(super) fn is_initializing(&self) -> bool {
        self.0.get() == NavigationState::Initializing
    }
}

struct PreparedCookie {
    url: Url,
    cookie: Cookie<'static>,
}

impl PreparedCookie {
    fn matches(&self, observed: &Cookie<'_>) -> bool {
        let expected = &self.cookie;
        observed
            .domain()
            .and_then(|host| Host::parse(host).ok())
            .as_ref()
            == self.url.host().map(|host| host.to_owned()).as_ref()
            && observed.name() == expected.name()
            && observed.path() == expected.path()
            && observed.value() == expected.value()
            && observed.secure().unwrap_or(false) == expected.secure().unwrap_or(false)
            && observed.http_only().unwrap_or(false) == expected.http_only().unwrap_or(false)
            && observed
                .expires_datetime()
                .map(OffsetDateTime::unix_timestamp)
                == expected
                    .expires_datetime()
                    .map(OffsetDateTime::unix_timestamp)
            && observed.same_site() == expected.same_site()
    }
}

struct PreparedStorage {
    url: Url,
    script: String,
    entries: usize,
}

struct Prepared {
    cookies: VecDeque<PreparedCookie>,
    storage: VecDeque<PreparedStorage>,
    expired: usize,
}

fn invalid() -> SiteDataError {
    SiteDataError::admission(SiteDataErrorKind::InvalidInput)
}

// The native setter accepts a pre-parsed Cookie, so validate the pair it will
// place on the wire. RFC 6265 §4.1.1 permits a token and optionally quoted octets.
fn valid_cookie_pair(name: &str, value: &str) -> bool {
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value);
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^`_|~".contains(&byte))
        && value.bytes().all(
            |byte| matches!(byte, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e),
        )
}

impl Prepared {
    fn new(snapshot: SiteDataSnapshot) -> Result<Self, SiteDataError> {
        let mut prepared = Self {
            cookies: VecDeque::new(),
            storage: VecDeque::new(),
            expired: 0,
        };
        let mut identities = HashSet::new();
        let now = OffsetDateTime::now_utc();
        for cookie in snapshot.cookies {
            let host = Host::parse(&cookie.host)
                .map_err(|_| invalid())?
                .to_string();
            if cookie.host.starts_with('.')
                || host.starts_with('.')
                || !cookie.path.starts_with('/')
                || !valid_cookie_pair(&cookie.name, &cookie.value)
            {
                return Err(invalid());
            }
            let mut url = Url::parse(if cookie.secure {
                "https://example.invalid/"
            } else {
                "http://example.invalid/"
            })
            .expect("fixed setter URL");
            url.set_host(Some(&host)).map_err(|_| invalid())?;
            url.set_path(&cookie.path);
            if url.path() != cookie.path
                || !identities.insert((host.clone(), cookie.path.clone(), cookie.name.clone()))
            {
                return Err(invalid());
            }
            let expiry = cookie
                .expires_unix_seconds
                .map(OffsetDateTime::from_unix_timestamp)
                .transpose()
                .map_err(|_| invalid())?;
            let mut native = Cookie::build((cookie.name, cookie.value))
                .path(cookie.path)
                .secure(cookie.secure)
                .http_only(cookie.http_only)
                .build();
            if !cookie.host_only {
                native.set_domain(host);
            }
            if let Some(same_site) = cookie.same_site {
                native.set_same_site(match same_site {
                    SiteDataSameSite::None => SameSite::None,
                    SiteDataSameSite::Lax => SameSite::Lax,
                    SiteDataSameSite::Strict => SameSite::Strict,
                });
            }
            native.set_expires(expiry);
            if expiry.is_some_and(|expiry| expiry <= now) {
                prepared.expired += 1;
            } else {
                prepared.cookies.push_back(PreparedCookie {
                    url,
                    cookie: native,
                });
            }
        }
        let mut origins = HashSet::new();
        for storage in snapshot.local_storage {
            let mut keys = HashSet::new();
            if !origins.insert(storage.origin.clone())
                || storage.entries.iter().any(|(key, _)| !keys.insert(key))
            {
                return Err(invalid());
            }
            let origin = storage.origin.serialize();
            let url = Url::parse(&format!("{origin}/__explorerkit_site_data__"))
                .map_err(|_| invalid())?;
            let entries = storage.entries.len();
            let data = serde_json::to_string(&(origin, storage.entries)).map_err(|_| invalid())?;
            let quoted = serde_json::to_string(&data).map_err(|_| invalid())?;
            let script = format!("(() => {{ const [origin, entries] = JSON.parse({quoted}); if (location.origin !== origin) return 'origin'; for (const [key, value] of entries) localStorage.setItem(key, value); return entries.every(([key, value]) => localStorage.getItem(key) === value) ? 'verified' : 'mismatch'; }})()");
            prepared.storage.push_back(PreparedStorage {
                url,
                script,
                entries,
            });
        }
        Ok(prepared)
    }
}

struct Bootstrap {
    url: Url,
    served: Cell<bool>,
    complete: Cell<bool>,
    failed: Cell<bool>,
}

impl WebViewDelegate for Bootstrap {
    fn load_web_resource(&self, _: WebView, load: WebResourceLoad) {
        let request = load.request();
        let accepted = request.is_for_main_frame && request.url == self.url && !self.served.get();
        let mut response = WebResourceResponse::new(request.url.clone());
        response.headers.insert(
            "content-type",
            "text/html".parse().expect("fixed content type"),
        );
        response.headers.insert(
            "cache-control",
            "no-store".parse().expect("fixed cache policy"),
        );
        let mut intercepted = load.intercept(response);
        if accepted {
            self.served.set(true);
            intercepted.send_body_data(b"<!doctype html><meta http-equiv='Content-Security-Policy' content=\"default-src 'none'; base-uri 'none'; form-action 'none'\">".to_vec());
            intercepted.finish();
        } else {
            intercepted.cancel();
        }
    }

    fn notify_crashed(&self, _: WebView, _: String, _: Option<String>) {
        self.failed.set(true);
    }

    fn notify_load_status_changed(&self, _: WebView, status: LoadStatus) {
        if status == LoadStatus::Complete {
            self.complete.set(true);
        }
    }
}

type CookieReadback = Rc<RefCell<Option<Vec<Cookie<'static>>>>>;

enum Step {
    Next,
    CookieWrite(Rc<Cell<bool>>),
    CookieRead(PreparedCookie, CookieReadback),
    Bootstrap(PreparedStorage, WebView, Rc<Bootstrap>),
    Storage(
        usize,
        WebView,
        Rc<Bootstrap>,
        Rc<Cell<Option<StorageResult>>>,
    ),
}

#[derive(Clone, Copy)]
enum StorageResult {
    Verified,
    Mismatch,
    Failed,
}

pub(super) struct Initialization {
    gate: Rc<Cell<NavigationState>>,
    completion: SiteDataCompletion<SiteDataWriteSummary>,
    prepared: Prepared,
    cookies_to_verify: VecDeque<PreparedCookie>,
    summary: SiteDataWriteSummary,
    step: Step,
    stage: SiteDataStage,
    mutation: SiteDataMutation,
}

impl Initialization {
    fn error(&self, kind: SiteDataErrorKind) -> SiteDataError {
        SiteDataError {
            kind,
            stage: self.stage,
            mutation: self.mutation,
        }
    }

    fn pump(&mut self, servo: &Servo) -> Option<Result<SiteDataWriteSummary, SiteDataError>> {
        if self.completion.cancellation_requested() {
            return Some(Err(self.error(SiteDataErrorKind::Cancelled)));
        }
        {
            match &self.step {
                Step::CookieWrite(done) if !done.get() => return None,
                Step::Next | Step::CookieWrite(_) => {}
                Step::CookieRead(cookie, result) => {
                    let observed = result.borrow_mut().take()?;
                    if !observed.iter().any(|record| cookie.matches(record)) {
                        return Some(Err(self.error(SiteDataErrorKind::VerificationFailed)));
                    }
                    self.summary.cookies_applied += 1;
                    self.step = Step::Next;
                }
                Step::Bootstrap(_, _, delegate) | Step::Storage(_, _, delegate, _)
                    if delegate.failed.get() =>
                {
                    return Some(Err(self.error(SiteDataErrorKind::NativeWrite)));
                }
                Step::Bootstrap(_, _, delegate) if !delegate.complete.get() => return None,
                Step::Bootstrap(_, _, delegate) if !delegate.served.get() => {
                    return Some(Err(self.error(SiteDataErrorKind::NativeWrite)));
                }
                Step::Bootstrap(..) => {
                    let Step::Bootstrap(storage, view, delegate) =
                        std::mem::replace(&mut self.step, Step::Next)
                    else {
                        unreachable!()
                    };
                    let result = Rc::new(Cell::new(None));
                    let reply = Rc::downgrade(&result);
                    if storage.entries != 0 {
                        self.mutation = SiteDataMutation::MayHaveChanged;
                    }
                    view.evaluate_javascript(storage.script, move |value| {
                        if let Some(reply) = reply.upgrade() {
                            reply.set(Some(match value {
                                Ok(JSValue::String(value)) if value == "verified" => {
                                    StorageResult::Verified
                                }
                                Ok(JSValue::String(value)) if value == "mismatch" => {
                                    StorageResult::Mismatch
                                }
                                _ => StorageResult::Failed,
                            }));
                        }
                    });
                    self.step = Step::Storage(storage.entries, view, delegate, result);
                    return None;
                }
                Step::Storage(entries, _view, _, result) => {
                    match result.take()? {
                        StorageResult::Verified => {}
                        StorageResult::Mismatch => {
                            return Some(Err(self.error(SiteDataErrorKind::VerificationFailed)))
                        }
                        StorageResult::Failed => {
                            return Some(Err(self.error(SiteDataErrorKind::NativeWrite)))
                        }
                    }
                    self.summary.local_storage_origins += 1;
                    self.summary.local_storage_entries += entries;
                    self.step = Step::Next;
                }
            }
            if let Some(cookie) = self.prepared.cookies.pop_front() {
                self.stage = SiteDataStage::WriteCookies;
                let done = Rc::new(Cell::new(false));
                let reply = Rc::downgrade(&done);
                self.mutation = SiteDataMutation::MayHaveChanged;
                servo.site_data_manager().set_cookie_for_url(
                    cookie.url.clone(),
                    cookie.cookie.clone(),
                    Some(Box::new(move || {
                        if let Some(reply) = reply.upgrade() {
                            reply.set(true);
                        }
                    })),
                );
                self.cookies_to_verify.push_back(cookie);
                self.step = Step::CookieWrite(done);
                return None;
            }
            if let Some(cookie) = self.cookies_to_verify.pop_front() {
                let readback = Rc::new(RefCell::new(None));
                let reply = Rc::downgrade(&readback);
                // Later native inserts can evict earlier cookies in the same
                // domain bucket. Verify the batch after all inserts settle.
                servo.site_data_manager().cookies_for_url_async(
                    cookie.url.clone(),
                    CookieSource::HTTP,
                    move |cookies| {
                        if let Some(reply) = reply.upgrade() {
                            *reply.borrow_mut() = Some(cookies);
                        }
                    },
                );
                self.step = Step::CookieRead(cookie, readback);
                return None;
            }
            if let Some(storage) = self.prepared.storage.pop_front() {
                self.stage = SiteDataStage::WriteLocalStorage;
                let context = match SoftwareRenderingContext::new(dpi::PhysicalSize::new(1, 1)) {
                    Ok(context) => Rc::new(context),
                    Err(_) => return Some(Err(self.error(SiteDataErrorKind::NativeWrite))),
                };
                if context.make_current().is_err() {
                    return Some(Err(self.error(SiteDataErrorKind::NativeWrite)));
                }
                let delegate = Rc::new(Bootstrap {
                    url: storage.url.clone(),
                    served: Cell::new(false),
                    complete: Cell::new(false),
                    failed: Cell::new(false),
                });
                let helper = WebViewBuilder::new(servo, context)
                    .delegate(delegate.clone())
                    .url(storage.url.clone())
                    .build();
                self.step = Step::Bootstrap(storage, helper, delegate);
                return None;
            }
            Some(Ok(self.summary))
        }
    }

    fn finish(self, result: Result<SiteDataWriteSummary, SiteDataError>) {
        let Self {
            gate,
            completion,
            step,
            prepared,
            cookies_to_verify,
            ..
        } = self;
        drop((step, prepared, cookies_to_verify));
        gate.set(if result.is_ok() {
            NavigationState::Blank
        } else {
            NavigationState::Failed
        });
        completion.complete(result);
    }
}

impl ServoWebView {
    /// Checks the initialization gate before the surface host queues navigation.
    #[doc(hidden)]
    pub fn check_site_data_navigation(&self) -> Result<(), String> {
        self.site_data_gate.check_navigation()
    }

    /// Host backend entry point for a native-ready blank macOS Servo destination.
    #[doc(hidden)]
    pub fn initialize_site_data(
        &self,
        snapshot: SiteDataSnapshot,
    ) -> Result<SiteDataOperation<SiteDataWriteSummary>, SiteDataError> {
        if self.site_data_gate.0.get() != NavigationState::Blank {
            return Err(SiteDataError::admission(SiteDataErrorKind::Busy));
        }
        self.adapter
            .native_webview()
            .map_err(|_| SiteDataError::admission(SiteDataErrorKind::NotReady))?;
        let runtime = self.adapter.runtime();
        let lease = &runtime.lease;
        if lease.site_data.borrow().is_some() {
            return Err(SiteDataError::admission(SiteDataErrorKind::Busy));
        }
        let prepared = Prepared::new(snapshot)?;
        let summary = SiteDataWriteSummary {
            expired_cookies_skipped: prepared.expired,
            ..SiteDataWriteSummary::default()
        };
        let wake = lease.waker.clone();
        let (operation, completion) = operation_pair(Arc::new(move || wake.wake()));
        self.site_data_gate.0.set(NavigationState::Initializing);
        *lease.site_data.borrow_mut() = Some(Initialization {
            gate: self.site_data_gate.0.clone(),
            completion,
            prepared,
            cookies_to_verify: VecDeque::new(),
            summary,
            step: Step::Next,
            stage: SiteDataStage::Admission,
            mutation: SiteDataMutation::Unchanged,
        });
        lease.waker.wake();
        Ok(operation)
    }
}

impl ProcessServoRuntimeLease {
    pub(super) fn pump_site_data(&self) {
        let Some(mut initialization) = self.site_data.borrow_mut().take() else {
            return;
        };
        if let Some(result) = initialization.pump(&self.servo) {
            initialization.finish(result);
        } else {
            *self.site_data.borrow_mut() = Some(initialization);
        }
    }

    pub(super) fn close_site_data(&self, gate: &NavigationGate) {
        let owns_operation = self
            .site_data
            .borrow()
            .as_ref()
            .is_some_and(|initialization| Rc::ptr_eq(&initialization.gate, &gate.0));
        if owns_operation {
            let initialization = self.site_data.borrow_mut().take().expect("checked owner");
            let error = initialization.error(SiteDataErrorKind::OwnerClosed);
            initialization.finish(Err(error));
        }
    }
}
