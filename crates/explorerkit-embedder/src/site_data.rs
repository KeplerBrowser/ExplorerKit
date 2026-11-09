//! Explicit, owned site-data transfer contracts. Engine storage policy remains native.

use std::{
    cell::{Cell, RefCell},
    fmt,
    rc::Rc,
    sync::Arc,
};

/// Sharing scope of the store identified by a view, not an isolation identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteDataScope {
    /// The host-selected store; other hosts selecting the same store may share it.
    HostStore,
    /// The process-wide Servo store, shared across views and SDK sessions.
    ProcessStore,
}

/// Explicit source selection. Cookie capture reads the selected store's entire collection.
#[derive(Default)]
pub struct SiteDataSelection {
    /// Opt into all cookies; filter the owned snapshot before initializing if needed.
    pub include_cookies: bool,
    /// Exact origins whose localStorage maps should be read. Repeated origins are deduplicated.
    pub local_storage_origins: Vec<SiteDataOrigin>,
}

/// A canonical HTTP(S) tuple origin, normalized by the URL library.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SiteDataOrigin(url::Origin);

impl SiteDataOrigin {
    /// Extracts an HTTP(S) origin from a URL without userinfo; rejects opaque origins.
    pub fn parse(input: &str) -> Result<Self, SiteDataError> {
        let userinfo = Cell::new(false);
        let on_violation = |violation| {
            if violation == url::SyntaxViolation::EmbeddedCredentials {
                userinfo.set(true);
            }
        };
        let url = url::Url::options()
            .syntax_violation_callback(Some(&on_violation))
            .parse(input)
            .map_err(|_| SiteDataError::admission(SiteDataErrorKind::InvalidInput))?;
        let origin = url.origin();
        if !matches!(url.scheme(), "http" | "https")
            || userinfo.get()
            || !matches!(origin, url::Origin::Tuple(..))
        {
            return Err(SiteDataError::admission(SiteDataErrorKind::InvalidInput));
        }
        Ok(Self(origin))
    }

    /// Returns the URL library's canonical origin serialization.
    pub fn serialize(&self) -> String {
        self.0.ascii_serialization()
    }
}

/// Owned credential-bearing data. Deliberately has no Debug, Display or serialization traits.
#[derive(Default)]
pub struct SiteDataSnapshot {
    /// Cookies selected by the caller for initialization.
    pub cookies: Vec<SiteDataCookie>,
    /// Exact-origin localStorage entries selected by the caller.
    pub local_storage: Vec<SiteDataLocalStorage>,
    /// Known source representation limits, not a browser compatibility certification.
    pub limitations: Vec<SiteDataLimitation>,
}

/// A cookie's transferable fields. Native engine acceptance rules still apply.
#[derive(Clone)]
pub struct SiteDataCookie {
    /// Cookie host without a leading dot, port, path or userinfo.
    pub host: String,
    /// True omits Domain when submitting the native cookie.
    pub host_only: bool,
    /// Absolute cookie path.
    pub path: String,
    /// Cookie name.
    pub name: String,
    /// Cookie value; treat as credentials.
    pub value: String,
    /// Secure transport flag.
    pub secure: bool,
    /// Script-inaccessible flag.
    pub http_only: bool,
    /// Unix-second expiry, or None for a session cookie.
    pub expires_unix_seconds: Option<i64>,
    /// SameSite metadata, where the source can distinguish it.
    pub same_site: Option<SiteDataSameSite>,
}

/// SameSite metadata; preserving it does not certify destination enforcement parity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteDataSameSite {
    /// Explicit SameSite=None.
    None,
    /// SameSite=Lax.
    Lax,
    /// SameSite=Strict.
    Strict,
}

/// Entries for one exact origin; initialization merges supplied keys without clearing others.
pub struct SiteDataLocalStorage {
    /// Exact origin.
    pub origin: SiteDataOrigin,
    /// Key/value pairs. Duplicate keys are rejected before writes.
    pub entries: Vec<(String, String)>,
}

/// Known limitations of native source metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteDataLimitation {
    /// WebKit export cannot distinguish omitted SameSite from explicit None.
    SameSiteUnspecifiedOrNone,
    /// Export does not represent cookie partition metadata.
    PartitionMetadataUnavailable,
}

/// Counts of acknowledged and verified destination work; contains no credentials.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SiteDataWriteSummary {
    /// Cookies whose observable fields passed readback.
    pub cookies_applied: usize,
    /// Already expired cookies omitted from the write.
    pub expired_cookies_skipped: usize,
    /// Exact origins initialized.
    pub local_storage_origins: usize,
    /// Keys written and read back.
    pub local_storage_entries: usize,
}

/// Failure category without native error text or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteDataErrorKind {
    /// This build/platform/direction does not provide the operation.
    Unavailable,
    /// The view is not live in this Runtime.
    UnknownView,
    /// The operation is unsupported for the view's engine.
    WrongEngine,
    /// Attach the view's native surface and pump before retrying.
    NotReady,
    /// The store/view is initializing or the destination already accepted navigation.
    Busy,
    /// Input cannot be represented or contains duplicate identities.
    InvalidInput,
    /// A native source read failed.
    NativeRead,
    /// A native destination write failed.
    NativeWrite,
    /// Destination readback did not match the observable supplied data.
    VerificationFailed,
    /// The caller cancelled or dropped the operation.
    Cancelled,
    /// The source/destination view or owning runtime closed.
    OwnerClosed,
}

