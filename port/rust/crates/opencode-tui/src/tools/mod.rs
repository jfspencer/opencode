//! Tool calls as the session view renders them (`routes/session/index.tsx`'s `ToolPart` and every tool's view),
//! and their helpers.
//!
//! - [`display`]: `util/tool-display.ts` (and strip-ansi); [`presentation`]: `util/permission.ts`;
//!   [`collapse`]: `util/collapse-tool-output.ts`; [`todo`]: `util/todo.ts` and `component/todo-item.tsx`;
//!   [`diff_util`]: `util/diff.ts`; [`filetype`]: `util/filetype.ts`; [`path_format`]: `util/path-format.ts` and
//!   `context/path-format.tsx`; [`patch_diff`]: `component/patch-diff.tsx`.
pub mod collapse;
pub mod diff_util;
pub mod display;
pub mod filetype;
pub mod parts;
pub mod patch_diff;
pub mod path_format;
pub mod presentation;
pub mod scroll;
pub mod standin;
pub mod todo;

pub use parts::{tool_images, tool_part};
