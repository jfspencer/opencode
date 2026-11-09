//! The schedule tools (packages/core/src/tool/plugin/schedule.ts): `schedule_create`, `schedule_list`,
//! `schedule_pause`, `schedule_resume` and `schedule_delete`, Code Mode tools of the `opencode` namespace registered
//! while `experimental.schedules` is on in the location. Their definitions are recorded from the reference
//! (`definitions/schedule.json`); the snapshot adds them to the Code Mode inventory, and so to its catalog.
//!
//! Their execution is out of scope (a call fails as not yet supported): the tools exist only while the experimental
//! `schedules` flag is on (experimental flags are out of scope; the standard turns it on only for the schedule API).

use std::sync::{Arc, OnceLock};

use super::Tool;
use crate::inventory::Inventory;

/// The schedule tools when the location has schedules on, in registration order (after the `opencode.tools` plugin's).
pub fn tools(inventory: &Inventory) -> Vec<Arc<dyn Tool>> {
    static TOOLS: OnceLock<Vec<Arc<dyn Tool>>> = OnceLock::new();
    if !crate::flag::enabled(&inventory.entries, "schedules") {
        return Vec::new();
    }
    TOOLS.get_or_init(|| super::opencode::recorded(include_str!("definitions/schedule.json"))).clone()
}
