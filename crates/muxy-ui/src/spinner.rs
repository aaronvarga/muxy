//! Indeterminate progress drawn by Core Animation, without app redraws.

use std::cell::Cell;
use std::ptr;

use gpui::{Bounds, Pixels, Rgba, Window};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSView;
use objc2_core_graphics::{CGColor, CGPath};
use objc2_foundation::{NSNumber, NSPoint, NSRect, NSSize, ns_string};
use objc2_quartz_core::{
    CABasicAnimation, CAMediaTiming, CAMediaTimingFunction, CAShapeLayer, CATransaction,
    kCALineCapRound, kCAMediaTimingFunctionLinear,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const LINE_FRACTION: f64 = 1.0 / 8.0;
const ARC_FRACTION: f64 = 0.28;
const TRACK_ALPHA: f32 = 0.25;

pub struct NativeSpinner {
    view: Retained<NSView>,
    parent: Retained<NSView>,
    track: Retained<CAShapeLayer>,
    arc: Retained<CAShapeLayer>,
    frame: Cell<Option<NSRect>>,
    color: Cell<Option<Rgba>>,
    scale_factor: Cell<Option<f32>>,
    visible: Cell<bool>,
    top_border: bool,
}

impl std::fmt::Debug for NativeSpinner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeSpinner")
            .field("visible", &self.visible.get())
            .finish_non_exhaustive()
    }
}

impl NativeSpinner {
    pub fn new(window: &Window) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("spinner must be created on the main thread")?;
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window)
            .map_err(|error| error.to_string())?
            .as_raw()
        else {
            return Err("expected AppKit window".into());
        };
        let parent = unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }
            .ok_or("missing native view")?;
        let view = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
        view.setWantsLayer(true);
        view.setHidden(true);
        let layer = view.layer().ok_or("spinner view has no layer")?;
        let track = CAShapeLayer::new();
        let arc = CAShapeLayer::new();
        for shape in [&track, &arc] {
            shape.setFillColor(None);
            layer.addSublayer(shape);
        }
        unsafe { arc.setLineCap(kCALineCapRound) };
        arc.setStrokeEnd(ARC_FRACTION);
        parent.addSubview(&view);
        Ok(Self {
            view,
            parent,
            track,
            arc,
            frame: Cell::new(None),
            color: Cell::new(None),
            scale_factor: Cell::new(None),
            visible: Cell::new(false),
            top_border: false,
        })
    }

    pub fn top_border(window: &Window) -> Result<Self, String> {
        let mut spinner = Self::new(window)?;
        spinner.top_border = true;
        Ok(spinner)
    }

    pub fn show(&self, bounds: Bounds<Pixels>, color: Rgba, visible: bool, scale_factor: f32) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let frame = self.frame_in_parent(bounds);
        if self.frame.get() != Some(frame) {
            self.view.setFrame(frame);
            if self
                .frame
                .get()
                .is_none_or(|previous| previous.size != frame.size)
            {
                self.layout(frame.size);
            }
            self.frame.set(Some(frame));
        }
        if self.scale_factor.replace(Some(scale_factor)) != Some(scale_factor) {
            for shape in [&self.track, &self.arc] {
                shape.setContentsScale(f64::from(scale_factor));
            }
        }
        if self.color.get() != Some(color) {
            let track = Rgba {
                a: color.a * TRACK_ALPHA,
                ..color
            };
            paint(&self.track, track, self.top_border);
            paint(&self.arc, color, self.top_border);
            self.color.set(Some(color));
        }
        CATransaction::commit();
        self.set_visible(visible);
    }

    pub fn set_visible(&self, visible: bool) {
        if self.visible.replace(visible) != visible {
            self.view.setHidden(!visible);
            if visible {
                self.turn();
            } else {
                self.arc.removeAllAnimations();
            }
        }
    }

    fn frame_in_parent(&self, bounds: Bounds<Pixels>) -> NSRect {
        let top = f64::from(f32::from(bounds.top()));
        let height = f64::from(f32::from(bounds.size.height));
        let y = if self.parent.isFlipped() {
            top
        } else {
            self.parent.bounds().size.height - top - height
        };
        NSRect::new(
            NSPoint::new(f64::from(f32::from(bounds.left())), y),
            NSSize::new(f64::from(f32::from(bounds.size.width)), height),
        )
    }

    fn layout(&self, size: NSSize) {
        if self.top_border {
            let rect = NSRect::new(NSPoint::ZERO, size);
            let path = unsafe { CGPath::with_rect(rect, ptr::null()) };
            for shape in [&self.track, &self.arc] {
                shape.setFrame(rect);
                shape.setPath(Some(&path));
            }
            return;
        }
        let diameter = size.width.min(size.height);
        let line = (diameter * LINE_FRACTION).max(1.0);
        let inset = NSRect::new(
            NSPoint::new(
                f64::midpoint(size.width - diameter, line),
                f64::midpoint(size.height - diameter, line),
            ),
            NSSize::new(diameter - line, diameter - line),
        );
        let path = unsafe { CGPath::with_ellipse_in_rect(inset, ptr::null()) };
        for shape in [&self.track, &self.arc] {
            shape.setFrame(NSRect::new(NSPoint::ZERO, size));
            shape.setPath(Some(&path));
            shape.setLineWidth(line);
        }
    }

    fn turn(&self) {
        self.arc
            .addAnimation_forKey(&animation(self.top_border), Some(ns_string!("activity")));
    }
}

fn animation(top_border: bool) -> Retained<CABasicAnimation> {
    let animation = CABasicAnimation::animationWithKeyPath(Some(if top_border {
        ns_string!("opacity")
    } else {
        ns_string!("transform.rotation.z")
    }));
    let (from, to) = if top_border {
        (0.2, 1.0)
    } else {
        (0.0, -std::f64::consts::TAU)
    };
    unsafe {
        animation.setFromValue(Some(&NSNumber::new_f64(from)));
        animation.setToValue(Some(&NSNumber::new_f64(to)));
    }
    let timing = unsafe { CAMediaTimingFunction::functionWithName(kCAMediaTimingFunctionLinear) };
    animation.setTimingFunction(Some(&timing));
    animation.setDuration(0.8);
    animation.setAutoreverses(top_border);
    animation.setRepeatCount(f32::INFINITY);
    animation
}

impl Drop for NativeSpinner {
    fn drop(&mut self) {
        self.arc.removeAllAnimations();
        self.view.removeFromSuperview();
    }
}

fn paint(shape: &CAShapeLayer, color: Rgba, filled: bool) {
    let color = CGColor::new_srgb(
        f64::from(color.r),
        f64::from(color.g),
        f64::from(color.b),
        f64::from(color.a),
    );
    if filled {
        shape.setFillColor(Some(&color));
    } else {
        shape.setStrokeColor(Some(&color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_border_pulses_without_rotating_the_strip_or_changing_spinner_motion() {
        let pulse = animation(true);
        assert_eq!(pulse.keyPath().as_deref(), Some(ns_string!("opacity")));
        assert!(pulse.autoreverses());
        assert!(pulse.repeatCount().is_infinite());
        let spin = animation(false);
        assert_eq!(
            spin.keyPath().as_deref(),
            Some(ns_string!("transform.rotation.z"))
        );
        assert!(!spin.autoreverses());
    }
}
