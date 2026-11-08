use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{mpsc, Arc};

use core_foundation::{
    base::TCFType, boolean::CFBoolean, dictionary::CFDictionary, string::CFString,
};
use core_video::pixel_buffer::{kCVPixelFormatType_32BGRA, CVPixelBufferKeys};
use dpi::PhysicalSize;
use euclid::default::{Size2D, Size2D as UntypedSize2D};
use explorerkit_embedder::WebViewHandle;
use gleam::gl::{self, Gl};
use glow::HasContext;
use image::RgbaImage;
use objc2_core_foundation::CFRetained;
use servo::{DeviceIntRect, RefreshDriver, RenderingContext};
use surfman::default::surface::NativeSurface as SurfmanNativeSurface;
use surfman::{
    Connection, Context, ContextAttributeFlags, ContextAttributes, Device, GLApi, GLVersion,
    Surface, SurfaceAccess, SurfaceTexture, SurfaceType,
};

use super::super::{SurfaceError, SurfaceSize};

const SLOT_COUNT: u32 = 3;

use super::gpu_frame::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    Available,
    Rendering,
    InFlight { frame_serial: u64 },
}

fn is_available(slot_generation: u64, slot_state: SlotState, current_generation: u64) -> bool {
    slot_generation == current_generation && slot_state == SlotState::Available
}

fn is_retired_in_flight(
    slot_generation: u64,
    slot_state: SlotState,
    current_generation: u64,
) -> bool {
    slot_generation != current_generation && matches!(slot_state, SlotState::InFlight { .. })
}

fn blocks_new_generation(
    slots: impl IntoIterator<Item = (u64, SlotState)>,
    current_generation: u64,
) -> bool {
    slots
        .into_iter()
        .any(|(generation, state)| is_retired_in_flight(generation, state, current_generation))
}

fn matches_completion(
    slot_id: u32,
    slot_generation: u64,
    slot_state: SlotState,
    identity: FrameIdentity,
) -> bool {
    slot_id == identity.slot
        && slot_generation == identity.generation
        && slot_state
            == SlotState::InFlight {
                frame_serial: identity.frame_serial,
            }
}

struct Slot {
    id: u32,
    generation: u64,
    physical_size: SurfaceSize,
    surface: Option<Surface>,
    native_surface: SurfmanNativeSurface,
    pixel_buffer: CVPixelBuffer,
    state: SlotState,
}

struct PoolState {
    physical_size: SurfaceSize,
    generation: u64,
    next_slot: u32,
    next_frame_serial: u64,
    active_slot: Option<u32>,
    attached: bool,
    slots: Vec<Slot>,
}

pub(crate) struct ExportableRenderingContext {
    gleam_gl: Rc<dyn Gl>,
    glow_gl: Arc<glow::Context>,
    device: RefCell<Device>,
    context: RefCell<Context>,
    state: RefCell<PoolState>,
    completion_sender: mpsc::Sender<FrameIdentity>,
    completion_receiver: mpsc::Receiver<FrameIdentity>,
    wake: Arc<dyn Fn() + Send + Sync + 'static>,
    refresh_driver: Rc<dyn RefreshDriver>,
    last_error: RefCell<Option<String>>,
    cpu_reads: Cell<u64>,
    reported_size: Cell<SurfaceSize>,
}

