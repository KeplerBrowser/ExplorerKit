//! WebKit-owned icon loading, with native first-image decoding.

use std::{
    cell::RefCell,
    collections::{HashSet, VecDeque},
    io::Cursor,
    rc::Rc,
};

use block2::{Block, RcBlock};
use explorerkit_embedder::{FaviconImage, FaviconPixelFormat};
use objc2::{
    define_class, msg_send,
    rc::{Retained, Weak},
    runtime::AnyObject,
    sel, DefinedClass, MainThreadOnly,
};
use objc2_foundation::{
    NSData, NSDictionary, NSError, NSObject, NSObjectProtocol, NSString, NSURL,
};
use objc2_web_kit::{WKBackForwardListItem, WKContentWorld, WKWebView};
use wry::{WebView, WebViewExtMacOS};

use super::{
    javascript::{native_error, NativeEvaluationError},
    SystemCallbackSink,
};

mod selection;
mod svg;
pub(super) use selection::{decline, Candidate, Selection};
use selection::{Key, Pending};

type DataCompletion = Block<dyn Fn(*mut NSData)>;
type LoadDecision = Block<dyn Fn(*mut DataCompletion)>;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SystemCallbackSink]
    pub(super) struct IconDelegate;

    unsafe impl NSObjectProtocol for IconDelegate {}

    impl IconDelegate {
        #[unsafe(method(webView:shouldLoadIconWithParameters:completionHandler:))]
        fn should_load_icon(&self, _view: &WKWebView, parameters: &AnyObject, decision: &LoadDecision) {
            // SAFETY: WebKit's iconType is NSInteger; standard favicons are zero.
            let icon_type: isize = unsafe { msg_send![parameters, iconType] };
            let sink = self.ivars().clone();
            let Some(token) = (icon_type == 0).then(|| sink.begin_favicon()).flatten() else {
                decision.call((std::ptr::null_mut(),));
                return;
            };
            let pending = Pending { key: native_key(parameters), decision: decision.copy() };
            let action = {
                let mut callbacks = sink.callbacks.borrow_mut();
                let Some(state) = callbacks.views.get_mut(&sink.target) else {
                    drop(callbacks);
                    decline(vec![pending]);
                    return;
                };
                state.favicon_selection.hold(pending)
            };
            match action {
                Ok(true) => collect(_view, sink, token),
                Ok(false) => {},
                Err(pending) => release(&sink, token, pending),
            }
        }
    }
);

// WHATWG HTML rel=icon: snapshot DOM order/media once; WebKit still owns all loads.
const SNAPSHOT: &str = r#"JSON.stringify([...document.querySelectorAll('link')].filter(e =>
 e instanceof HTMLLinkElement && [...e.relList].some(t => t.toLowerCase() === 'icon')).map(e => ({
 url:e.href, attributes:Object.fromEntries([...e.attributes].map(a => [a.localName,a.value])),
 media:!e.media || matchMedia(e.media).matches, sizes:[...e.sizes]
})))"#;

fn native_key(parameters: &AnyObject) -> Option<Key> {
    if !unsafe { msg_send![parameters, respondsToSelector: sel!(attributes)] } {
        return None;
    }
    // SAFETY: _WKLinkIconParameters's declared native properties are copied here.
    unsafe {
        let url: Option<Retained<NSURL>> = msg_send![parameters, url];
        let attributes: Option<Retained<NSDictionary<NSString, NSString>>> =
            msg_send![parameters, attributes];
        let (names, values) = attributes?.to_vecs();
        Some(Key {
            url: url?.absoluteString()?.to_string(),
            attributes: names
                .iter()
                .zip(&values)
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        })
    }
}

fn collect(view: &WKWebView, sink: SystemCallbackSink, token: u64) {
    let selector = sel!(evaluateJavaScript:inFrame:inContentWorld:completionHandler:);
    if !view.respondsToSelector(selector) {
        finish_collection(&sink, token, None);
        return;
    }
    let completion = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
        if !sink.favicon_is_current(token) {
            return;
        }
        // SAFETY: WebKit lends these objects for this main-thread completion.
        unsafe {
            if let Some(error) = error.as_ref() {
                if matches!(
                    native_error(error),
                    NativeEvaluationError::ProcessTerminated
                        | NativeEvaluationError::ViewInvalidated
                ) {
                    sink.favicon_lifetime_error(token);
                    return;
                }
            }
            let json = error
                .is_null()
                .then(|| {
                    value
                        .as_ref()?
                        .downcast_ref::<NSString>()
                        .map(|s| s.to_string())
                })
                .flatten();
            finish_collection(&sink, token, json.as_deref());
        }
    });
    // SAFETY: selector availability also establishes WKContentWorld (macOS11); no view is captured.
    unsafe {
        view.evaluateJavaScript_inFrame_inContentWorld_completionHandler(
            &NSString::from_str(SNAPSHOT),
            None,
            &WKContentWorld::defaultClientWorld(view.mtm()),
            Some(&completion),
        );
    }
}

