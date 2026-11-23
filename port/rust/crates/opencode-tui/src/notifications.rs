//! `feature-plugins/system/notifications.ts` (`opencode.notifications`): the toasts and attention signals a
//! session's lifecycle produces. Only the failure toast is user-visible in the TUI here; sounds and desktop
//! notifications belong to the attention context, which is not ported.
use opencode_tui_sync::json::JsonExt;

use crate::ctx::Tui;

/// Install the plugin's event listener for the app's lifetime.
pub fn install(t: &Tui) {
    let toast = t.toast.clone();
    let unsubscribe = t.data.store.on("session.execution.failed", move |event| {
        let data = event.data();
        let error = data.get("error");
        let kind = error.and_then(|e| e.str("type"));
        // A model's free/go limit is a normal stop, not a failure worth interrupting the user for.
        if matches!(kind, Some("provider.free-limit") | Some("provider.go-limit")) {
            return;
        }
        let message = error.and_then(|e| e.str("message")).unwrap_or("Unknown error").to_string();
        toast.show(crate::toast::ToastInput::new(crate::toast::Variant::Error, message).title("Session failed"));
    });
    opencode_tui_core::reactive::on_cleanup(move || unsubscribe.call());
}