impl ExportableRenderingContext {
    pub(crate) fn new(
        physical_size: SurfaceSize,
        wake: Arc<dyn Fn() + Send + Sync + 'static>,
        refresh_driver: Rc<dyn RefreshDriver>,
    ) -> Result<Self, SurfaceError> {
        validate_size(physical_size)?;
        let connection = Connection::new().map_err(surface_error)?;
        let adapter = connection
            .create_hardware_adapter()
            .map_err(surface_error)?;
        let device = connection.create_device(&adapter).map_err(surface_error)?;
        let gl_api = connection.gl_api();
        let version = match gl_api {
            GLApi::GLES => GLVersion::new(3, 0),
            GLApi::GL => GLVersion::new(3, 2),
        };
        let descriptor = device
            .create_context_descriptor(&ContextAttributes {
                flags: ContextAttributeFlags::ALPHA
                    | ContextAttributeFlags::DEPTH
                    | ContextAttributeFlags::STENCIL,
                version,
            })
            .map_err(surface_error)?;
        let context = device
            .create_context(&descriptor, None)
            .map_err(surface_error)?;
        let gleam_gl: Rc<dyn Gl> = match gl_api {
            GLApi::GL => unsafe {
                gl::GlFns::load_with(|name| device.get_proc_address(&context, name))
            },
            GLApi::GLES => unsafe {
                gl::GlesFns::load_with(|name| device.get_proc_address(&context, name))
            },
        };
        let glow_gl = unsafe {
            glow::Context::from_loader_function(|name| device.get_proc_address(&context, name))
        };
        let (completion_sender, completion_receiver) = mpsc::channel();
        let this = Self {
            gleam_gl,
            glow_gl: Arc::new(glow_gl),
            device: RefCell::new(device),
            context: RefCell::new(context),
            state: RefCell::new(PoolState {
                physical_size,
                generation: 1,
                next_slot: 0,
                next_frame_serial: 0,
                active_slot: None,
                attached: true,
                slots: Vec::new(),
            }),
            completion_sender,
            completion_receiver,
            wake,
            refresh_driver,
            last_error: RefCell::new(None),
            cpu_reads: Cell::new(0),
            reported_size: Cell::new(physical_size),
        };

        for _ in 0..SLOT_COUNT {
            let slot = this.create_next_slot(1, physical_size)?;
            this.state.borrow_mut().slots.push(slot);
        }
        this.bind_available_slot()?;
        Ok(this)
    }

    pub(crate) fn can_paint(&self) -> Result<bool, SurfaceError> {
        self.drain_completions();
        if let Some(error) = self.last_error.borrow_mut().take() {
            return Err(SurfaceError::new(error));
        }
        if !self.state.borrow().attached {
            return Ok(false);
        }
        {
            let state = self.state.borrow();
            if state.active_slot.is_none()
                && !state
                    .slots
                    .iter()
                    .any(|slot| is_available(slot.generation, slot.state, state.generation))
            {
                return Ok(false);
            }
        }
        self.bind_available_slot()?;
        Ok(true)
    }

    pub(crate) fn attach(&self) {
        self.state.borrow_mut().attached = true;
    }

    pub(crate) fn detach(&self) -> Result<(), SurfaceError> {
        self.drain_completions();
        let outstanding = self.outstanding_frames();
        if outstanding != 0 {
            return Err(SurfaceError::new(format!(
                "{outstanding} consumer-held GPU frame(s) are still awaiting completion"
            )));
        }
        self.recycle_unpublished()?;
        self.state.borrow_mut().attached = false;
        Ok(())
    }

    pub(crate) fn outstanding_frames(&self) -> usize {
        self.state
            .borrow()
            .slots
            .iter()
            .filter(|slot| matches!(slot.state, SlotState::InFlight { .. }))
            .count()
    }