fn finish_collection(sink: &SystemCallbackSink, token: u64, json: Option<&str>) {
    let pending = {
        let mut callbacks = sink.callbacks.borrow_mut();
        let Some(state) = callbacks.views.get_mut(&sink.target) else {
            return;
        };
        if !state.document_ready || state.favicon_request != token {
            return;
        }
        state.favicon_selection.settle(json)
    };
    if let Some(pending) = pending {
        for request in pending {
            release(sink, token, request);
        }
    }
}

fn release(sink: &SystemCallbackSink, token: u64, pending: Pending) {
    if !sink.favicon_is_current(token) {
        decline(vec![pending]);
        return;
    }
    let svg = svg::available();
    let candidate = {
        let callbacks = sink.callbacks.borrow();
        callbacks
            .views
            .get(&sink.target)
            .map(|state| state.favicon_selection.classify(pending.key.as_ref(), svg))
            .unwrap_or(Candidate::Rejected)
    };
    if candidate == Candidate::Rejected {
        decline(vec![pending]);
        return;
    }
    let completion_sink = sink.clone();
    let completion = RcBlock::new(move |data: *mut NSData| {
        if !completion_sink.favicon_is_current(token) {
            return;
        }
        // SAFETY: WebKit lends NSData on the main thread. All output bytes are owned.
        if let Some(data) = unsafe { data.as_ref() }.filter(|data| data.len() <= MAX_ENCODED_BYTES)
        {
            let bytes = data.to_vec();
            if let Some((image, bytes)) = decode_native(bytes) {
                completion_sink.complete_native_favicon(token, image, bytes, candidate);
            }
        }
    });
    if sink.favicon_is_current(token) {
        pending.decision.call((RcBlock::as_ptr(&completion),));
    } else {
        decline(vec![pending]);
    }
}

fn decode_native(bytes: Vec<u8>) -> Option<(FaviconImage, Vec<u8>)> {
    if bytes.is_empty() || bytes.len() > MAX_ENCODED_BYTES {
        return None;
    }
    if image::guess_format(&bytes).is_ok() {
        return decode_bytes(&bytes).map(|image| (image, bytes));
    }
    let png = svg::normalize(&bytes)?;
    decode_bytes(&png).map(|image| (image, png))
}

pub(super) struct IconRegistration {
    view: Retained<WKWebView>,
    _delegate: Retained<IconDelegate>,
}

impl IconRegistration {
    pub(super) fn attach(native: &WebView, sink: SystemCallbackSink) -> Option<Self> {
        let view = native.webview().into_super();
        // SAFETY: the private property is weak. Keep a separate strong delegate,
        // without replacing WRY's navigation/UI delegates, for this native lifetime.
        unsafe {
            if !icon_loading_available(&view) {
                return None;
            }
            sink.enable_favicons(&view);
            let delegate = IconDelegate::alloc(view.mtm()).set_ivars(sink);
            let delegate: Retained<IconDelegate> = msg_send![super(delegate), init];
            let _: () = msg_send![&*view, _setIconLoadingDelegate: &*delegate];
            Some(Self {
                view,
                _delegate: delegate,
            })
        }
    }
}

impl Drop for IconRegistration {
    fn drop(&mut self) {
        // SAFETY: attachment checked this selector, and the retained view is live.
        unsafe {
            let _: () =
                msg_send![&*self.view, _setIconLoadingDelegate: std::ptr::null::<AnyObject>()];
        }
    }
}

fn icon_loading_available(view: &NSObject) -> bool {
    view.respondsToSelector(sel!(_setIconLoadingDelegate:))
}

// Bound encoded input and decoded pixels; oversized icons remain unobserved.
const MAX_ENCODED_BYTES: usize = 16 * 1024 * 1024;
const MAX_PIXELS: u64 = 4 * 1024 * 1024;

