//! Capture native cookies and committed localStorage from the retained named store.

use std::{
    cell::RefCell,
    ptr::NonNull,
    rc::{Rc, Weak},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
};

use block2::RcBlock;
use explorerkit_embedder::site_data::{
    backend::{operation_pair, SiteDataCompletion},
    SiteDataCookie, SiteDataError, SiteDataErrorKind, SiteDataLimitation, SiteDataLocalStorage,
    SiteDataMutation, SiteDataOperation, SiteDataOrigin, SiteDataSameSite, SiteDataSelection,
    SiteDataSnapshot, SiteDataStage,
};
use objc2::{rc::Retained, MainThreadOnly};
use objc2_foundation::{NSArray, NSHTTPCookie, NSSet, NSUUID};
use objc2_web_kit::{WKWebsiteDataRecord, WKWebsiteDataStore, WKWebsiteDataTypeLocalStorage};

use crate::surface::SurfaceEventLoopWaker;

mod committed_storage;

type StorageResult = Result<Option<Vec<SiteDataLocalStorage>>, ()>;

struct StorageRead {
    result: mpsc::Receiver<StorageResult>,
    cancelled: Arc<AtomicBool>,
}

impl StorageRead {
    fn start(
        store: committed_storage::NamedStore,
        origins: Vec<SiteDataOrigin>,
        wake: SurfaceEventLoopWaker,
    ) -> Result<Self, ()> {
        let (sender, result) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        thread::Builder::new()
            .name("explorerkit-site-data".into())
            .spawn(move || {
                let read = || {
                    if worker_cancelled.load(Ordering::Relaxed) {
                        return Err(());
                    }
                    let Some(storage) = store.open()? else {
                        return Ok(None);
                    };
                    origins
                        .into_iter()
                        .map(|origin| {
                            if worker_cancelled.load(Ordering::Relaxed) {
                                return Err(());
                            }
                            storage.capture(origin)
                        })
                        .collect::<Result<_, _>>()
                        .map(Some)
                };
                let result = read();
                if !worker_cancelled.load(Ordering::Relaxed) && sender.send(result).is_ok() {
                    wake();
                }
            })
            .map_err(|_| ())?;
        Ok(Self { result, cancelled })
    }
}