    pub(crate) fn take_gpu_frame(&self, webview: WebViewHandle) -> Result<GpuFrame, SurfaceError> {
        self.drain_completions();
        if let Some(error) = self.last_error.borrow_mut().take() {
            return Err(SurfaceError::new(error));
        }
        if self.cpu_reads.get() != 0 {
            return Err(SurfaceError::new(
                "exportable frames must not use Servo's CPU readback path",
            ));
        }
        self.force_opaque_alpha();
        self.gleam_gl.finish();

        let active_slot = self
            .state
            .borrow()
            .active_slot
            .ok_or_else(|| SurfaceError::new("no rendered exportable frame is available"))?;
        let unbound = self
            .device
            .borrow()
            .unbind_surface_from_context(&mut self.context.borrow_mut())
            .map_err(surface_error);
        let surface = match unbound {
            Err(error) => {
                *self.last_error.borrow_mut() = Some(error.to_string());
                return Err(error);
            }
            Ok(None) => {
                let error = SurfaceError::new("exportable surface was not bound");
                *self.last_error.borrow_mut() = Some(error.to_string());
                return Err(error);
            }
            Ok(Some(surface)) => surface,
        };

        let (info, pixel_buffer, quarantine) = {
            let mut state = self.state.borrow_mut();
            state.active_slot = None;
            state.next_frame_serial = state.next_frame_serial.wrapping_add(1);
            let frame_serial = state.next_frame_serial;
            let slot = state
                .slots
                .iter_mut()
                .find(|slot| slot.id == active_slot)
                .expect("active exportable slot must exist");
            debug_assert_eq!(slot.state, SlotState::Rendering);
            slot.surface = Some(surface);
            slot.state = SlotState::InFlight { frame_serial };
            let identity = FrameIdentity {
                webview,
                generation: slot.generation,
                slot: slot.id,
                frame_serial,
            };
            (
                GpuFrameInfo {
                    identity,
                    physical_size: slot.physical_size,
                },
                slot.pixel_buffer.clone(),
                IOSurfaceLease::new(&slot.native_surface),
            )
        };

        Ok(GpuFrame {
            info,
            pixel_buffer,
            completion: GpuFrameCompletion {
                pending: Some(PendingCompletion {
                    identity: info.identity,
                    sender: self.completion_sender.clone(),
                    wake: self.wake.clone(),
                    quarantine: Some(quarantine),
                }),
            },
        })
    }

    pub(crate) fn recycle_unpublished(&self) -> Result<(), SurfaceError> {
        let Some(active_slot) = self.state.borrow().active_slot else {
            return Ok(());
        };
        let unbound = self
            .device
            .borrow()
            .unbind_surface_from_context(&mut self.context.borrow_mut())
            .map_err(surface_error);
        let surface = match unbound {
            Err(error) => {
                *self.last_error.borrow_mut() = Some(error.to_string());
                return Err(error);
            }
            Ok(None) => {
                let error = SurfaceError::new("exportable surface was not bound");
                *self.last_error.borrow_mut() = Some(error.to_string());
                return Err(error);
            }
            Ok(Some(surface)) => surface,
        };
        let mut state = self.state.borrow_mut();
        state.active_slot = None;
        if let Some(slot) = state.slots.iter_mut().find(|slot| slot.id == active_slot) {
            slot.surface = Some(surface);
            slot.state = SlotState::Available;
        }
        Ok(())
    }

    pub(crate) fn prepare_resize_to(&self, physical_size: SurfaceSize) -> Result<(), SurfaceError> {
        validate_size(physical_size)?;
        self.drain_completions();
        if self.state.borrow().physical_size == physical_size {
            return Ok(());
        }
        {
            let state = self.state.borrow();
            if blocks_new_generation(
                state.slots.iter().map(|slot| (slot.generation, slot.state)),
                state.generation,
            ) {
                return Err(SurfaceError::new(
                    "a retired exportable generation is still awaiting consumer completion",
                ));
            }
        }

        self.recycle_unpublished()?;
        let (old_generation, new_generation) = {
            let state = self.state.borrow();
            (state.generation, state.generation.wrapping_add(1))
        };
        let mut new_slots = Vec::with_capacity(SLOT_COUNT as usize);
        for _ in 0..SLOT_COUNT {
            match self.create_next_slot(new_generation, physical_size) {
                Ok(slot) => new_slots.push(slot),
                Err(error) => {
                    self.destroy_slots(new_slots);
                    let _ = self.bind_available_slot();
                    return Err(error);
                }
            }
        }

        {
            let mut state = self.state.borrow_mut();
            state.generation = new_generation;
            state.physical_size = physical_size;
            state.slots.extend(new_slots);
        }
        if let Err(error) = self.bind_available_slot() {
            let rollback = self.take_generation_slots(new_generation, true);
            self.destroy_slots(rollback);
            let mut state = self.state.borrow_mut();
            state.generation = old_generation;
            if let Some(slot) = state
                .slots
                .iter()
                .find(|slot| slot.generation == old_generation)
            {
                state.physical_size = slot.physical_size;
            }
            drop(state);
            let _ = self.bind_available_slot();
            return Err(error);
        }

        let retired = self.take_generation_slots(old_generation, false);
        self.destroy_slots(retired);
        self.last_error.borrow_mut().take();
        Ok(())
    }

