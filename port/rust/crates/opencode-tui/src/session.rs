//! The session route (`routes/session`) and submitting from the home prompt (`component/prompt/index.tsx`
//! `submit` without a session).
//!
//! | module | ports |
//! |---|---|
//! | [`route`] | `index.tsx`: `Session` (layout, scrolling, navigation, commands), `SessionRowView`, `SessionEntryView` |
//! | [`messages`] | `index.tsx`'s message views (user, switches, notices, compactions, footers, retries) |
//! | [`parts`] | `SessionPartView` and the extension points the tools slice fills (tool parts, prompts) |
//! | [`message_parts`] | `message-parts.tsx`: text and reasoning parts, `InlineToolRow` |
//! | [`rows`], [`grouping`] | `rows.ts`, `grouping/session.ts`, `grouping/tree.ts` |
//! | [`group_view`], [`activity_summary`] | `group-view.tsx`, `activity-summary.ts` |
//! | [`anchors`] | `anchors.ts`, `anchor-view.tsx` |
//! | [`mount_budget`], [`message_navigation`], [`history`] | `mount-budget.ts`, `message-navigation.ts`, `history.ts` |
//! | [`render_context`] | `render-context.tsx` |
//!
//! TODO(port-tui): the prompt slice: creating the session (with `--session`'s new ID), selecting its model and
//! agent, and sending the prompt from home.
use crate::ctx::Tui;

pub mod activity_summary;
pub mod anchors;
pub mod extras;
pub mod group_view;
pub mod grouping;
pub mod history;
pub mod message_navigation;
pub mod message_parts;
pub mod messages;
pub mod mount_budget;
pub mod parts;
pub mod render_context;
pub mod route;
pub mod rows;

pub fn submit_from_home(t: &Tui, value: String) {
    let trimmed = value.trim();
    // `exitWord`, and the `app.exit` command's slash name and aliases.
    if matches!(trimmed, "exit" | "quit" | ":q" | "/exit" | "/quit" | "/q") {
        (t.exit)();
        return;
    }
    // `promptModelWarning()`: nothing to send the prompt to. TODO(port-tui): the integration dialog when no
    // provider is connected.
    let has_model = t.data.models.with_untracked(|models| models.as_ref().is_some_and(|m| !m.is_empty()));
    if !has_model {
        t.toast.show(
            crate::toast::ToastInput::new(crate::toast::Variant::Warning, "Connect an integration to send prompts")
                .duration(3000),
        );
    }
}
