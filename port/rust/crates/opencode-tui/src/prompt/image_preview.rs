//! The prompt's image viewer (`component/dialog-image-preview.tsx`): the attached images one at a time, left and
//! right between them.
//!
//! TODO(port-tui): the Image renderable (and `createMediaSource`). Until it lands nothing decodes, so every image
//! shows as "No preview" in place of the picture, as TS shows an image it cannot decode.
use std::rc::Rc;

use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{FlexDirection, Justify, MouseEventType};

use crate::ctx::tui;
use crate::keymap::{Command, Layer, create_layer};
use crate::theme::SurfaceName;
use crate::ui::dialog::{
    DialogSize, FooterAction, FooterActions, FooterProps, HeaderProps, LayoutProps, dialog_header, dialog_layout,
    use_dialog,
};
use crate::ui::theme::use_theme;
use crate::util::rgba;

/// An attached image (`ImagePreviewItem`).
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewImage {
    pub uri: String,
    pub mention: Option<String>,
}

/// `<DialogImagePreview images initial>`.
pub fn dialog_image_preview(images: Vec<PreviewImage>, initial: usize) -> View {
    let dialog = use_dialog();
    let dimensions = tui().dimensions;
    let theme = use_theme().surface(SurfaceName::Dialog);
    let count = images.len();
    let index = create_signal(initial.min(count.saturating_sub(1))).0;
    let images = Rc::new(images);
    let current = {
        let images = images.clone();
        create_memo(move || images.get(index.get()).cloned())
    };
    // `createMediaSource(uri)`: no decoder yet (see the module doc).
    let failed = move || true;
    let image_height = create_memo(move || (dimensions.get().1 as i64 - 8).max(3) as f32);
    dialog.set_size(DialogSize::Xlarge);
    dialog.set_centered(true);
    let step: Rc<dyn Fn(i64)> = Rc::new(move |direction: i64| {
        if count < 2 {
            return;
        }
        let next = (index.get_untracked() as i64 + direction + count as i64) % count as i64;
        index.set(next as usize);
    });
    {
        let step = step.clone();
        create_layer(move || {
            let (back, forward) = (step.clone(), step.clone());
            Layer {
                mode: Some("modal"),
                commands: vec![
                    Command::inline("left", move || {
                        back(-1);
                        true
                    })
                    .title("Previous image")
                    .group("Dialog"),
                    Command::inline("right", move || {
                        forward(1);
                        true
                    })
                    .title("Next image")
                    .group("Dialog"),
                ],
                ..Layer::default()
            }
        });
    }
    let label = move || {
        if failed() {
            return "No preview".to_string();
        }
        current.get().and_then(|c| c.mention).unwrap_or_else(|| format!("Image {}", index.get() + 1))
    };
    let label_view = move || {
        let el = text();
        bind_always(
            move || {
                let th = theme.get();
                if failed() { rgba(th.text.feedback.error.base) } else { rgba(th.text.muted) }
            },
            move |c| el.set_fg(Some(c)),
        );
        el.set_wrap_mode(WrapMode::None);
        el.set_truncate(true);
        insert(el, View::dynamic(move || View::from(label())), At::Only);
        View::Node(el)
    };
    let footer_actions = {
        let step = step.clone();
        Rc::new(move || {
            if count < 2 {
                return FooterActions::default();
            }
            let (back, forward) = (step.clone(), step.clone());
            FooterActions {
                secondary: vec![],
                primary: vec![
                    FooterAction { title: "previous".into(), key: Some("←".into()), on_trigger: Some(Rc::new(move || back(-1))), ..FooterAction::default() },
                    FooterAction { title: "next".into(), key: Some("→".into()), on_trigger: Some(Rc::new(move || forward(1))), ..FooterAction::default() },
                ],
            }
        }) as Rc<dyn Fn() -> FooterActions>
    };
    let legacy_footer = {
        let step = step.clone();
        Rc::new(move || {
            let row = box_();
            row.set_flex_direction(FlexDirection::Row);
            row.set_justify_content(Justify::SpaceBetween);
            let previous = text();
            insert_node(row, previous, None);
            bind_always(move || rgba(theme.get().text.muted), move |c| previous.set_fg(Some(c)));
            {
                let step = step.clone();
                previous.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| step(-1))));
            }
            insert(previous, View::from(if count > 1 { "← previous" } else { "" }), At::Only);
            insert(row, label_view(), At::End);
            let next = text();
            insert_node(row, next, None);
            bind_always(move || rgba(theme.get().text.muted), move |c| next.set_fg(Some(c)));
            {
                let step = step.clone();
                next.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| step(1))));
            }
            insert(next, View::from(if count > 1 { "next →" } else { "" }), At::Only);
            View::Node(row)
        }) as Rc<dyn Fn() -> View>
    };
    dialog_layout(LayoutProps {
        id: Some("prompt-image-viewer".into()),
        header: Box::new(move || {
            component(move || {
                dialog_header(HeaderProps {
                    title: Some(Rc::new(move || format!("Image {} of {count}", index.get() + 1))),
                    ..HeaderProps::default()
                })
            })
        }),
        footer: Some(FooterProps { actions: Some(footer_actions), inset: None, children: Some(Rc::new(label_view)) }),
        legacy_footer: Some(legacy_footer),
        children: Box::new(move || {
            // `<image id="prompt-image-viewer-image" fit="fit" width="100%" height={imageHeight()}>`.
            let image = box_();
            image.set_id("prompt-image-viewer-image");
            image.set_width(Some(Value::Percent(100.0)));
            bind(move || image_height.get(), move |h| image.set_height(Some(Value::Point(h))));
            View::Node(image)
        }),
        ..LayoutProps::default()
    })
}
