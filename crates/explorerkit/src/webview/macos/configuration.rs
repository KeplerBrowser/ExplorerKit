use objc2::{rc::Retained, runtime::AnyClass, sel, MainThreadOnly};
#[allow(deprecated)]
use objc2_web_kit::{WKProcessPool, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore};

#[allow(
    dead_code,
    reason = "The standalone configuration test only uses the context type."
)]
pub(super) fn named_stores_available() -> bool {
    AnyClass::get(c"WKWebsiteDataStore")
        .is_some_and(|class| class.class_method(sel!(dataStoreForIdentifier:)).is_some())
}

#[allow(
    dead_code,
    reason = "The standalone configuration test only uses the context type."
)]
pub(super) fn selected_store(
    view: &WKWebView,
    expected: [u8; 16],
) -> Option<Retained<WKWebsiteDataStore>> {
    // SAFETY: The live view and its configuration belong to AppKit's thread.
    // The caller has checked that named stores are available on this system.
    unsafe {
        let store = view.configuration().websiteDataStore();
        let identifier = store.identifier()?;
        (identifier.as_bytes() == expected).then_some(store)
    }
}

// WKProcessPool still owns WebKit's font-map cache on the supported native path.
#[allow(deprecated)]
pub(super) struct SystemWebViewContext {
    process_pool: Retained<WKProcessPool>,
    website_data_store: Retained<WKWebsiteDataStore>,
}

#[allow(deprecated)]
impl SystemWebViewContext {
    pub(super) fn from_configuration(configuration: &WKWebViewConfiguration) -> Self {
        // SAFETY: The configuration and its retained objects belong to AppKit's thread.
        unsafe {
            Self {
                process_pool: configuration.processPool(),
                website_data_store: configuration.websiteDataStore(),
            }
        }
    }

    pub(super) fn configuration(&self) -> Retained<WKWebViewConfiguration> {
        // SAFETY: All objects are retained on AppKit's thread. Only the native pool
        // and selected store are shared; preferences and content controllers stay fresh.
        unsafe {
            let configuration = WKWebViewConfiguration::new(self.process_pool.mtm());
            configuration.setProcessPool(&self.process_pool);
            configuration.setWebsiteDataStore(&self.website_data_store);
            configuration
        }
    }
}
