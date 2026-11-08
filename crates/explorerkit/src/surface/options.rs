use explorerkit_host::SurfaceError;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// Clipboard services exposed through the ExplorerKit facade and adapted to Servo internally.
pub trait SurfaceClipboard {
    /// Clears clipboard text, defaulting to a successful no-op.
    fn clear_text(&self) -> Result<(), SurfaceError> {
        Ok(())
    }

    /// Returns clipboard text, defaulting to an empty string.
    fn get_text(&self) -> Result<String, SurfaceError> {
        Ok(String::new())
    }

    /// Replaces clipboard text, defaulting to a successful no-op.
    fn set_text(&self, _text: String) -> Result<(), SurfaceError> {
        Ok(())
    }
}

/// In-memory clipboard suitable for simple hosts and tests.
#[derive(Debug, Default)]
pub struct MemoryClipboard {
    text: RefCell<String>,
}

impl SurfaceClipboard for MemoryClipboard {
    fn clear_text(&self) -> Result<(), SurfaceError> {
        self.text.borrow_mut().clear();
        Ok(())
    }

    fn get_text(&self) -> Result<String, SurfaceError> {
        Ok(self.text.borrow().clone())
    }

    fn set_text(&self, text: String) -> Result<(), SurfaceError> {
        self.text.replace(text);
        Ok(())
    }
}

/// Thread-safe callback used to wake the host event loop for Servo work or queued native results.
///
/// Schedule a later update on the owning thread; do not synchronously reenter the Runtime.
/// Wake hints may be coalesced or spurious. macOS system state changes also wake the host;
/// periodic URL/history collection is unnecessary. Keep the host's animation pacing.
pub type SurfaceEventLoopWaker = Arc<dyn Fn() + Send + Sync + 'static>;

/// Event-loop, clipboard, and process-engine options supplied to `SurfaceHost` when Servo is compiled.
///
/// The macOS system host also uses the event-loop waker. Servo profile and
/// clipboard options do not configure a WebKit data store.
#[derive(Clone)]
pub struct SurfaceHostOptions {
    pub(crate) event_loop_waker: SurfaceEventLoopWaker,
    pub(crate) clipboard: Rc<dyn SurfaceClipboard>,
    pub(crate) config_directory: Option<PathBuf>,
}

impl SurfaceHostOptions {
    /// Creates host options from an event-loop waker and clipboard service.
    pub fn new(
        event_loop_waker: SurfaceEventLoopWaker,
        clipboard: Rc<dyn SurfaceClipboard>,
    ) -> Self {
        Self {
            event_loop_waker,
            clipboard,
            config_directory: None,
        }
    }

    /// Sets Servo's process-wide config directory for persistent website data.
    ///
    /// The first initialized Servo engine fixes this directory for the process.
    /// Later hosts must request the same value.
    pub fn with_config_directory(mut self, config_directory: impl Into<PathBuf>) -> Self {
        self.config_directory = Some(config_directory.into());
        self
    }

    /// Borrows the event-loop wake callback for Servo work and macOS system-view results.
    pub fn event_loop_waker(&self) -> &SurfaceEventLoopWaker {
        &self.event_loop_waker
    }

    /// Clones the reference-counted clipboard service.
    pub fn clipboard(&self) -> Rc<dyn SurfaceClipboard> {
        self.clipboard.clone()
    }

    /// Returns Servo's configured process-wide config directory, if any.
    pub fn config_directory(&self) -> Option<&Path> {
        self.config_directory.as_deref()
    }
}

impl Default for SurfaceHostOptions {
    fn default() -> Self {
        Self::new(Arc::new(|| {}), Rc::new(MemoryClipboard::default()))
    }
}
