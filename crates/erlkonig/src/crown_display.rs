//! One view-bound native display link; no worker, polling timer or runtime state.

use crate::crown::{self, WorkActivity};
use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSCellImagePosition, NSImageScaling, NSStatusBarButton, NSWorkspace};
use objc2_foundation::{NSObject, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes};
use objc2_quartz_core::CADisplayLink;
use std::cell::Cell;
use std::time::Instant;

struct CrownIvars {
    button: Retained<NSStatusBarButton>,
    origin: Instant,
    activity: Cell<Option<WorkActivity>>,
    needs_attention: Cell<bool>,
    animating: Cell<bool>,
    reduced: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclass requirements. All state and AppKit
    // access stay on the main thread; the target never retains its link.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = CrownIvars]
    struct CrownTarget;

    unsafe impl NSObjectProtocol for CrownTarget {}

    impl CrownTarget {
        // SAFETY: CADisplayLink invokes this exact void(id) selector on the
        // main run loop to which we add it below.
        #[unsafe(method(displayFrame:))]
        fn display_frame(&self, link: &CADisplayLink) {
            let frame = self.frame();
            self.install(&frame);
            link.setPaused(!frame.animating);
        }
    }
);

impl CrownTarget {
    fn frame(&self) -> crown::Frame {
        let state = self.ivars();
        let reduced = NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
        state.reduced.set(reduced);
        crown::frame(
            state.origin.elapsed(),
            state.activity.get(),
            reduced,
            state.needs_attention.get(),
        )
    }

    fn install(&self, frame: &crown::Frame) {
        self.ivars().animating.set(frame.animating);
        self.ivars().button.setImage(Some(&frame.template_image()));
    }

    fn reconcile(&self, link: &CADisplayLink, attention_changed: bool) {
        let previous_reduced = self.ivars().reduced.get();
        let frame = self.frame();
        // Reconcile starts/stops presentation and redraws changed static state.
        // While active, native callbacks present attention changes too;
        // image-caused wakes cannot redraw.
        if (frame.animating && link.isPaused())
            || (!frame.animating && self.ivars().animating.get())
            || previous_reduced != self.ivars().reduced.get()
            || (attention_changed && !frame.animating)
        {
            self.install(&frame);
        }
        link.setPaused(!frame.animating);
    }
}

pub struct DisplayCrown {
    link: Retained<CADisplayLink>,
    target: Retained<CrownTarget>,
}

impl DisplayCrown {
    pub fn new(
        button: Retained<NSStatusBarButton>,
        origin: Instant,
        mtm: MainThreadMarker,
    ) -> Self {
        button.setImageScaling(NSImageScaling::ScaleNone);
        button.setImagePosition(NSCellImagePosition::ImageOnly);
        let target = CrownTarget::alloc(mtm).set_ivars(CrownIvars {
            button: button.clone(),
            origin,
            activity: Cell::new(None),
            needs_attention: Cell::new(false),
            animating: Cell::new(false),
            reduced: Cell::new(false),
        });
        // SAFETY: NSObject init has this signature; target implements the
        // display-link selector above. NSView follows the button's display and
        // suppresses callbacks while hidden/off-display (macOS 14+).
        let target: Retained<CrownTarget> = unsafe { msg_send![super(target), init] };
        let link = unsafe { button.displayLinkWithTarget_selector(&target, sel!(displayFrame:)) };
        link.setPaused(true);
        // SAFETY: constructed and used only on the main thread. Common modes
        // keep native presentation alive during menu tracking too.
        unsafe {
            link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes);
        }
        target.install(&target.frame());
        Self { link, target }
    }

    pub fn update(&self, activity: Option<WorkActivity>, needs_attention: bool) {
        self.target.ivars().activity.set(activity);
        let attention_changed = self.target.ivars().needs_attention.replace(needs_attention)
            != needs_attention;
        self.target.reconcile(&self.link, attention_changed);
    }

}

impl Drop for DisplayCrown {
    fn drop(&mut self) {
        // Removes every run-loop registration and releases the target. The
        // target owns no link, so no reference cycle or callback survives us.
        self.link.invalidate();
    }
}
