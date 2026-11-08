use cocoa::{
    appkit::{
        NSApp, NSApplication, NSApplicationActivationPolicyRegular, NSBackingStoreBuffered,
        NSEventMask, NSView, NSWindow, NSWindowStyleMask,
    },
    base::{id, nil, NO, YES},
    foundation::{
        NSAutoreleasePool, NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString,
    },
};
use explorerkit::{
    surface::{
        NativeSurface, SurfaceDelegate, SurfaceError, SurfaceFrame, SurfaceHostOptions,
        SurfaceTarget,
    },
    webview::macos::{
        automation::{WebKitAutomation, WebKitAutomationNewViewRequestId, WebKitAutomationStatus},
        AppKitNewWebViewDestination, AppKitViewDelegate, MacOsViewHost, MacOsViewHostOptions,
        MacOsWebViewKind, MacOsWebViewOptions, WebKitDataStoreIdentifier,
    },
    HostEvent, HostSurface, Runtime, SessionHandle, SurfaceSize, SurfaceViewport, WebViewHandle,
};
use raw_window_handle::{AppKitWindowHandle, DisplayHandle, RawWindowHandle, WindowHandle};
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io::{BufRead, Write},
    os::{
        fd::AsRawFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    process::{Child, Command, Stdio},
    ptr::NonNull,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Instant,
};

type NativeRuntime = Runtime<MacOsViewHost<Slots>>;
#[derive(Clone, Default)]
struct Slots {
    parents: Rc<RefCell<HashMap<String, id>>>,
    fail_attach: Rc<Cell<bool>>,
}
impl SurfaceDelegate for Slots {
    type Frame<'a> = SurfaceFrame<'a>;
    fn render_target(
        &mut self,
        _: &HostSurface,
        _: SurfaceViewport,
    ) -> Result<SurfaceTarget<'_>, SurfaceError> {
        Err(SurfaceError::new("system WebKit presents natively"))
    }
}
impl AppKitViewDelegate for Slots {
    fn appkit_parent(
        &mut self,
        surface: &HostSurface,
        _: SurfaceViewport,
    ) -> Result<NativeSurface<'_>, SurfaceError> {
        if self.fail_attach.replace(false) {
            return Err(SurfaceError::new("injected appkit_parent attach failure"));
        }
        let ptr = *self
            .parents
            .borrow()
            .get(surface.id())
            .ok_or_else(|| SurfaceError::new("missing app slot"))?;
        // The AppKit hierarchy retains the slot until SDK retirement completes.
        Ok(unsafe {
            NativeSurface::new(
                DisplayHandle::appkit(),
                WindowHandle::borrow_raw(RawWindowHandle::AppKit(AppKitWindowHandle::new(
                    NonNull::new(ptr.cast()).unwrap(),
                ))),
            )
        })
    }
}
struct PopupSlot(objc2::rc::Retained<objc2_app_kit::NSView>);
impl raw_window_handle::HasWindowHandle for PopupSlot {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        Ok(unsafe {
            WindowHandle::borrow_raw(RawWindowHandle::AppKit(AppKitWindowHandle::new(
                NonNull::from(&*self.0).cast(),
            )))
        })
    }
}
impl Drop for PopupSlot {
    fn drop(&mut self) {
        self.0.removeFromSuperview();
    }
}

struct Page {
    handle: WebViewHandle,
    slot: id,
    surface: HostSurface,
    closed: bool,
    popup_owner: Option<Rc<PopupSlot>>,
    native: Option<objc2::rc::Weak<objc2_web_kit::WKWebView>>,
}
type PopupSlots = Rc<RefCell<HashMap<String, (HostSurface, Rc<PopupSlot>)>>>;