impl Drop for StorageRead {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

type Inbox<T> = Rc<RefCell<Option<T>>>;

fn reply<T>(sink: &Weak<RefCell<Option<T>>>, wake: &SurfaceEventLoopWaker, value: T) {
    if let Some(inbox) = sink.upgrade() {
        *inbox.borrow_mut() = Some(value);
        wake();
    }
}

enum Pending {
    Cookies(Inbox<Vec<SiteDataCookie>>),
    VerifyStore(Inbox<bool>),
    MaterializeStore(Inbox<()>),
    Read(StorageRead),
}

pub(super) struct Capture {
    store: Retained<WKWebsiteDataStore>,
    completion: Option<SiteDataCompletion<SiteDataSnapshot>>,
    snapshot: SiteDataSnapshot,
    origins: Vec<SiteDataOrigin>,
    pending: Option<Pending>,
    materialized: bool,
    wake: SurfaceEventLoopWaker,
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
            origins: selection.local_storage_origins,
            pending: None,
            materialized: false,
            wake,
        };
        if selection.include_cookies {
            capture.snapshot.limitations = vec![
                SiteDataLimitation::SameSiteUnspecifiedOrNone,
                SiteDataLimitation::PartitionMetadataUnavailable,
            ];
            let inbox: Inbox<Vec<SiteDataCookie>> = Rc::default();
            let sink = Rc::downgrade(&inbox);
            let wake = capture.wake.clone();
            let callback = RcBlock::new(move |array: NonNull<NSArray<NSHTTPCookie>>| {
                if sink.strong_count() != 0 {
                    // SAFETY: WebKit supplies this array on AppKit's thread.
                    let cookies = unsafe { array.as_ref() }
                        .iter()
                        .map(|cookie| read_cookie(&cookie))
                        .collect();
                    reply(&sink, &wake, cookies);
                }
            });
            capture.pending = Some(Pending::Cookies(inbox));
            // SAFETY: The store and callback belong to AppKit's thread.
            unsafe { capture.store.httpCookieStore().getAllCookies(&callback) };
        } else {
            capture.verify_store();
        }
        let pending = capture.completion.is_some().then_some(capture);
        (operation, pending)
    }

    fn verify_store(&mut self) {
        if self.origins.is_empty() {
            self.succeed(Vec::new());
            return;
        }
        // SAFETY: Admission retained and verified this named store on AppKit's thread.
        let Some(identifier) = (unsafe { self.store.identifier() }) else {
            self.fail(SiteDataErrorKind::NativeRead);
            return;
        };
        let expected = identifier.as_bytes();
        let inbox = Rc::default();
        let sink = Rc::downgrade(&inbox);
        let wake = self.wake.clone();
        let callback = RcBlock::new(move |array: NonNull<NSArray<NSUUID>>| {
            if sink.strong_count() != 0 {
                // SAFETY: WebKit supplies the UUIDs on AppKit's thread.
                let found = unsafe { array.as_ref() }
                    .iter()
                    .any(|id| id.as_bytes() == expected);
                reply(&sink, &wake, found);
            }
        });
        self.pending = Some(Pending::VerifyStore(inbox));
        // WebKit lists the UUIDs in its active app/container namespace. A
        // matching name in an inactive namespace is not evidence of a store.
        // SAFETY: This macOS 14 API is used only for a verified named store.
        unsafe { WKWebsiteDataStore::fetchAllDataStoreIdentifiers(&callback, self.store.mtm()) };
    }

    fn materialize_store(&mut self) {
        if self.materialized {
            self.fail(SiteDataErrorKind::NativeRead);
            return;
        }
        self.materialized = true;
        let inbox = Rc::default();
        let sink = Rc::downgrade(&inbox);
        let wake = self.wake.clone();
        let callback = RcBlock::new(move |_: NonNull<NSArray<WKWebsiteDataRecord>>| {
            reply(&sink, &wake, ());
        });
        self.pending = Some(Pending::MaterializeStore(inbox));
        // A fresh named store can exist before the network process has
        // created its storage layout. Fetch metadata to start that process.
        // The returned records supply neither values nor evidence of emptiness.
        // SAFETY: The store and callback belong to AppKit's thread.
        unsafe {
            self.store.fetchDataRecordsOfTypes_completionHandler(
                &NSSet::from_slice(&[WKWebsiteDataTypeLocalStorage]),
                &callback,
            )
        };
    }

    fn begin_storage(&mut self) {
        // SAFETY: Admission retained and verified this named store on AppKit's thread.
        let Some(identifier) = (unsafe { self.store.identifier() }) else {
            self.fail(SiteDataErrorKind::NativeRead);
            return;
        };
        match committed_storage::NamedStore::new(&identifier)
            .and_then(|store| StorageRead::start(store, self.origins.clone(), self.wake.clone()))
        {
            Ok(storage) => self.pending = Some(Pending::Read(storage)),
            Err(()) => self.fail(SiteDataErrorKind::NativeRead),
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
        match self.pending.as_ref() {
            Some(Pending::Cookies(inbox)) => {
                let cookies = inbox.borrow_mut().take();
                if let Some(cookies) = cookies {
                    self.pending = None;
                    self.snapshot.cookies = cookies;
                    self.verify_store();
                }
            }
            Some(Pending::VerifyStore(inbox)) => {
                let found = inbox.borrow_mut().take();
                match found {
                    Some(true) => self.begin_storage(),
                    Some(false) => self.materialize_store(),
                    None => {}
                }
            }
            Some(Pending::MaterializeStore(inbox)) => {
                let ready = inbox.borrow_mut().take().is_some();
                if ready {
                    self.verify_store();
                }
            }
            Some(Pending::Read(storage)) => match storage.result.try_recv() {
                Ok(Ok(Some(records))) => self.succeed(records),
                Ok(Ok(None)) => self.materialize_store(),
                Ok(Err(())) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.fail(SiteDataErrorKind::NativeRead);
                }
                Err(mpsc::TryRecvError::Empty) => {}
            },
            None => {}
        }
        self.completion.is_none()
    }

    fn succeed(&mut self, records: Vec<SiteDataLocalStorage>) {
        let mut snapshot = std::mem::take(&mut self.snapshot);
        snapshot.local_storage = records;
        self.finish(Ok(snapshot));
    }

    fn fail(&mut self, kind: SiteDataErrorKind) {
        let stage = if matches!(self.pending, Some(Pending::Cookies(_))) {
            SiteDataStage::CaptureCookies
        } else {
            SiteDataStage::CaptureLocalStorage
        };
        self.finish(Err(SiteDataError {
            kind,
            stage,
            mutation: SiteDataMutation::Unchanged,
        }));
    }

    fn finish(&mut self, result: Result<SiteDataSnapshot, SiteDataError>) {
        self.pending.take();
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
