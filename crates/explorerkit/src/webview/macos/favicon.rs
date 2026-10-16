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
use objc2_foundation::{NSData, NSObject, NSObjectProtocol};
use objc2_web_kit::{WKBackForwardListItem, WKWebView};
use wry::{WebView, WebViewExtMacOS};

use super::SystemCallbackSink;

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
            // SAFETY: _WKLinkIconParameters declares iconType as NSInteger;
            // WKLinkIconTypeFavicon is zero. WebKit owns candidate discovery.
            let icon_type: isize = unsafe { msg_send![parameters, iconType] };
            let sink = self.ivars().clone();
            let token = (icon_type == 0).then(|| sink.begin_favicon()).flatten();
            let Some(token) = token else {
                decision.call((std::ptr::null_mut(),));
                return;
            };
            let completion = RcBlock::new(move |data: *mut NSData| {
                if !sink.favicon_is_current(token) { return; }
                // SAFETY: WebKit lends NSData for this main-thread callback.
                // Decoding copies pixels; no borrowed bytes escape.
                if let Some(data) = unsafe { data.as_ref() }.filter(|data| data.len() <= MAX_ENCODED_BYTES) {
                    let bytes = data.to_vec();
                    if let Some(icon) = decode_bytes(&bytes) {
                        sink.complete_native_favicon(token, icon, bytes);
                    }
                }
            });
            // WebKit copies this data completion before returning its decision.
            decision.call((RcBlock::as_ptr(&completion),));
        }
    }
);

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
    current: Option<K>,
    // Preserve known lineage across filtered native snapshots without keeping
    // orphaned image bytes alive after their last retained record disappears.
    current_document: std::rc::Weak<RefCell<Option<Rc<[u8]>>>>,
}

impl<K> Default for History<K> {
    fn default() -> Self {
        Self {
            records: VecDeque::new(),
            current: None,
            current_document: std::rc::Weak::new(),
        }
    }
}

impl<K: Clone + Eq> History<K> {
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
        // SAFETY: callbacks and collection run on this WKWebView's main thread.
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
        if !committed && (!allow_activation || unsafe { view.isLoading() }) {
            self.history.prune(&live);
            return None;
        }
        let retained = self.history.observe(current, &live, committed);
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

    fn encode(width: u32, height: u32, bytes: Vec<u8>, format: ImageFormat) -> Vec<u8> {
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(RgbaImage::from_raw(width, height, bytes).unwrap())
            .write_to(&mut encoded, format)
            .unwrap();
        encoded.into_inner()
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
