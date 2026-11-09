//! `util/scroll.ts`: the wheel acceleration from the config (`scroll.acceleration`, `scroll.speed`, else 3 rows a
//! step).
use std::rc::Rc;

use opencode_tui_core::renderables::scrollbox::{MacOSScrollAccel, ScrollAcceleration};

use crate::config::Resolved;

/// `CustomSpeedScroll`.
pub struct CustomSpeedScroll(pub f64);

impl ScrollAcceleration for CustomSpeedScroll {
    fn tick(&self, _now: f64) -> f64 {
        self.0
    }
    fn reset(&self) {}
}

/// `getScrollAcceleration(config)`.
pub fn scroll_acceleration(config: &Resolved) -> Rc<dyn ScrollAcceleration> {
    let scroll = config.scroll.as_ref();
    if scroll.and_then(|s| s.acceleration).unwrap_or(false) {
        return Rc::new(MacOSScrollAccel::new(None, None, None));
    }
    if let Some(speed) = scroll.and_then(|s| s.speed) {
        return Rc::new(CustomSpeedScroll(speed));
    }
    Rc::new(CustomSpeedScroll(3.0))
}
