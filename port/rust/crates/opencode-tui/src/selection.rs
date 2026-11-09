//! Copying the selection: `util/selection.ts` (`copy`, `copyOnSelectRelease`, `handleSelectionKey`) and
//! `clipboard.ts`'s `write` (the TUI's clipboard service over `opencode_tui_core::clipboard`).
use opencode_tui_core::clipboard::{self, HostStatus, TerminalStatus};
use opencode_tui_core::renderer;
use opencode_tui_core::selection::{self as core_selection, SelectionBehavior};
use opencode_tui_core::tree::MouseEvent;
use opencode_tui_input::keymap::KeymapEvent;

use crate::toast::{ToastInput, Toasts, Variant};

/// `clipboard.write(text)`: the host clipboard and OSC 52; fails only when neither took the text.
pub async fn write_clipboard(text: &str) -> Result<(), String> {
    // OpenTUI rejects NUL before any destination; host clipboard text cannot contain it.
    let result = clipboard::write_text(&text.replace('\0', "")).await;
    if result.host == HostStatus::Written || result.terminal == TerminalStatus::Attempted {
        return Ok(());
    }
    if let HostStatus::Failed(error) = result.host {
        return Err(error);
    }
    let host = match result.host {
        HostStatus::Unsupported => "unsupported",
        _ => "not-attempted",
    };
    let terminal = match result.terminal {
        TerminalStatus::LocalFailure => "local-failure",
        _ => "not-attempted",
    };
    Err(format!("Clipboard write failed (host: {host}, terminal: {terminal})"))
}

/// `terminal.copy` is `"select"` (the default off Windows): a finished drag copies.
pub fn copy_on_select_enabled(copy: Option<&str>) -> bool {
    copy.unwrap_or(if cfg!(windows) { "manual" } else { "select" }) == "select"
}

/// `copyOnSelectRelease(event, ..)`: a release that ends a drag copies.
pub fn copy_on_select_release(event: &MouseEvent, toast: &Toasts) -> bool {
    if !event.is_dragging {
        return false;
    }
    copy(toast)
}

/// `copy(renderer, toast, clipboard)`: the selected text to the clipboard, with a toast. False when nothing is
/// selected (a click without a drag selects nothing). Copying never clears the selection.
pub fn copy(toast: &Toasts) -> bool {
    let Some(selection) = core_selection::get_selection() else { return false };
    if selection.is_start() && selection.behavior() == SelectionBehavior::Cell {
        return false;
    }
    let text = selection.selected_text();
    if text.is_empty() {
        return false;
    }
    // TODO(port-tui): `getClipboardText` of the focused renderable (the prompt's own clipboard form) when the
    // selection includes it.
    let toast = toast.clone();
    tokio::task::spawn_local(async move {
        match write_clipboard(&text).await {
            Ok(()) => toast.show(ToastInput::new(Variant::Info, "Copied to clipboard")),
            Err(error) => toast.error(error),
        }
    });
    true
}

/// `handleSelectionKey(renderer, toast, event, clipboard, copyOnSelect)`: with a selection, ctrl+c copies (or
/// clears it), escape clears it, and any other key clears it unless the focused editor holds it. True when the
/// key is consumed.
pub fn handle_selection_key(toast: &Toasts, event: &dyn KeymapEvent, copy_on_select: bool) -> bool {
    let Some(selection) = core_selection::get_selection() else { return false };
    let focus = renderer::current_focused_editor();
    let editing = focus.is_some_and(|f| f.has_selection() && selection.selected.contains(&f));
    // Kitty can report a non-Latin key name with a Latin base-layout C.
    if event.ctrl() && (event.name() == "c" || matches!(event.base_code(), Some(99) | Some(67))) {
        if (copy_on_select && !editing) || !copy(toast) {
            core_selection::clear_selection();
            return false;
        }
        return true;
    }
    if event.name() == "escape" {
        let text = if selection.is_start() && selection.behavior() == SelectionBehavior::Cell {
            String::new()
        } else {
            selection.selected_text()
        };
        core_selection::clear_selection();
        return !text.is_empty();
    }
    if editing {
        return false;
    }
    core_selection::clear_selection();
    false
}