#[cfg(test)]
fn decode(data: &NSData) -> Option<FaviconImage> {
    if data.is_empty() || data.len() > MAX_ENCODED_BYTES {
        return None;
    }
    decode_bytes(&data.to_vec())
}

fn decode_bytes(bytes: &[u8]) -> Option<FaviconImage> {
    if bytes.is_empty() || bytes.len() > MAX_ENCODED_BYTES {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PIXELS as u32);
    limits.max_image_height = Some(MAX_PIXELS as u32);
    limits.max_alloc = Some(MAX_PIXELS * 4);
    let reader = || {
        let mut reader = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        reader.limits(limits.clone());
        Some(reader)
    };
    let (width, height) = reader()?.into_dimensions().ok()?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let rgba = reader()?.decode().ok()?.into_rgba8();
    Some(FaviconImage {
        width: rgba.width(),
        height: rgba.height(),
        format: FaviconPixelFormat::RGBA8,
        bytes: rgba.into_raw(),
    })
}

// The store bounds only retained encoded data. Decoding and delivered events have
// their own existing limits and lifetimes.
const HISTORY_BYTES: usize = 8 * 1024 * 1024;
const HISTORY_ENTRIES: usize = 128;
type DocumentIcon = Rc<RefCell<Option<Rc<[u8]>>>>;

struct Record<K> {
    key: K,
    document: DocumentIcon,
}

struct History<K> {
    records: VecDeque<Record<K>>,
    // Native membership survives image eviction, so an old unknown target cannot
    // be mistaken for a newly added same-document alias. No image bytes here.
    last_live: Vec<K>,
    current: Option<K>,
    // Preserve known lineage across filtered native snapshots without keeping
    // orphaned image bytes alive after their last retained record disappears.
    current_document: std::rc::Weak<RefCell<Option<Rc<[u8]>>>>,
}

impl<K> Default for History<K> {
    fn default() -> Self {
        Self {
            records: VecDeque::new(),
            last_live: Vec::new(),
            current: None,
            current_document: std::rc::Weak::new(),
        }
    }
}

impl<K: Clone + Eq> History<K> {
    fn reconcile(
        &mut self,
        current: Option<K>,
        live: Vec<K>,
        committed: bool,
        allow_activation: bool,
    ) -> Option<Rc<[u8]>> {
        if !committed && !allow_activation {
            self.prune(&live);
            self.last_live.retain(|key| live.contains(key));
            return None;
        }
        if !committed {
            // Several silent pushState entries can precede the next native
            // notification. Preserve every new alias, not only the current one.
            let added: Vec<_> = live
                .iter()
                .filter(|key| !self.last_live.contains(key))
                .cloned()
                .collect();
            for key in added {
                self.observe(Some(key), &live, false);
            }
        }
        let unknown_existing = !committed
            && current.as_ref().is_some_and(|key| {
                self.last_live.contains(key)
                    && !self.records.iter().any(|record| &record.key == key)
            });
        let retained = if unknown_existing {
            self.prune(&live);
            None
        } else {
            self.observe(current, &live, committed)
        };
        self.last_live = live;
        retained
    }

    fn prune(&mut self, live: &[K]) {
        self.records.retain(|record| live.contains(&record.key));
        if self.current.as_ref().is_some_and(|key| !live.contains(key)) {
            // The weak document identity is sufficient for known lineage;
            // do not retain an extra removed native-item bookkeeping record.
            self.current = None;
        }
    }

