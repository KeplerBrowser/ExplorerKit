//! Public AppKit SVG normalization; history only retains the resulting PNG.

use super::{MAX_ENCODED_BYTES, MAX_PIXELS};
use objc2::{msg_send, rc::autoreleasepool, runtime::AnyClass, AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSBitmapFormat, NSBitmapImageFileType, NSBitmapImageRep, NSCompositingOperation,
    NSDeviceRGBColorSpace, NSGraphicsContext, NSImage, NSImageRep,
};
use objc2_foundation::{
    ns_string, NSArray, NSData, NSDictionary, NSPoint, NSRect, NSSize, NSString,
};

fn representation() -> Option<&'static AnyClass> {
    let class = NSImageRep::imageRepClassForType(ns_string!("public.svg-image"))?;
    // SAFETY: this is the registered NSImageRep subclass's public type list.
    let types: objc2::rc::Retained<NSArray<NSString>> =
        unsafe { msg_send![class, imageUnfilteredTypes] };
    (types.len() == 1 && &*types.objectAtIndex(0) == ns_string!("public.svg-image"))
        .then_some(class)
}

pub(super) fn available() -> bool {
    representation().is_some()
}

pub(super) fn normalize(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > MAX_ENCODED_BYTES {
        return None;
    }
    MainThreadMarker::new()?;
    autoreleasepool(|_| {
        let class = representation()?;
        let data = NSData::with_bytes(bytes);
        if NSImageRep::imageRepClassForData(&data) != Some(class) {
            return None;
        }
        // SAFETY: owned in-memory SVG on AppKit's thread. Native internals have no hard allocation cap.
        unsafe {
            let image = NSImage::initWithData(NSImage::alloc(), &data)?;
            let size = image.size();
            let (width, height) = dimensions(size.width, size.height)?;
            let bitmap = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(), std::ptr::null_mut(), width as isize, height as isize, 8, 4, true, false, NSDeviceRGBColorSpace, NSBitmapFormat(0), (width * 4) as isize, 32,
            )?;
            let pointer = bitmap.bitmapData();
            if pointer.is_null() {
                return None;
            }
            std::ptr::write_bytes(pointer, 0, bitmap.bytesPerRow() as usize * height as usize);
            let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)?;
            NSGraphicsContext::saveGraphicsState_class();
            NSGraphicsContext::setCurrentContext(Some(&context));
            let rect = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(f64::from(width), f64::from(height)),
            );
            let source = image.bestRepresentationForRect_context_hints(rect, Some(&context), None);
            let drawn = source.is_some_and(|source| {
                source.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                    rect,
                    NSRect::ZERO,
                    NSCompositingOperation::Copy,
                    1.0,
                    false,
                    None,
                )
            });
            NSGraphicsContext::restoreGraphicsState_class();
            if !drawn {
                return None;
            }
            let png = bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )?;
            copy_encoded_png(&png)
        }
    })
}

fn copy_encoded_png(data: &NSData) -> Option<Vec<u8>> {
    (!data.is_empty() && data.len() <= MAX_ENCODED_BYTES).then(|| data.to_vec())
}

fn dimensions(width: f64, height: f64) -> Option<(u32, u32)> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let (width, height) = (width.ceil(), height.ceil());
    if width > MAX_PIXELS as f64 || height > MAX_PIXELS as f64 || width * height > MAX_PIXELS as f64
    {
        return None;
    }
    Some((width as u32, height as u32))
}

#[cfg(test)]
mod tests {
    use super::dimensions;
    #[test]
    fn checked_native_dimensions_bound_only_the_destination() {
        assert_eq!(dimensions(3.2, 2.2), Some((4, 3)));
        assert_eq!(dimensions(2048.0, 2048.0), Some((2048, 2048)));
        for (w, h) in [
            (0.0, 1.0),
            (-1.0, 1.0),
            (f64::NAN, 1.0),
            (1.0, f64::INFINITY),
            (2049.0, 2048.0),
            (1e30, 1e30),
        ] {
            assert_eq!(dimensions(w, h), None);
        }
    }
    #[test]
    fn normalized_png_length_is_checked_before_copying_native_data() {
        use super::{copy_encoded_png, MAX_ENCODED_BYTES};
        use objc2_foundation::NSData;
        assert_eq!(copy_encoded_png(&NSData::with_bytes(&[])), None);
        assert_eq!(
            copy_encoded_png(&NSData::with_bytes(&[1, 2, 3])),
            Some(vec![1, 2, 3])
        );
        assert_eq!(
            copy_encoded_png(&NSData::with_bytes(&vec![0; MAX_ENCODED_BYTES + 1])),
            None
        );
        assert_eq!(
            copy_encoded_png(&NSData::with_bytes(&vec![0; MAX_ENCODED_BYTES]))
                .unwrap()
                .len(),
            MAX_ENCODED_BYTES
        );
    }
}