    pub(crate) fn finish_resize_to(&self, physical_size: SurfaceSize) -> Result<(), SurfaceError> {
        if self.state.borrow().physical_size != physical_size
            || self.reported_size.get() != physical_size
        {
            return Err(SurfaceError::new(
                "Servo did not commit the prepared exportable surface size",
            ));
        }
        Ok(())
    }

    fn create_next_slot(
        &self,
        generation: u64,
        physical_size: SurfaceSize,
    ) -> Result<Slot, SurfaceError> {
        let id = {
            let mut state = self.state.borrow_mut();
            let id = state.next_slot;
            state.next_slot = state.next_slot.wrapping_add(1);
            id
        };
        let surface = self
            .device
            .borrow()
            .create_surface(
                &self.context.borrow(),
                SurfaceAccess::GPUOnly,
                SurfaceType::Generic {
                    size: Size2D::new(physical_size.width as i32, physical_size.height as i32),
                },
            )
            .map_err(surface_error)?;
        let native_surface = self.device.borrow().native_surface(&surface);
        let pixel_buffer = match pixel_buffer(&native_surface, physical_size) {
            Ok(pixel_buffer) => pixel_buffer,
            Err(error) => {
                drop(native_surface);
                let mut surface = surface;
                let _ = destroy_surface_in_owning_context(
                    &self.device.borrow(),
                    &mut self.context.borrow_mut(),
                    &mut surface,
                );
                return Err(error);
            }
        };
        Ok(Slot {
            id,
            generation,
            physical_size,
            surface: Some(surface),
            native_surface,
            pixel_buffer,
            state: SlotState::Available,
        })
    }

    fn bind_available_slot(&self) -> Result<(), SurfaceError> {
        if self.state.borrow().active_slot.is_some() {
            return Ok(());
        }
        let slot_id = {
            let state = self.state.borrow();
            state
                .slots
                .iter()
                .find(|slot| is_available(slot.generation, slot.state, state.generation))
                .map(|slot| slot.id)
                .ok_or_else(|| SurfaceError::new("exportable surface pool is exhausted"))?
        };
        let surface = {
            let mut state = self.state.borrow_mut();
            let slot = state
                .slots
                .iter_mut()
                .find(|slot| slot.id == slot_id)
                .expect("selected exportable slot must exist");
            slot.surface
                .take()
                .expect("available exportable slot must own its surface")
        };
        match self
            .device
            .borrow()
            .bind_surface_to_context(&mut self.context.borrow_mut(), surface)
        {
            Ok(()) => {
                let mut state = self.state.borrow_mut();
                state.active_slot = Some(slot_id);
                state
                    .slots
                    .iter_mut()
                    .find(|slot| slot.id == slot_id)
                    .expect("bound exportable slot must exist")
                    .state = SlotState::Rendering;
                Ok(())
            }
            Err((error, surface)) => {
                let mut state = self.state.borrow_mut();
                state
                    .slots
                    .iter_mut()
                    .find(|slot| slot.id == slot_id)
                    .expect("failed exportable slot must exist")
                    .surface = Some(surface);
                Err(surface_error(error))
            }
        }
    }

    fn drain_completions(&self) {
        let completed: Vec<_> = self.completion_receiver.try_iter().collect();
        if completed.is_empty() {
            return;
        }
        let mut retired = Vec::new();
        {
            let mut state = self.state.borrow_mut();
            for identity in completed {
                let Some(index) = state.slots.iter().position(|slot| {
                    matches_completion(slot.id, slot.generation, slot.state, identity)
                }) else {
                    continue;
                };
                if identity.generation == state.generation {
                    state.slots[index].state = SlotState::Available;
                } else {
                    retired.push(state.slots.swap_remove(index));
                }
            }
        }
        self.destroy_slots(retired);
    }

    fn take_generation_slots(&self, generation: u64, include_in_flight: bool) -> Vec<Slot> {
        let mut state = self.state.borrow_mut();
        let mut taken = Vec::new();
        let mut index = 0;
        while index < state.slots.len() {
            let slot = &state.slots[index];
            if slot.generation == generation
                && (include_in_flight || !matches!(slot.state, SlotState::InFlight { .. }))
            {
                taken.push(state.slots.swap_remove(index));
            } else {
                index += 1;
            }
        }
        taken
    }