    fn activate(
        &mut self,
        key: K,
        committed: bool,
        previous: Option<DocumentIcon>,
    ) -> Option<Rc<[u8]>> {
        if !committed && self.current.as_ref() != Some(&key) {
            // Native history traversal can expose an already-known target
            // before isLoading/policy/commit. A different known document is
            // never a same-document observation; keep the departing identity.
            let target = self.records.iter().find(|record| record.key == key);
            // Weak identity remains valid after the departed slot's image and
            // last strong record are released. A dead slot is still different
            // from a known target; never activate that target before commit.
            if target.is_some_and(|target| {
                !std::rc::Weak::ptr_eq(&Rc::downgrade(&target.document), &self.current_document)
            }) {
                return None;
            }
        }
        let existing = self
            .records
            .iter()
            .position(|record| record.key == key)
            .and_then(|index| self.records.remove(index));
        // A same-document traversal can change native items without a commit.
        // A subsequent real commit into the currently active document's known
        // lineage is a reload, even if the last observed item was its sibling.
        let same_document_commit = committed
            && (self.current.as_ref() == Some(&key)
                || existing
                    .as_ref()
                    .zip(previous.as_ref())
                    .is_some_and(|(existing, previous)| Rc::ptr_eq(previous, &existing.document)));
        let document = if same_document_commit {
            // Reload replaces the document behind same-document aliases too.
            // Invalidate their shared old value instead of replaying departed
            // pixels later; only the freshly observed target receives new data.
            if let Some(record) = &existing {
                *record.document.borrow_mut() = None;
            }
            Rc::default()
        } else if let Some(record) = existing {
            record.document
        } else if !committed {
            // A change observed without a document commit is same-document.
            // Reuse only the previously observed native entry's document slot,
            // including its pending result; URLs never establish lineage.
            previous.unwrap_or_default()
        } else {
            Rc::default()
        };
        let value = document.borrow().clone();
        self.current_document = Rc::downgrade(&document);
        self.current = Some(key.clone());
        self.records.push_back(Record { key, document });
        self.bound();
        value
    }

    fn observe(&mut self, current: Option<K>, live: &[K], committed: bool) -> Option<Rc<[u8]>> {
        // replaceState may remove the prior native item. Capture its known
        // document slot before pruning, then release all orphan slots normally.
        let previous = self.current_document.upgrade();
        self.prune(live);
        match current {
            Some(key) => self.activate(key, committed, previous),
            None => {
                self.current = None;
                self.current_document = std::rc::Weak::new();
                None
            }
        }
    }

    fn retain(&mut self, bytes: Vec<u8>) {
        if let Some(record) = self
            .records
            .iter()
            .find(|record| Some(&record.key) == self.current.as_ref())
        {
            *record.document.borrow_mut() = (bytes.len() <= HISTORY_BYTES).then(|| Rc::from(bytes));
        }
        self.bound();
    }

    fn retained_bytes(&self) -> usize {
        let mut seen = HashSet::new();
        self.records
            .iter()
            .filter_map(|record| {
                let value = record.document.borrow();
                value
                    .as_ref()
                    .and_then(|bytes| seen.insert(bytes.as_ptr()).then_some(bytes.len()))
            })
            .sum()
    }

    fn bound(&mut self) {
        while self.records.len() > HISTORY_ENTRIES || self.retained_bytes() > HISTORY_BYTES {
            self.records.pop_front();
        }
    }
}

#[derive(Clone)]
struct NativeItem(Retained<WKBackForwardListItem>);
impl PartialEq for NativeItem {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(&*self.0, &*other.0)
    }
}
impl Eq for NativeItem {}

pub(super) struct IconState {
    // Never create WKWebView -> delegate -> sink -> WKWebView ownership cycles.
    view: Weak<WKWebView>,
    history: History<NativeItem>,
}

impl IconState {
    pub(super) fn new(view: &WKWebView) -> Self {
        Self {
            view: Weak::new(view),
            history: History::default(),
        }
    }

    pub(super) fn observe(
        &mut self,
        committed: bool,
        allow_activation: bool,
    ) -> Option<FaviconImage> {
        let view = self.view.load()?;
        // SAFETY: native callbacks run on this WKWebView's main thread.
        let (current, live) = unsafe {
            let list = view.backForwardList();
            let current = list.currentItem().map(NativeItem);
            let mut live: Vec<_> = list
                .backList()
                .to_vec()
                .into_iter()
                .map(NativeItem)
                .collect();
            live.extend(list.forwardList().to_vec().into_iter().map(NativeItem));
            live.extend(current.iter().cloned());
            (current, live)
        };
        // Membership is authoritative even when activation is guarded. Always
        // prune before a late result can trigger byte-budget eviction.
        // SAFETY: this is the retained owning-thread WKWebView.
        let retained = self.history.reconcile(
            current,
            live,
            committed,
            allow_activation && !unsafe { view.isLoading() },
        );
        if committed {
            retained.and_then(|bytes| decode_bytes(&bytes))
        } else {
            None
        }
    }

    pub(super) fn retain(&mut self, bytes: Vec<u8>, observe: bool) {
        self.observe(false, observe);
        self.history.retain(bytes);
    }

    pub(super) fn clear(&mut self) {
        self.history = History::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat, RgbaImage};