struct App {
    runtime: Option<NativeRuntime>,
    automation: WebKitAutomation,
    session: SessionHandle,
    slots: Slots,
    pages: HashMap<String, Page>,
    requests: HashMap<String, WebKitAutomationNewViewRequestId>,
    next_request: u64,
    window: id,
    helper: Option<Child>,
    driver_port: u16,
    selected: Rc<Cell<Option<u64>>>,
    selection_slots: Rc<RefCell<HashMap<u64, id>>>,
    wake: Arc<AtomicBool>,
    popup_slots: PopupSlots,
    background_pending: bool,
}
// Public AppKit inspection is diagnostic only. Never retain another native owner
// in the host's view table: SDK retirement must release the last WKWebView owner.
fn native_identity(
    slot: id,
    known: &mut Option<objc2::rc::Weak<objc2_web_kit::WKWebView>>,
) -> Value {
    use objc2::{ClassType, MainThreadMarker};
    use objc2_foundation::NSObjectProtocol;
    fn find(view: &objc2_app_kit::NSView) -> Option<objc2::rc::Retained<objc2_web_kit::WKWebView>> {
        for child in view.subviews() {
            if child.isKindOfClass(objc2_web_kit::WKWebView::class()) {
                return Some(unsafe {
                    objc2::rc::Retained::retain(
                        (&*child as *const objc2_app_kit::NSView).cast_mut().cast(),
                    )
                    .unwrap()
                });
            }
            if let Some(found) = find(&child) {
                return Some(found);
            }
        }
        None
    }
    let _ = MainThreadMarker::new().unwrap();
    let Some(view) = find(unsafe { &*slot.cast::<objc2_app_kit::NSView>() })
        .or_else(|| known.as_ref().and_then(objc2::rc::Weak::load))
    else {
        return Value::Null;
    };
    *known = Some(objc2::rc::Weak::new(&view));
    unsafe {
        let configuration = view.configuration();
        json!({"native":format!("{:p}", &*view),"pool":format!("{:p}", &*configuration.processPool()),"store":format!("{:p}", &*configuration.websiteDataStore()),"visible":!view.isHiddenOrHasHiddenAncestor() && view.window().is_some_and(|w|w.isVisible()),"windowActive":view.window().is_some_and(|w|w.isKeyWindow())})
    }
}
fn emit(value: Value) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}
impl App {
    fn new(window: id) -> Self {
        let slots = Slots::default();
        let selected = Rc::new(Cell::new(None));
        let selection_slots = Rc::new(RefCell::new(HashMap::<u64, id>::new()));
        let wake = Arc::new(AtomicBool::new(true));
        let signal = wake.clone();
        let mut options = MacOsViewHostOptions::new(
            SurfaceHostOptions::new(
                Arc::new(move || {
                    signal.store(true, Ordering::Release);
                }),
                SurfaceHostOptions::default().clipboard(),
            ),
            WebKitDataStoreIdentifier(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
                    .to_be_bytes(),
            ),
        );
        if std::env::var_os("EXPLORERKIT_SELECTION_HOOK").is_some() {
            let selected = selected.clone();
            let slots = selection_slots.clone();
            options = options.with_webkit_automation_view_selection_handler(Rc::new(move |view| {
                // Host presentation only: no Runtime/control reentry or app activation.
                if let Some(&slot) = slots.borrow().get(&view.raw()) {
                    unsafe {
                        (&*slot.cast::<objc2_app_kit::NSView>()).setHidden(false);
                    }
                }
                selected.set(Some(view.raw()));
                emit(json!({"event":"selected", "sdk":view.raw()}));
            }));
        }
        let popup_slots = Rc::new(RefCell::new(HashMap::new()));
        let staged = popup_slots.clone();
        options = options.with_new_webview_destination_handler(Rc::new(move |request| {
            // This host-owned placement callback uses no Runtime/control borrow.
            let slot = unsafe {
                NSView::alloc(nil)
                    .initWithFrame_(NSRect::new(NSPoint::new(0., 370.), NSSize::new(500., 360.)))
                    .autorelease()
            };
            unsafe {
                window.contentView().addSubview_(slot);
            }
            let retained = unsafe {
                objc2::rc::Retained::retain(slot.cast::<objc2_app_kit::NSView>()).unwrap()
            };
            let surface = HostSurface::new(format!("popup-slot-{}", request.request_id));
            let owner = Rc::new(PopupSlot(retained));
            staged
                .borrow_mut()
                .insert(request.request_id.clone(), (surface.clone(), owner.clone()));
            let scale = unsafe { window.backingScaleFactor() as f32 };
            Ok(AppKitNewWebViewDestination::new(
                surface,
                SurfaceViewport::new(
                    SurfaceSize::new((500. * scale) as u32, (360. * scale) as u32),
                    scale,
                ),
                owner,
            ))
        }));
        let host = MacOsViewHost::new(slots.clone(), options);
        let automation = host.webkit_automation();
        let mut runtime = Runtime::new(host);
        let session = runtime.create_session();
        Self {
            runtime: Some(runtime),
            automation,
            session,
            slots,
            pages: HashMap::new(),
            requests: HashMap::new(),
            next_request: 0,
            window,
            helper: None,
            driver_port: 0,
            selected,
            selection_slots,
            wake,
            popup_slots,
            background_pending: false,
        }
    }
    fn runtime(&mut self) -> &mut NativeRuntime {
        self.runtime.as_mut().expect("live Runtime")
    }
    fn handle(&self, name: &str) -> Result<WebViewHandle, String> {
        self.pages
            .get(name)
            .filter(|p| !p.closed)
            .map(|p| p.handle)
            .ok_or_else(|| format!("unknown view {name}"))
    }
    fn create(&mut self, command: &Value) -> Result<Value, String> {
        let name = command["name"].as_str().ok_or("missing name")?.to_owned();
        if self.pages.contains_key(&name) {
            return Err("name already exists".into());
        }
        let options = MacOsWebViewOptions::new(MacOsWebViewKind::SystemWebView)
            .with_webkit_automation(command["eligible"].as_bool().unwrap_or(true));
        let session = self.session;
        let handle = self
            .runtime()
            .create_webview_with_options(session, options)
            .map_err(|e| e.to_string())?;
        let index = self.pages.values().filter(|p| !p.closed).count();
        let slot = unsafe {
            NSView::alloc(nil)
                .initWithFrame_(NSRect::new(
                    NSPoint::new((index % 2) as f64 * 510., (index / 2) as f64 * 370.),
                    NSSize::new(500., 360.),
                ))
                .autorelease()
        };
        unsafe {
            self.window.contentView().addSubview_(slot);
        }
        let surface = HostSurface::new(name.clone());
        self.slots.parents.borrow_mut().insert(name.clone(), slot);
        self.selection_slots.borrow_mut().insert(handle.raw(), slot);
        self.pages.insert(
            name.clone(),
            Page {
                handle,
                slot,
                surface,
                closed: false,
                popup_owner: None,
                native: None,
            },
        );
        if command["attach"].as_bool() != Some(false) {
            self.slots
                .fail_attach
                .set(command["failAttach"].as_bool().unwrap_or(false));
            if let Err(error) = self.attach(&name) {
                let rollback = self.runtime().destroy_webview(handle).is_ok();
                self.remove_slot(&name);
                return Ok(
                    json!({"created":false,"sdk":handle.raw(),"error":error,"rollback":rollback}),
                );
            }
        }
        if let Some(url) = command["url"].as_str() {
            self.runtime()
                .load_url(handle, url)
                .map_err(|e| e.to_string())?;
        }
        Ok(json!({"created":true,"sdk":handle.raw()}))
    }
    fn attach(&mut self, name: &str) -> Result<(), String> {
        let page = self.pages.get(name).ok_or("unknown view")?;
        let (handle, surface) = (page.handle, page.surface.clone());
        let scale = unsafe { self.window.backingScaleFactor() as f32 };
        self.runtime()
            .attach_surface_with_viewport(
                handle,
                surface,
                SurfaceViewport::new(
                    SurfaceSize::new((500. * scale) as u32, (360. * scale) as u32),
                    scale,
                ),
            )
            .map_err(|e| e.to_string())
    }
    fn remove_slot(&mut self, name: &str) {
        if let Some(page) = self.pages.get_mut(name).filter(|p| !p.closed) {
            unsafe {
                page.slot.removeFromSuperview();
            }
            self.slots.parents.borrow_mut().remove(page.surface.id());
            self.selection_slots.borrow_mut().remove(&page.handle.raw());
            page.closed = true;
            page.slot = nil;
            page.popup_owner.take();
        }
    }
    fn pump_presentation(&mut self) {
        if self.background_pending {
            unsafe {
                let app = &*NSApp().cast::<objc2_app_kit::NSApplication>();
                // Hide completes asynchronously. Restore the existing window only
                // after its inactive/hidden transition, without making it key.
                if !app.isActive() && app.isHidden() {
                    app.unhideWithoutActivation();
                    self.window.orderFrontRegardless();
                    self.background_pending = false;
                }
            }
        }
    }
    fn pump(&mut self) {
        if self.runtime.is_none() || !self.wake.swap(false, Ordering::AcqRel) {
            return;
        }
        if let Err(error) = self.runtime().perform_all_updates() {
            emit(json!({"event":"hostError","error":error.to_string()}));
        }
        for event in self.runtime().drain_events() {
            if let (Some(child), HostEvent::PopupCreated { request_id, .. }) =
                (event.created_webview, &event.event)
            {
                if let Some((surface, owner)) = self.popup_slots.borrow_mut().remove(request_id) {
                    let slot = objc2::rc::Retained::as_ptr(&owner.0).cast_mut().cast();
                    self.selection_slots.borrow_mut().insert(child.raw(), slot);
                    self.pages.insert(
                        format!("popup-{}", child.raw()),
                        Page {
                            handle: child,
                            slot,
                            surface,
                            closed: false,
                            popup_owner: Some(owner),
                            native: None,
                        },
                    );
                }
            }
            let mut value = json!({"event":"sdkEvent","sdk":event.webview.raw(),"detail":format!("{:?}", event.event)});
            match &event.event {
                HostEvent::PopupCreated { .. } => {
                    value["createdSdk"] = json!(event.created_webview.map(|v| v.raw()));
                    value["popupName"] =
                        json!(event.created_webview.map(|v| format!("popup-{}", v.raw())));
                }
                HostEvent::PopupCreationFailed { request_id, .. } => {
                    self.popup_slots.borrow_mut().remove(request_id);
                }
                HostEvent::SimpleDialogRequested {
                    dialog_id,
                    kind,
                    message,
                    default_value,
                } => {
                    value["dialog"] = json!({"id":dialog_id,"kind":kind.as_str(),"message":message,"default":default_value});
                }
                HostEvent::SimpleDialogDismissed { dialog_id } => {
                    value["dismissed"] = json!(dialog_id);
                }
                HostEvent::JavaScriptEvaluationResult {
                    evaluation_id,
                    ok,
                    value_json,
                    error_type,
                } => {
                    value["evaluation"] = json!({"id":evaluation_id,"ok":ok,"valueJson":value_json,"errorType":format!("{error_type:?}")});
                }
                HostEvent::Closed => {
                    let names: Vec<_> = self
                        .pages
                        .iter()
                        .filter(|(_, p)| p.handle == event.webview && !p.closed)
                        .map(|(n, _)| n.clone())
                        .collect();
                    for name in names {
                        self.remove_slot(&name);
                    }
                    value["closed"] = json!(true);
                }
                _ => {}
            }
            emit(value);
        }
        match self.automation.drain_new_view_requests() {
            Ok(requests) => {
                for request in requests {
                    self.next_request += 1;
                    let token = format!("request-{}", self.next_request);
                    self.requests.insert(token.clone(), request.id);
                    emit(
                        json!({"event":"newViewRequest","token":token,"preferNewTab":request.prefer_new_tab}),
                    );
                }
            }
            Err(error) => emit(json!({"event":"hostError","error":error.to_string()})),
        }
    }
    fn connect(&mut self) -> Result<Value, String> {
        let (sdk, helper) = UnixStream::pair().map_err(|e| e.to_string())?;
        self.automation
            .connect(sdk.into())
            .map_err(|e| e.to_string())?;
        self.stop_helper();
        let path = std::env::var("EXPLORERKIT_MINIDRIVER")
            .map_err(|_| "EXPLORERKIT_MINIDRIVER is required")?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        self.driver_port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut command = Command::new(path);
        command
            .args([
                format!("--port={}", self.driver_port),
                "--host=local".into(),
                "--bidi-port=0".into(),
                "--attach-fd=3".into(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let descriptor = helper.as_raw_fd();
        // Only async-signal-safe descriptor operations run between fork and exec.
        unsafe {
            command.pre_exec(move || {
                unsafe extern "C" {
                    fn dup2(old: i32, new: i32) -> i32;
                    fn fcntl(fd: i32, command: i32, value: i32) -> i32;
                }
                if dup2(descriptor, 3) < 0 || fcntl(3, 2, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        match command.spawn() {
            Ok(child) => {
                let pid = child.id();
                self.helper = Some(child);
                Ok(
                    json!({"driverUrl":format!("http://127.0.0.1:{}", self.driver_port),"driverPid":pid}),
                )
            }
            Err(error) => {
                let _ = self.automation.disconnect();
                Err(error.to_string())
            }
        }
    }
    fn stop_helper(&mut self) {
        if let Some(mut helper) = self.helper.take() {
            let pid = helper.id();
            let _ = helper.kill();
            let result = helper.wait();
            emit(json!({"event":"helperReaped","pid":pid,"result":format!("{result:?}")}));
        }
    }
    fn snapshot(&mut self) -> Value {
        let views: Vec<_> = self.pages.iter_mut().filter(|(_,p)| !p.closed).map(|(name,page)| {
            let context = self.automation.context(page.handle);
            json!({"name":name,"sdk":page.handle.raw(),"nativeIdentity":native_identity(page.slot, &mut page.native),"slot":format!("{:p}",page.slot),"context":context.as_ref().ok().and_then(|v|v.as_ref()).map(|c|json!({"sessionId":c.session_id,"handle":c.handle})),"contextError":context.err().map(|e|e.to_string())})
        }).collect();
        let status = self.automation.status();
        let status = match status {
            Ok(WebKitAutomationStatus::Disconnected) => json!({"state":"disconnected"}),
            Ok(WebKitAutomationStatus::AwaitingSession) => json!({"state":"awaitingSession"}),
            Ok(WebKitAutomationStatus::Active { session_id }) => {
                json!({"state":"active","sessionId":session_id})
            }
            Ok(WebKitAutomationStatus::Failed { message }) => {
                json!({"state":"failed","message":message})
            }
            Err(error) => json!({"error":error.to_string()}),
        };
        json!({"views":views,"status":status,"selected":self.selected.get(),"hostPid":std::process::id(),"appActive":unsafe { (&*NSApp().cast::<objc2_app_kit::NSApplication>()).isActive() }})
    }
    fn command(&mut self, command: &Value) -> Result<Value, String> {
        let name = command["name"].as_str().unwrap_or("A");
        match command["command"].as_str().ok_or("missing command")? {
            "create" => self.create(command),
            "enablePopups" => {
                let view = self.handle(name)?;
                self.runtime()
                    .set_new_webview_handler(
                        view,
                        Some(Rc::new(|_| explorerkit::NewWebViewDecision::Create)),
                    )
                    .map_err(|e| e.to_string())?;
                Ok(json!({}))
            }
            "attach" => {
                self.attach(name)?;
                Ok(json!({}))
            }
            "enroll" => {
                self.automation
                    .set_view_automation(
                        self.handle(name)?,
                        command["enabled"].as_bool().unwrap_or(true),
                    )
                    .map_err(|e| e.to_string())?;
                Ok(json!({}))
            }
            "connect" => self.connect(),
            "disconnect" => {
                self.automation.disconnect().map_err(|e| e.to_string())?;
                Ok(json!({}))
            }
            "crashDriver" => {
                self.stop_helper();
                Ok(json!({}))
            }
            "snapshot" => Ok(self.snapshot()),
            "resolveBatch" => {
                let resolutions = command["resolutions"]
                    .as_array()
                    .ok_or("missing resolutions")?;
                if resolutions.len() != 2 {
                    return Err("resolveBatch requires two resolutions".into());
                }
                let prepared: Result<Vec<_>, String> = resolutions
                    .iter()
                    .map(|item| {
                        let token = *self
                            .requests
                            .get(item["token"].as_str().ok_or("missing token")?)
                            .ok_or("unknown token")?;
                        let view = item["view"]
                            .as_str()
                            .map(|name| self.handle(name))
                            .transpose()?;
                        Ok((token, view))
                    })
                    .collect();
                // Deliberately no pump, Runtime call or nested loop between these
                // control calls: the SDK must enforce its own retirement boundary.
                let results: Vec<_> = prepared?
                    .into_iter()
                    .map(|(token, view)| {
                        let result = self.automation.resolve_new_view_request(token, view);
                        json!({"ok":result.is_ok(),"error":result.err().map(|e|e.to_string())})
                    })
                    .collect();
                let disconnected = command["disconnectAfter"].as_bool().unwrap_or(false);
                if disconnected {
                    self.automation.disconnect().map_err(|e| e.to_string())?;
                }
                Ok(json!({"results":results,"disconnected":disconnected}))
            }
            "resolve" => {
                let request = *self
                    .requests
                    .get(command["token"].as_str().ok_or("missing token")?)
                    .ok_or("unknown token")?;
                let view = command["view"]
                    .as_str()
                    .map(|name| self.handle(name))
                    .transpose()?;
                self.automation
                    .resolve_new_view_request(request, view)
                    .map_err(|e| e.to_string())?;
                self.pump();
                Ok(json!({}))
            }
            "dialog" => {
                let view = self.handle(name)?;
                self.runtime()
                    .resolve_simple_dialog(
                        view,
                        command["dialogId"].as_str().ok_or("missing dialogId")?,
                        command["confirmed"].as_bool().unwrap_or(false),
                        command["value"].as_str(),
                    )
                    .map_err(|e| e.to_string())?;
                Ok(json!({}))
            }
            "eval" => {
                let view = self.handle(name)?;
                self.runtime()
                    .evaluate_javascript(
                        view,
                        command["evaluationId"]
                            .as_str()
                            .ok_or("missing evaluationId")?,
                        command["script"].as_str().ok_or("missing script")?,
                    )
                    .map_err(|e| e.to_string())?;
                Ok(json!({}))
            }
            "destroy" => {
                let view = self.handle(name)?;
                self.runtime()
                    .destroy_webview(view)
                    .map_err(|e| e.to_string())?;
                self.remove_slot(name);
                Ok(json!({}))
            }
            "detach" => {
                let view = self.handle(name)?;
                self.runtime()
                    .detach_surface(view)
                    .map_err(|e| e.to_string())?;
                Ok(json!({}))
            }
            "hideView" => {
                let page = self
                    .pages
                    .get(name)
                    .filter(|p| !p.closed)
                    .ok_or("unknown view")?;
                unsafe {
                    (&*page.slot.cast::<objc2_app_kit::NSView>()).setHidden(true);
                }
                Ok(json!({}))
            }
            "background" => {
                self.background_pending = true;
                unsafe {
                    (&*NSApp().cast::<objc2_app_kit::NSApplication>()).hide(None);
                }
                Ok(json!({}))
            }
            "foreground" => {
                unsafe {
                    NSApp().activateIgnoringOtherApps_(YES);
                    self.window.makeKeyAndOrderFront_(nil);
                }
                Ok(json!({}))
            }
            "shutdownRuntime" => {
                let runtime = self.runtime.take().ok_or("Runtime already ended")?;
                let result = runtime.shutdown().map_err(|e| e.to_string());
                self.stop_helper();
                result?;
                Ok(json!({"weakError":self.automation.status().unwrap_err().to_string()}))
            }
            "dropRuntime" => {
                drop(self.runtime.take());
                self.stop_helper();
                Ok(json!({"weakError":self.automation.status().unwrap_err().to_string()}))
            }
            _ => Err("unknown host command".into()),
        }
    }
}
impl Drop for App {
    fn drop(&mut self) {
        let _ = self.automation.disconnect();
        self.stop_helper();
        drop(self.runtime.take());
    }
}
pub fn run() {
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let app = NSApp();
        app.setActivationPolicy_(NSApplicationActivationPolicyRegular);
        let window = NSWindow::alloc(nil).initWithContentRect_styleMask_backing_defer_(
            NSRect::new(NSPoint::new(100., 100.), NSSize::new(1020., 760.)),
            NSWindowStyleMask::NSTitledWindowMask
                | NSWindowStyleMask::NSClosableWindowMask
                | NSWindowStyleMask::NSResizableWindowMask,
            NSBackingStoreBuffered,
            NO,
        );
        window.setTitle_(NSString::alloc(nil).init_str("ExplorerKit automation example"));
        window.orderFront_(nil);
        app.finishLaunching();
        let mut host = App::new(window);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else {
                    break;
                };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        emit(json!({"event":"ready","hostPid":std::process::id()}));
        let start = Instant::now();
        loop {
            let turn_pool = NSAutoreleasePool::new(nil);
            let until = NSDate::distantPast(nil);
            let event = app.nextEventMatchingMask_untilDate_inMode_dequeue_(
                NSEventMask::NSAnyEventMask.bits(),
                until,
                NSDefaultRunLoopMode,
                YES,
            );
            if !event.is_null() {
                app.sendEvent_(event);
            }
            host.pump_presentation();
            host.pump();
            loop {
                let line = match receiver.try_recv() {
                    Ok(line) => line,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        drop(host);
                        emit(json!({"event":"done","reason":"stdin closed"}));
                        window.close();
                        turn_pool.drain();
                        pool.drain();
                        return;
                    }
                };
                match serde_json::from_str::<Value>(&line) {
                    Ok(command) if command["command"] == "quit" => {
                        let id = command["id"].clone();
                        drop(host);
                        emit(
                            json!({"event":"done","id":id,"seconds":start.elapsed().as_secs_f64()}),
                        );
                        window.close();
                        turn_pool.drain();
                        pool.drain();
                        return;
                    }
                    Ok(command) => {
                        let result = host.command(&command);
                        host.wake.store(true, Ordering::Release);
                        emit(
                            json!({"event":"result","id":command["id"],"ok":result.is_ok(),"value":result.as_ref().ok(),"error":result.err()}),
                        );
                    }
                    Err(error) => emit(json!({"event":"hostError","error":error.to_string()})),
                }
            }
            turn_pool.drain();
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
    }
}