    fn destroy_slots(&self, slots: Vec<Slot>) {
        let device = self.device.borrow();
        let mut context = self.context.borrow_mut();
        for mut slot in slots {
            if let Some(mut surface) = slot.surface.take() {
                let _ = destroy_surface_in_owning_context(&device, &mut context, &mut surface);
            }
        }
    }

    fn framebuffer(&self) -> u32 {
        self.device
            .borrow()
            .context_surface_info(&self.context.borrow())
            .ok()
            .flatten()
            .and_then(|info| info.framebuffer_object)
            .map_or(0, |framebuffer| framebuffer.0.get())
    }

    fn force_opaque_alpha(&self) {
        let mut clear_color = [0.0; 4];
        unsafe {
            self.glow_gl
                .get_parameter_f32_slice(gl::COLOR_CLEAR_VALUE, &mut clear_color);
            let color_mask = self
                .glow_gl
                .get_parameter_bool_array::<4>(gl::COLOR_WRITEMASK);
            let scissor_enabled = self.glow_gl.is_enabled(gl::SCISSOR_TEST);
            if scissor_enabled {
                self.glow_gl.disable(gl::SCISSOR_TEST);
            }
            self.glow_gl.color_mask(false, false, false, true);
            self.glow_gl.clear_color(0.0, 0.0, 0.0, 1.0);
            self.glow_gl.clear(gl::COLOR_BUFFER_BIT);
            self.glow_gl.clear_color(
                clear_color[0],
                clear_color[1],
                clear_color[2],
                clear_color[3],
            );
            self.glow_gl
                .color_mask(color_mask[0], color_mask[1], color_mask[2], color_mask[3]);
            if scissor_enabled {
                self.glow_gl.enable(gl::SCISSOR_TEST);
            }
        }
    }
}

impl RenderingContext for ExportableRenderingContext {
    fn prepare_for_rendering(&self) {
        self.drain_completions();
        if let Err(error) = self.bind_available_slot() {
            *self.last_error.borrow_mut() = Some(error.to_string());
            return;
        }
        self.gleam_gl
            .bind_framebuffer(gl::FRAMEBUFFER, self.framebuffer());
    }

    fn read_to_image(&self, _source_rectangle: DeviceIntRect) -> Option<RgbaImage> {
        self.cpu_reads.set(self.cpu_reads.get() + 1);
        None
    }

    fn size(&self) -> PhysicalSize<u32> {
        let size = self.reported_size.get();
        PhysicalSize::new(size.width, size.height)
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        let size = SurfaceSize::new(size.width, size.height);
        if self.state.borrow().physical_size == size {
            self.reported_size.set(size);
        } else {
            *self.last_error.borrow_mut() =
                Some(SurfaceError::new("exportable surface resize was not prepared").to_string());
        }
    }

    fn present(&self) {}

    fn make_current(&self) -> Result<(), surfman::Error> {
        self.device
            .borrow()
            .make_context_current(&self.context.borrow())
    }

    fn gleam_gl_api(&self) -> Rc<dyn Gl> {
        self.gleam_gl.clone()
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.glow_gl.clone()
    }

    fn create_texture(
        &self,
        surface: Surface,
    ) -> Option<(SurfaceTexture, u32, UntypedSize2D<i32>)> {
        let device = self.device.borrow();
        let mut context = self.context.borrow_mut();
        let size = device.surface_info(&surface).size;
        let texture = device.create_surface_texture(&mut context, surface).ok()?;
        let object = device
            .surface_texture_object(&texture)
            .map(|object| object.0.get())
            .unwrap_or(0);
        Some((texture, object, size))
    }

    fn destroy_texture(&self, texture: SurfaceTexture) -> Option<Surface> {
        self.device
            .borrow()
            .destroy_surface_texture(&mut self.context.borrow_mut(), texture)
            .ok()
    }

    fn connection(&self) -> Option<Connection> {
        Some(self.device.borrow().connection())
    }

