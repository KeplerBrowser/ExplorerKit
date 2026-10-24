use objc2::{rc::Retained, MainThreadOnly};
#[allow(deprecated)]
use objc2_web_kit::{WKProcessPool, WKWebViewConfiguration, WKWebsiteDataStore};

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
