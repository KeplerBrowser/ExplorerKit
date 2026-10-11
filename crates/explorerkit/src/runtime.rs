//! Runtime/session facade exports.

pub use explorerkit_embedder::{
    NewWebViewDecision, NewWebViewRequest, Runtime, RuntimeError, RuntimeError as ExplorerKitError,
    SessionHandle,
};

#[cfg(all(
    feature = "servo",
    any(
        target_os = "android",
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    )
))]
pub use explorerkit_embedder::{
    ensure_default_rustls_crypto_provider, install_rustls_crypto_provider,
    RustlsCryptoProviderStatus,
};