    fn refresh_driver(&self) -> Option<Rc<dyn RefreshDriver>> {
        Some(self.refresh_driver.clone())
    }
}

impl Drop for ExportableRenderingContext {
    fn drop(&mut self) {
        let device = self.device.get_mut();
        let context = self.context.get_mut();
        if let Ok(Some(surface)) = device.unbind_surface_from_context(context) {
            if let Some(slot_id) = self.state.get_mut().active_slot.take() {
                if let Some(slot) = self
                    .state
                    .get_mut()
                    .slots
                    .iter_mut()
                    .find(|slot| slot.id == slot_id)
                {
                    slot.surface = Some(surface);
                    slot.state = SlotState::Available;
                }
            }
        }
        for slot in &mut self.state.get_mut().slots {
            if let Some(mut surface) = slot.surface.take() {
                let _ = destroy_surface_in_owning_context(device, context, &mut surface);
            }
        }
        let _ = device.destroy_context(context);
    }
}

fn pixel_buffer(
    native_surface: &SurfmanNativeSurface,
    physical_size: SurfaceSize,
) -> Result<CVPixelBuffer, SurfaceError> {
    let raw_surface = CFRetained::as_ptr(&native_surface.0).as_ptr() as io_surface::IOSurfaceRef;
    let legacy_surface = unsafe { io_surface::IOSurface::wrap_under_get_rule(raw_surface) };
    let options = CFDictionary::from_CFType_pairs(&[(
        CFString::from(CVPixelBufferKeys::MetalCompatibility),
        CFBoolean::true_value().as_CFType(),
    )]);
    let pixel_buffer = CVPixelBuffer::from_io_surface(&legacy_surface, Some(&options))
        .map_err(|error| SurfaceError::new(format!("CVPixelBuffer creation failed: {error}")))?;
    if pixel_buffer.get_pixel_format() != kCVPixelFormatType_32BGRA
        || pixel_buffer.get_width() != physical_size.width as usize
        || pixel_buffer.get_height() != physical_size.height as usize
    {
        return Err(SurfaceError::new(
            "surfman IOSurface did not expose the required 32BGRA physical extent",
        ));
    }
    Ok(pixel_buffer)
}

fn validate_size(size: SurfaceSize) -> Result<(), SurfaceError> {
    if size.width == 0
        || size.height == 0
        || size.width > i32::MAX as u32
        || size.height > i32::MAX as u32
    {
        return Err(SurfaceError::new(format!(
            "exportable surface size must fit a non-zero i32 extent, got {}x{}",
            size.width, size.height
        )));
    }
    Ok(())
}

fn surface_error(error: surfman::Error) -> SurfaceError {
    SurfaceError::new(format!("{error:?}"))
}

