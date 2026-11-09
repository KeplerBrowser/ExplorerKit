//! Explicit one-way native site-data transfer. Applications own selection and lifetime.
//!
//! Creating a view never copies state. Initialize before navigating the destination.
//! Servo storage is process-shared; initialization merges supplied records.

pub use explorerkit_embedder::site_data::{
    SiteDataCookie, SiteDataError, SiteDataErrorKind, SiteDataLimitation, SiteDataLocalStorage,
    SiteDataMutation, SiteDataOperation, SiteDataOrigin, SiteDataSameSite, SiteDataScope,
    SiteDataSelection, SiteDataSnapshot, SiteDataStage, SiteDataWriteSummary,
};