/// Stage at which a transfer failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteDataStage {
    /// Admission, or termination before the first native stage.
    Admission,
    /// Reading source cookies.
    CaptureCookies,
    /// Reading source localStorage.
    CaptureLocalStorage,
    /// Writing or verifying cookies.
    WriteCookies,
    /// Writing or verifying localStorage.
    WriteLocalStorage,
    /// Retiring private helper views.
    Cleanup,
}

/// Whether destination changes may have occurred; failure never promises rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteDataMutation {
    /// No destination write was dispatched.
    Unchanged,
    /// A destination write was dispatched and may have completed.
    MayHaveChanged,
}

/// A credential-free error. Raw native descriptions and script contents are never retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiteDataError {
    /// Failure category.
    pub kind: SiteDataErrorKind,
    /// Failing stage.
    pub stage: SiteDataStage,
    /// Conservative destination mutation status.
    pub mutation: SiteDataMutation,
}
impl SiteDataError {
    /// Creates an admission rejection with unchanged destination state.
    pub fn admission(kind: SiteDataErrorKind) -> Self {
        Self {
            kind,
            stage: SiteDataStage::Admission,
            mutation: SiteDataMutation::Unchanged,
        }
    }
}
impl fmt::Display for SiteDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "site data {:?} at {:?} ({:?})",
            self.kind, self.stage, self.mutation
        )
    }
}
impl std::error::Error for SiteDataError {}

struct State<T> {
    cancelled: bool,
    finished: bool,
    result: Option<Result<T, SiteDataError>>,
}

/// Owning-thread-only, non-cloneable waiter; pump the native/Runtime event loop to progress.
/// Dropping it requests cancellation; submitted native writes may still complete.
pub struct SiteDataOperation<T> {
    state: Rc<RefCell<State<T>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}
impl<T> SiteDataOperation<T> {
    /// Takes the terminal result once. Returns None while pending and after it was taken.
    pub fn try_take(&mut self) -> Option<Result<T, SiteDataError>> {
        self.state.borrow_mut().result.take()
    }
    /// Requests cancellation without rollback or blocking; repeated calls have no effect.
    pub fn cancel(&mut self) {
        let changed = {
            let mut state = self.state.borrow_mut();
            let changed = !state.finished && !state.cancelled;
            if changed {
                state.cancelled = true;
            }
            changed
        };
        if changed {
            (self.wake)();
        }
    }
}
impl<T> Drop for SiteDataOperation<T> {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Backend construction API, intentionally not re-exported by the application facade.
pub mod backend {
    use super::*;

    /// Single completion right retained by the native owner, not by a callback or UI waiter.
    pub struct SiteDataCompletion<T> {
        state: Rc<RefCell<State<T>>>,
        wake: Arc<dyn Fn() + Send + Sync>,
    }
    /// Creates local operation state using the host's existing event-loop wake callback.
    pub fn operation_pair<T>(
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> (SiteDataOperation<T>, SiteDataCompletion<T>) {
        let state = Rc::new(RefCell::new(State {
            cancelled: false,
            finished: false,
            result: None,
        }));
        (
            SiteDataOperation {
                state: state.clone(),
                wake: wake.clone(),
            },
            SiteDataCompletion { state, wake },
        )
    }
    impl<T> SiteDataCompletion<T> {
        /// Whether cancellation/drop was requested; the host must still retire native helpers.
        pub fn cancellation_requested(&self) -> bool {
            self.state.borrow().cancelled
        }
        /// Publishes the result after helper cleanup. Consumes the sole completion right.
        pub fn complete(self, result: Result<T, SiteDataError>) {
            {
                let mut state = self.state.borrow_mut();
                state.result = Some(result);
                state.finished = true;
            }
            (self.wake)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn origin_uses_native_canonicalization_and_rejects_userinfo() {
        assert_eq!(
            SiteDataOrigin::parse("https://EXAMPLE.com:443/path?q=1")
                .unwrap()
                .serialize(),
            "https://example.com"
        );
        for invalid in [
            "data:text/plain,x",
            "file:///tmp/x",
            "https://user@example.com",
            "https://@example.com",
            "https://example.com:bad",
        ] {
            assert!(SiteDataOrigin::parse(invalid).is_err());
        }
    }
    #[test]
    fn result_is_taken_once_and_cancel_after_completion_is_noop() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        let (mut operation, completion) = backend::operation_pair(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        assert_eq!(operation.try_take(), None);
        completion.complete(Ok(7));
        assert_eq!(operation.try_take(), Some(Ok(7)));
        assert_eq!(operation.try_take(), None);
        operation.cancel();
        drop(operation);
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn dropping_waiter_requests_cancellation_without_destroying_owner_state() {
        let (operation, completion) = backend::operation_pair::<()>(Arc::new(|| {}));
        drop(operation);
        assert!(completion.cancellation_requested());
        completion.complete(Err(SiteDataError {
            kind: SiteDataErrorKind::Cancelled,
            stage: SiteDataStage::Cleanup,
            mutation: SiteDataMutation::MayHaveChanged,
        }));
    }
}