    #[test]
    fn native_boundaries_preserve_all_silent_aliases_and_reload_invalidates_them() {
        let mut history = History::default();
        history.reconcile(Some(1), vec![1], true, true);
        history.retain(vec![7]);
        history.reconcile(Some(3), vec![1, 2, 3], false, true);
        assert_eq!(history.retained_bytes(), 1);
        history.reconcile(Some(4), vec![1, 2, 3, 4], true, true);
        assert_eq!(
            &*history
                .reconcile(Some(2), vec![1, 2, 3, 4], true, true)
                .unwrap(),
            &[7]
        );
        assert_eq!(
            history.reconcile(Some(2), vec![1, 2, 3, 4], true, true),
            None
        );
        assert_eq!(
            history.reconcile(Some(3), vec![1, 2, 3, 4], true, true),
            None
        );
    }

    #[test]
    fn evicted_native_identity_is_not_a_new_alias_before_foreign_commit() {
        let mut history = History::default();
        for key in 1..=3 {
            history.reconcile(Some(key), (1..=key).collect(), true, true);
            history.retain(vec![key as u8; HISTORY_BYTES / 2]);
        }
        assert!(history.records.iter().all(|record| record.key != 1));
        assert_eq!(history.reconcile(Some(1), vec![1, 2, 3], false, true), None);
        assert_eq!(history.current, Some(3));
        assert_eq!(history.reconcile(Some(1), vec![1, 2, 3], true, true), None);
        assert_eq!(
            &*history
                .reconcile(Some(3), vec![1, 2, 3], true, true)
                .unwrap(),
            &[3; HISTORY_BYTES / 2]
        );
    }

    #[test]
    fn guarded_observation_prunes_membership_and_images_before_retention() {
        let mut history = History::default();
        for key in 1..=3 {
            history.reconcile(Some(key), (1..=key).collect(), true, true);
            history.retain(vec![key as u8; HISTORY_BYTES / 4]);
        }
        history.reconcile(Some(2), vec![1, 2, 3], true, true);
        history.reconcile(Some(4), vec![1, 2, 4], false, false);
        assert_eq!(history.last_live, vec![1, 2]);
        assert!(history
            .records
            .iter()
            .all(|record| record.key != 3 && record.key != 4));
        history.retain(vec![5; HISTORY_BYTES / 2]);
        assert!(history.records.iter().any(|record| record.key == 1));
        history.reconcile(Some(4), vec![1, 2, 4], true, true);
        assert_eq!(history.last_live, vec![1, 2, 4]);
    }

    #[test]
    fn native_membership_is_separate_from_image_record_budget_and_releases_on_reset() {
        let mut history = History::default();
        history.reconcile(Some(0), vec![0], true, true);
        history.retain(vec![1]);
        let live: Vec<_> = (0..HISTORY_ENTRIES + 9).collect();
        history.reconcile(Some(HISTORY_ENTRIES + 8), live.clone(), false, true);
        assert_eq!(history.last_live, live);
        assert_eq!(history.records.len(), HISTORY_ENTRIES);
        assert_eq!(history.retained_bytes(), 1);
        history.reconcile(None, vec![], false, false);
        assert!(history.last_live.is_empty());
        assert!(history.records.is_empty());
        assert_eq!(history.retained_bytes(), 0);
    }

    fn encode(width: u32, height: u32, bytes: Vec<u8>, format: ImageFormat) -> Vec<u8> {
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(RgbaImage::from_raw(width, height, bytes).unwrap())
            .write_to(&mut encoded, format)
            .unwrap();
        encoded.into_inner()
    }

