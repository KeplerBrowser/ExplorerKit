//! The upstream MiniBrowser event framing over an owned connected Unix stream.
//! Dispatch I/O invokes all handlers on AppKit's main queue; no native object is
//! sent to a worker. The cleanup block closes the descriptor after dispatch stops.
use super::{HostError, State};
use block2::{DynBlock, RcBlock};
use std::{
    cell::RefCell,
    ffi::c_void,
    os::fd::{AsRawFd, OwnedFd},
    os::unix::net::UnixStream,
    rc::Rc,
};

type Object = *mut c_void;
unsafe extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_io_create(
        kind: usize,
        fd: i32,
        queue: Object,
        cleanup: &DynBlock<dyn Fn(i32)>,
    ) -> Object;
    fn dispatch_io_set_low_water(channel: Object, value: usize);
    fn dispatch_io_read(
        channel: Object,
        offset: i64,
        length: usize,
        queue: Object,
        handler: &DynBlock<dyn Fn(u8, Object, i32)>,
    );
    fn dispatch_io_write(
        channel: Object,
        offset: i64,
        data: Object,
        queue: Object,
        handler: &DynBlock<dyn Fn(u8, Object, i32)>,
    );
    fn dispatch_io_close(channel: Object, flags: usize);
    fn dispatch_data_create(
        data: *const c_void,
        size: usize,
        queue: Object,
        destructor: &DynBlock<dyn Fn()>,
    ) -> Object;
    fn dispatch_data_create_map(
        data: Object,
        buffer: *mut *const c_void,
        size: *mut usize,
    ) -> Object;
    fn dispatch_release(object: Object);
    fn getsockopt(fd: i32, level: i32, name: i32, value: *mut i32, length: *mut u32) -> i32;
}
fn main_queue() -> Object {
    (&raw const _dispatch_main_q).cast_mut()
}

pub(super) struct Transport {
    channel: Object,
}
impl Transport {
    pub(super) fn open(fd: OwnedFd, state: &Rc<State>, epoch: u64) -> Result<Self, HostError> {
        let stream = UnixStream::from(fd);
        stream.peer_addr().map_err(|e| {
            HostError::new(format!("automation needs a connected Unix stream: {e}"))
        })?;
        let mut kind = 0;
        let mut size = 4;
        // SAFETY: getsockopt writes the supplied i32 and length, for this owned fd.
        if unsafe { getsockopt(stream.as_raw_fd(), 0xffff, 0x1008, &mut kind, &mut size) } != 0
            || kind != 1
        {
            return Err(HostError::new(
                "automation descriptor is not a stream socket",
            ));
        }
        let fd: OwnedFd = stream.into();
        let number = fd.as_raw_fd();
        let owner = RefCell::new(Some(fd));
        let cleanup = RcBlock::new(move |_: i32| {
            drop(owner.borrow_mut().take());
        });
        // SAFETY: Native dispatch owns only I/O scheduling; cleanup owns the fd.
        let channel = unsafe { dispatch_io_create(0, number, main_queue(), &cleanup) };
        if channel.is_null() {
            return Err(HostError::new("could not create automation dispatch I/O"));
        }
        let weak = Rc::downgrade(state);
        let buffer = RefCell::new(Vec::new());
        let handler = RcBlock::new(move |done: u8, data: Object, error: i32| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if !state.is_epoch(epoch) {
                return;
            }
            if error != 0 {
                state.disconnect(Some(format!("automation read failed: {error}")));
                return;
            }
            if !data.is_null() {
                let mut bytes = std::ptr::null();
                let mut size = 0;
                // SAFETY: The callback lends data; map is retained until released below.
                let map = unsafe { dispatch_data_create_map(data, &mut bytes, &mut size) };
                if size != 0 {
                    buffer.borrow_mut().extend_from_slice(unsafe {
                        std::slice::from_raw_parts(bytes.cast::<u8>(), size)
                    });
                }
                if !map.is_null() {
                    unsafe { dispatch_release(map) };
                }
                loop {
                    let frame = {
                        let mut buffer = buffer.borrow_mut();
                        if buffer.len() < 4 {
                            break;
                        }
                        let length = u32::from_be_bytes(buffer[..4].try_into().unwrap()) as usize;
                        if buffer.len() < 4 + length {
                            break;
                        }
                        let frame = buffer[4..4 + length].to_vec();
                        buffer.drain(..4 + length);
                        frame
                    };
                    match serde_json::from_slice(&frame) {
                        Ok(event) => state.enqueue(epoch, event),
                        Err(error) => {
                            state.disconnect(Some(format!(
                                "invalid automation transport event: {error}"
                            )));
                            return;
                        }
                    }
                }
            }
            if done != 0 {
                state.disconnect(Some("automation stream ended".into()));
            }
        });
        unsafe {
            dispatch_io_set_low_water(channel, 1);
            dispatch_io_read(channel, 0, usize::MAX, main_queue(), &handler);
        }
        Ok(Self { channel })
    }

    pub(super) fn send(&self, event: &serde_json::Value, state: &Rc<State>, epoch: u64) {
        let bytes = serde_json::to_vec(event).expect("transport events contain JSON values");
        let Ok(length) = u32::try_from(bytes.len()) else {
            state.disconnect(Some("automation event is too large".into()));
            return;
        };
        let mut frame = length.to_be_bytes().to_vec();
        frame.extend(bytes);
        let pointer = frame.as_ptr();
        let length = frame.len();
        let destructor = RcBlock::new(move || {
            let _ = &frame;
        });
        let weak = Rc::downgrade(state);
        let completion = RcBlock::new(move |_: u8, _: Object, error: i32| {
            if error != 0 {
                if let Some(state) = weak.upgrade().filter(|state| state.is_epoch(epoch)) {
                    state.disconnect(Some(format!("automation write failed: {error}")));
                }
            }
        });
        // SAFETY: The data destructor retains frame; native I/O retains the data
        // and completion blocks until the write finishes, on the main queue.
        unsafe {
            let data = dispatch_data_create(pointer.cast(), length, main_queue(), &destructor);
            dispatch_io_write(self.channel, 0, data, main_queue(), &completion);
            dispatch_release(data);
        }
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        unsafe {
            dispatch_io_close(self.channel, 1);
            dispatch_release(self.channel);
        }
    }
}
