//! Trackpad pinch (macOS "magnify" gestures), which GPUI doesn't deliver: a native event
//! monitor passes each one on, with where it happened, to whoever listens.

use gpui::{Pixels, Point, point, px};

/// One step of a pinch: how much bigger (0.05 = 5% more, negative = smaller), and where the
/// fingers are in the window (top-left origin, like GPUI's).
#[derive(Clone, Copy, Debug)]
pub struct Pinch {
    pub magnify: f32,
    pub at: Point<Pixels>,
}

/// Starts listening (once); each pinch is sent down `tx`.
#[cfg(target_os = "macos")]
pub fn listen(tx: futures::channel::mpsc::UnboundedSender<Pinch>) {
    use block::ConcreteBlock;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSSize {
        w: f64,
        h: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSRect {
        origin: NSPoint,
        size: NSSize,
    }
    // NSEventMaskMagnify (1 << NSEventTypeMagnify, which is 30).
    const MASK_MAGNIFY: u64 = 1 << 30;

    let handler = ConcreteBlock::new(move |event: *mut Object| -> *mut Object {
        unsafe {
            let magnify: f64 = msg_send![event, magnification];
            let loc: NSPoint = msg_send![event, locationInWindow];
            let window: *mut Object = msg_send![event, window];
            if !window.is_null() {
                let view: *mut Object = msg_send![window, contentView];
                let frame: NSRect = msg_send![view, frame];
                // AppKit counts from the bottom; GPUI from the top.
                let at = point(px(loc.x as f32), px((frame.size.h - loc.y) as f32));
                let _ = tx.unbounded_send(Pinch { magnify: magnify as f32, at });
            }
        }
        // Handed on as usual.
        event
    })
    .copy();
    unsafe {
        let _: *mut Object = msg_send![class!(NSEvent), addLocalMonitorForEventsMatchingMask: MASK_MAGNIFY handler: &*handler];
    }
    // The monitor lives as long as the app.
    std::mem::forget(handler);
}

#[cfg(not(target_os = "macos"))]
pub fn listen(_tx: futures::channel::mpsc::UnboundedSender<Pinch>) {}