    #[test]
    fn old_metadata_cannot_release_or_retire_a_new_document_queue() {
        use super::super::SystemCallbacks;
        use std::cell::Cell;
        let callbacks = Rc::new(RefCell::new(SystemCallbacks::new(std::sync::Arc::new(
            || {},
        ))));
        let sink = SystemCallbackSink::register_raw(callbacks.clone(), 1, 1);
        sink.document_committed();
        let old = sink.begin_favicon().unwrap();
        sink.document_committed();
        let current = sink.begin_favicon().unwrap();
        let calls = Rc::new(Cell::new(0));
        let count = calls.clone();
        let decision = RcBlock::new(move |data: *mut DataCompletion| {
            assert!(data.is_null());
            count.set(count.get() + 1);
        });
        callbacks
            .borrow_mut()
            .views
            .get_mut(&sink.target)
            .unwrap()
            .favicon_selection
            .hold(Pending {
                key: None,
                decision,
            })
            .ok()
            .unwrap();
        finish_collection(&sink, old, Some("[]"));
        sink.favicon_lifetime_error(old);
        assert_eq!(calls.get(), 0);
        assert!(sink.favicon_is_current(current));
        sink.document_committed();
        assert_eq!(calls.get(), 1);
        finish_collection(&sink, current, None);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn retirement_consumes_held_decisions_once_without_callback_borrows() {
        use super::super::{NativeEvaluationError, SystemCallbacks};
        use std::cell::Cell;
        for reason in ["commit", "process", "evaluation", "metadata", "destroy"] {
            let callbacks = Rc::new(RefCell::new(SystemCallbacks::new(std::sync::Arc::new(
                || {},
            ))));
            let sink = SystemCallbackSink::register_raw(callbacks.clone(), 1, 1);
            sink.document_committed();
            sink.drain();
            let epoch = sink.begin_favicon().unwrap();
            let calls = Rc::new(Cell::new(0));
            let count = calls.clone();
            let reentrant = callbacks.clone();
            let decision = RcBlock::new(move |data: *mut DataCompletion| {
                assert!(data.is_null());
                let _borrow = reentrant.borrow_mut();
                count.set(count.get() + 1);
            });
            callbacks
                .borrow_mut()
                .views
                .get_mut(&sink.target)
                .unwrap()
                .favicon_selection
                .hold(Pending {
                    key: None,
                    decision,
                })
                .ok()
                .unwrap();
            match reason {
                "commit" => sink.document_committed(),
                "process" => sink.process_terminated(),
                "evaluation" => {
                    let token = sink.begin_evaluation("native").unwrap().unwrap();
                    sink.complete_evaluation(
                        "native",
                        token,
                        Err(NativeEvaluationError::ViewInvalidated),
                    );
                }
                "metadata" => sink.favicon_lifetime_error(epoch),
                "destroy" => sink.invalidate(),
                _ => unreachable!(),
            }
            assert_eq!(calls.get(), 1, "{reason}");
            finish_collection(&sink, epoch, Some("[]"));
            sink.favicon_lifetime_error(epoch);
            assert_eq!(calls.get(), 1);
        }
    }

    #[test]
    fn native_pruning_precedes_budget_eviction_while_activation_is_guarded() {
        let mib = 1024 * 1024;
        let mut history = History::default();
        for key in 1..=3 {
            history.observe(Some(key), &[1, 2, 3], true);
            history.retain(vec![key as u8; 2 * mib]);
        }
        history.observe(Some(2), &[1, 2, 3], true); // Back to B
        history.observe(Some(2), &[1, 2, 3], true); // reload clears B
                                                    // Failed provisional guard holds activation, while pushState removes C.
        history.prune(&[1, 2, 4]);
        history.retain(vec![5; 5 * mib]);
        assert_eq!(history.retained_bytes(), 7 * mib);
        assert!(history.records.iter().any(|record| record.key == 1));
        assert!(history.records.iter().all(|record| record.key != 3));
        assert_eq!(
            &*history.observe(Some(1), &[1, 2, 4], true).unwrap(),
            &vec![1; 2 * mib]
        );
    }

    #[test]
    fn dead_departing_slot_does_not_allow_early_foreign_target_activation() {
        let mut history = History::default();
        history.observe(Some(1), &[1], true);
        history.retain(vec![1]);
        history.observe(Some(2), &[1, 2], true);
        history.retain(vec![2]);
        history.prune(&[1]);
        assert!(history.current_document.upgrade().is_none());
        assert_eq!(history.observe(Some(1), &[1], false), None);
        assert_eq!(history.current, None);
        assert_eq!(&*history.observe(Some(1), &[1], true).unwrap(), &[1]);
    }

    #[test]
    fn reload_classification_survives_filtered_departing_alias() {
        let mut history = History::default();
        history.observe(Some(1), &[1], true);
        history.retain(vec![1]);
        history.observe(Some(2), &[1, 2], false);
        assert_eq!(history.observe(Some(1), &[1], true), None);
        assert_eq!(history.retained_bytes(), 0);
    }

    #[test]
    fn prune_only_preserves_known_lineage_but_releases_orphan_bytes() {
        let mut history = History::default();
        history.observe(Some(1), &[1], true);
        history.retain(vec![1]);
        history.observe(Some(2), &[1, 2], false);
        history.prune(&[1]);
        assert_eq!(history.current, None);
        assert_eq!(history.observe(Some(1), &[1], true), None);
        history.retain(vec![2]);
        let weak = Rc::downgrade(
            history
                .records
                .back()
                .unwrap()
                .document
                .borrow()
                .as_ref()
                .unwrap(),
        );
        history.prune(&[]);
        assert!(weak.upgrade().is_none());
        assert!(history.current_document.upgrade().is_none());
        assert_eq!(history.observe(Some(3), &[3], true), None);
    }

    #[test]
    fn history_replays_identity_and_reload_retires_the_old_document() {
        let mut history = History::default();
        assert_eq!(history.observe(Some(1), &[1], true), None);
        history.retain(vec![1]);
        assert_eq!(history.observe(Some(2), &[1, 2], true), None);
        history.retain(vec![2]);
        assert_eq!(&*history.observe(Some(1), &[1, 2], true).unwrap(), &[1]);
        assert_eq!(&*history.observe(Some(2), &[1, 2], true).unwrap(), &[2]);
        assert_eq!(history.observe(Some(2), &[1, 2], true), None);
        assert_eq!(history.retained_bytes(), 1);
        // An unobserved native entry is a safe miss, even for duplicate URLs.
        assert_eq!(history.observe(Some(3), &[1, 2, 3], true), None);
    }

    #[test]
    fn early_native_history_target_does_not_become_the_active_document() {
        let mut history = History::default();
        history.observe(Some(1), &[1], true);
        history.retain(vec![1]);
        history.observe(Some(2), &[1, 2], true);
        history.retain(vec![2]);
        assert_eq!(history.observe(Some(1), &[1, 2], false), None);
        assert_eq!(history.current, Some(2));
        assert_eq!(&*history.observe(Some(1), &[1, 2], true).unwrap(), &[1]);
    }

    #[test]
    fn same_document_entries_share_pending_bytes_and_reload_separates_them() {
        let mut history = History::default();
        history.observe(Some(1), &[1], true);
        history.observe(Some(2), &[1, 2], false);
        history.retain(vec![7; 8]);
        assert_eq!(history.retained_bytes(), 8);
        assert_eq!(&*history.observe(Some(1), &[1, 2], false).unwrap(), &[7; 8]);
        history.observe(Some(1), &[1, 2], true); // reload
        history.retain(vec![8; 4]);
        assert_eq!(history.retained_bytes(), 4);
        assert_eq!(history.observe(Some(2), &[1, 2], true), None);
        history.observe(Some(2), &[2], false);
        assert_eq!(history.retained_bytes(), 0); // native-removed entry orphan freed
    }

    #[test]
    fn reload_after_unobserved_same_document_back_invalidates_its_entry() {
        let mut history = History::default();
        history.observe(Some(1), &[1], true);
        history.observe(Some(2), &[1, 2], false);
        history.retain(vec![1]);
        history.observe(Some(3), &[1, 2, 3], true);
        history.observe(Some(2), &[1, 2, 3], true);
        // Native same-document Back can happen without a commit notification;
        // policy conservatively keeps the last observed sibling active.
        assert_eq!(history.observe(Some(1), &[1, 2, 3], true), None);
        history.retain(vec![2]);
        assert_eq!(history.observe(Some(2), &[1, 2, 3], true), None);
    }

    #[test]
    fn same_document_replacement_keeps_known_data_after_native_pruning() {
        let mut history = History::default();
        history.observe(Some(1), &[1], true);
        history.retain(vec![3; 4]);
        history.observe(Some(2), &[2], false);
        assert_eq!(history.records.len(), 1);
        assert_eq!(history.retained_bytes(), 4);
        history.observe(Some(3), &[2, 3], true);
        assert_eq!(&*history.observe(Some(2), &[2, 3], true).unwrap(), &[3; 4]);
    }

    #[test]
    fn byte_budget_evicts_lru_and_oversized_values_are_not_retained() {
        let mut history = History::default();
        for key in 1..=3 {
            history.observe(Some(key), &[1, 2, 3], true);
            history.retain(vec![key as u8; HISTORY_BYTES / 2]);
        }
        assert_eq!(history.retained_bytes(), HISTORY_BYTES);
        assert_eq!(history.records.len(), 2);
        assert_eq!(history.observe(Some(1), &[1, 2, 3], true), None);
        history.retain(vec![9; HISTORY_BYTES + 1]);
        assert_eq!(history.observe(Some(1), &[1, 2, 3], true), None);
        assert!(history.retained_bytes() <= HISTORY_BYTES);
        history.observe(None, &[], false);
        assert!(history.records.is_empty());
        assert_eq!(history.retained_bytes(), 0);
    }

    #[test]
    fn activation_updates_lru_and_teardown_releases_shared_data() {
        let mut history = History::default();
        for key in 1..=2 {
            history.observe(Some(key), &[1, 2, 3], true);
            history.retain(vec![key as u8; HISTORY_BYTES / 2]);
        }
        history.observe(Some(1), &[1, 2, 3], true); // most recently activated
        history.observe(Some(3), &[1, 2, 3], true);
        history.retain(vec![3; HISTORY_BYTES / 2]);
        assert!(history.records.iter().all(|record| record.key != 2));
        assert_eq!(history.records.len(), 2);
        let weak = Rc::downgrade(
            history
                .records
                .back()
                .unwrap()
                .document
                .borrow()
                .as_ref()
                .unwrap(),
        );
        history.observe(Some(4), &[1, 3, 4], false);
        assert_eq!(history.retained_bytes(), HISTORY_BYTES);
        drop(history);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn metadata_is_bounded_even_when_all_entries_share_one_blob() {
        let mut history = History::default();
        let live: Vec<_> = (0..=HISTORY_ENTRIES).collect();
        history.observe(Some(0), &live, true);
        history.retain(vec![1; HISTORY_BYTES]);
        for key in 1..=HISTORY_ENTRIES {
            history.observe(Some(key), &live, false);
        }
        assert_eq!(history.records.len(), HISTORY_ENTRIES);
        assert_eq!(history.retained_bytes(), HISTORY_BYTES);
        assert_eq!(history.records.front().unwrap().key, 1);
        assert!(history
            .observe(Some(HISTORY_ENTRIES), &live, true)
            .is_none());
        // Reload invalidates the old document's data across all known aliases.
        assert_eq!(history.retained_bytes(), 0);
        history.observe(Some(HISTORY_ENTRIES), &[HISTORY_ENTRIES], false);
        assert_eq!(history.retained_bytes(), 0);
    }

    #[test]
    fn png_and_ico_preserve_row_order_channels_and_straight_alpha() {
        let pixels = vec![
            255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 255, 128, 64, 64,
        ];
        for format in [ImageFormat::Png, ImageFormat::Ico] {
            let encoded = encode(2, 2, pixels.clone(), format);
            let decoded = decode_bytes(&encoded).unwrap();
            assert_eq!(
                decoded,
                FaviconImage {
                    width: 2,
                    height: 2,
                    format: FaviconPixelFormat::RGBA8,
                    bytes: pixels.clone()
                },
                "{format:?}"
            );
            let native = NSData::with_bytes(&encoded);
            assert_eq!(decode(&native), Some(decoded));
        }
    }

    #[test]
    fn animated_icon_reports_only_the_first_frame() {
        let first = vec![255, 0, 0, 255];
        let mut encoded = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut encoded);
            for pixels in [first.clone(), vec![0, 0, 255, 255]] {
                encoder
                    .encode_frame(image::Frame::new(
                        RgbaImage::from_raw(1, 1, pixels).unwrap(),
                    ))
                    .unwrap();
            }
        }
        assert_eq!(decode_bytes(&encoded).unwrap().bytes, first);
    }

    #[test]
    fn malformed_and_oversized_images_do_not_produce_pixels() {
        for bytes in [&b""[..], &b"not an icon"[..], &b"\x89PNG\r\n\x1a\n"[..]] {
            assert_eq!(decode_bytes(bytes), None);
        }
        assert_eq!(decode_bytes(&vec![0; MAX_ENCODED_BYTES + 1]), None);
        let oversized = encode(2049, 2048, vec![0; 2049 * 2048 * 4], ImageFormat::Png);
        assert!(oversized.len() < MAX_ENCODED_BYTES);
        assert_eq!(decode_bytes(&oversized), None);
        let valid = encode(2, 2, vec![128; 16], ImageFormat::Png);
        assert_eq!(decode_bytes(&valid[..valid.len() / 2]), None);
    }

    #[test]
    fn missing_private_selector_keeps_icon_loading_unavailable() {
        assert!(!icon_loading_available(&NSObject::new()));
    }
}