fn destroy_surface_in_owning_context(
    device: &Device,
    context: &mut Context,
    surface: &mut Surface,
) -> Result<(), surfman::Error> {
    device.make_context_current(context)?;
    device.destroy_surface(context, surface)
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorerkit_embedder::{PlaceholderHost, Runtime};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestRefreshDriver;

    impl RefreshDriver for TestRefreshDriver {
        fn observe_next_frame(&self, _start_frame_callback: Box<dyn Fn() + Send + 'static>) {}
    }

    fn exportable_context(size: SurfaceSize) -> ExportableRenderingContext {
        ExportableRenderingContext::new(size, Arc::new(|| {}), Rc::new(TestRefreshDriver)).unwrap()
    }

    fn live_framebuffer(context: &ExportableRenderingContext) -> u32 {
        context.make_current().unwrap();
        let framebuffer = context.framebuffer();
        assert_ne!(framebuffer, 0);
        assert_ne!(context.gleam_gl.is_framebuffer(framebuffer), 0);
        framebuffer
    }

    fn assert_framebuffer_is_live(context: &ExportableRenderingContext, framebuffer: u32) {
        context.make_current().unwrap();
        assert_ne!(
            context.gleam_gl.is_framebuffer(framebuffer),
            0,
            "another context deleted the sibling framebuffer"
        );
    }

    fn webview() -> WebViewHandle {
        let mut runtime = Runtime::new(PlaceholderHost::default());
        let session = runtime.create_session();
        runtime.create_webview(session).unwrap()
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn completion_is_send() {
        assert_send::<GpuFrameCompletion>();
    }

    #[test]
    fn resizing_one_context_preserves_a_current_sibling_framebuffer() {
        let first = exportable_context(SurfaceSize::new(64, 64));
        let second = exportable_context(SurfaceSize::new(64, 64));
        let second_framebuffer = live_framebuffer(&second);

        first.prepare_resize_to(SurfaceSize::new(96, 96)).unwrap();

        assert_framebuffer_is_live(&second, second_framebuffer);
    }

    #[test]
    fn deferred_completion_preserves_a_current_sibling_framebuffer() {
        let first = exportable_context(SurfaceSize::new(64, 64));
        let second = exportable_context(SurfaceSize::new(64, 64));
        first.make_current().unwrap();
        first.prepare_for_rendering();
        let frame = first.take_gpu_frame(webview()).unwrap();
        first.prepare_resize_to(SurfaceSize::new(96, 96)).unwrap();
        let second_framebuffer = live_framebuffer(&second);

        let (pixel_buffer, completion) = frame.into_parts();
        drop(pixel_buffer);
        completion.complete();
        first.can_paint().unwrap();

        assert_framebuffer_is_live(&second, second_framebuffer);
    }

    #[test]
    fn dropping_one_context_preserves_a_current_sibling_framebuffer() {
        let first = exportable_context(SurfaceSize::new(64, 64));
        let second = exportable_context(SurfaceSize::new(64, 64));
        let second_framebuffer = live_framebuffer(&second);

        drop(first);

        assert_framebuffer_is_live(&second, second_framebuffer);
    }

    #[test]
    fn completion_after_producer_teardown_does_not_wake_the_host() {
        let (sender, receiver) = mpsc::channel();
        drop(receiver);
        let wake_count = Arc::new(AtomicUsize::new(0));
        let completion = GpuFrameCompletion {
            pending: Some(PendingCompletion {
                identity: FrameIdentity {
                    webview: webview(),
                    generation: 1,
                    slot: 0,
                    frame_serial: 1,
                },
                sender,
                wake: {
                    let wake_count = wake_count.clone();
                    Arc::new(move || {
                        wake_count.fetch_add(1, Ordering::Relaxed);
                    })
                },
                quarantine: None,
            }),
        };

        completion.complete();

        assert_eq!(wake_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn bounded_pool_backpressure_requires_an_available_current_slot() {
        let states = [
            SlotState::InFlight { frame_serial: 1 },
            SlotState::InFlight { frame_serial: 2 },
            SlotState::InFlight { frame_serial: 3 },
        ];
        assert_eq!(states.len(), SLOT_COUNT as usize);
        assert!(!states.iter().any(|state| is_available(1, *state, 1)));
        assert!(is_available(1, SlotState::Available, 1));
        assert!(!is_available(1, SlotState::Available, 2));
    }

    #[test]
    fn completion_matches_only_the_exact_generation_slot_and_serial() {
        let webview = webview();
        let identity = FrameIdentity {
            webview,
            generation: 2,
            slot: 4,
            frame_serial: 9,
        };
        let in_flight = SlotState::InFlight { frame_serial: 9 };
        assert!(matches_completion(4, 2, in_flight, identity));
        assert!(!matches_completion(3, 2, in_flight, identity));
        assert!(!matches_completion(4, 1, in_flight, identity));
        assert!(!matches_completion(
            4,
            2,
            SlotState::InFlight { frame_serial: 8 },
            identity
        ));
        assert!(!matches_completion(4, 2, SlotState::Available, identity));
    }

    #[test]
    fn rejected_third_generation_leaves_the_second_generation_paintable() {
        let slots = [
            (1, SlotState::InFlight { frame_serial: 7 }),
            (2, SlotState::Available),
            (2, SlotState::InFlight { frame_serial: 8 }),
        ];
        let before = slots;

        assert!(blocks_new_generation(slots, 2));
        assert_eq!(slots, before);
        assert!(slots
            .into_iter()
            .any(|(generation, state)| is_available(generation, state, 2)));
    }
}
