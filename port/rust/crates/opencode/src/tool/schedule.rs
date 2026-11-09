//! The schedule tools (packages/core/src/tool/plugin/schedule.ts): `schedule_create`, `schedule_list`,
//! `schedule_pause`, `schedule_resume` and `schedule_delete`, Code Mode tools of the `opencode` namespace registered
//! while `experimental.schedules` is on in the location.
//!
//! STAND-IN: the reference configuration's Code Mode catalog with the schedule tools in it
//! (`codemode-catalog-schedules.json`, recorded from the reference with `experimental.schedules` on), until the Code
//! Mode tools are registered and summarized here. TODO(port): the tools' execution (through `execute`).

use std::sync::OnceLock;

use opencode_protocol::Json;

use crate::inventory::Inventory;
use crate::json;

/// The Code Mode catalog with the schedule tools when the location has schedules on, else `catalog` as it is.
pub fn code_mode_catalog(inventory: &Inventory, catalog: Option<Json>) -> Option<Json> {
    static CATALOG: OnceLock<Json> = OnceLock::new();
    let catalog = catalog?;
    if !crate::flag::enabled(&inventory.entries, "schedules") {
        return Some(catalog);
    }
    Some(CATALOG.get_or_init(|| json::parse(include_str!("codemode-catalog-schedules.json")).expect("catalog is JSON")).clone())
}
