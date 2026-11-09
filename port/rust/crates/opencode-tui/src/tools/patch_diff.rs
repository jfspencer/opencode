//! `<PatchDiff>` (`component/patch-diff.tsx`): a patch as one `<diff>` per hunk, with the hunk headers between
//! them and one gutter width for all, so the code columns line up.
//!
//! TODO(port-tui): `VirtualAddedPatch` (large new files in the diff viewer, chunks mounted around the viewport)
//! comes with the diff viewer; until then every patch renders as hunks.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::LazyLock;

use opencode_tui_core::color::RGBA;
use opencode_tui_core::elements::{box_, diff, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::diff::{DiffGutterOptions, DiffView};
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::Element;
use regex::Regex;

use super::diff_util::{PatchHunk, split_patch_hunks};
use crate::theme::ResolvedTheme;
use crate::util::rgba;

type Prop<T> = Option<Rc<dyn Fn() -> T>>;

/// The `<diff>` props PatchDiff passes through (`diffProps`), set in the order the tool views write them.
#[derive(Clone, Default)]
pub struct DiffProps {
    pub view: Prop<DiffView>,
    pub filetype: Prop<Option<String>>,
    pub show_line_numbers: Option<bool>,
    pub width: Option<Value>,
    pub wrap_mode: Prop<WrapMode>,
    pub fg: Prop<RGBA>,
    pub added_bg: Prop<RGBA>,
    pub removed_bg: Prop<RGBA>,
    pub context_bg: Prop<RGBA>,
    pub added_sign_color: Prop<RGBA>,
    pub removed_sign_color: Prop<RGBA>,
    pub line_number_fg: Prop<RGBA>,
    pub added_line_number_bg: Prop<RGBA>,
    pub removed_line_number_bg: Prop<RGBA>,
}

impl DiffProps {
    /// The props every tool view and the permission prompt give a patch: the theme's diff colors.
    pub fn themed(
        theme: Signal<ResolvedTheme>,
        view: Rc<dyn Fn() -> DiffView>,
        filetype: Rc<dyn Fn() -> Option<String>>,
        wrap_mode: Rc<dyn Fn() -> WrapMode>,
    ) -> DiffProps {
        let color = |f: fn(&ResolvedTheme) -> crate::theme::Color| -> Prop<RGBA> {
            Some(Rc::new(move || rgba(theme.with(|t| f(t)))))
        };
        DiffProps {
            view: Some(view),
            filetype: Some(filetype),
            show_line_numbers: Some(true),
            width: Some(Value::Percent(100.0)),
            wrap_mode: Some(wrap_mode),
            fg: color(|t| t.text.base),
            added_bg: color(|t| t.diff.background.added),
            removed_bg: color(|t| t.diff.background.removed),
            context_bg: color(|t| t.diff.background.context),
            added_sign_color: color(|t| t.diff.highlight.added),
            removed_sign_color: color(|t| t.diff.highlight.removed),
            line_number_fg: color(|t| t.diff.line_number.text),
            added_line_number_bg: color(|t| t.diff.line_number.background.added),
            removed_line_number_bg: color(|t| t.diff.line_number.background.removed),
        }
    }
}

/// `PatchDiffRef`: the hunks' `<diff>`s in order.
#[derive(Clone)]
pub struct PatchDiffRef {
    nodes: Rc<RefCell<BTreeMap<usize, Element>>>,
}

impl PatchDiffRef {
    pub fn hunks(&self) -> Vec<Element> {
        self.nodes.borrow().values().copied().filter(|n| !n.is_destroyed()).collect()
    }
}

pub struct PatchDiffProps {
    pub diff: Rc<dyn Fn() -> String>,
    pub hunk_fg: Rc<dyn Fn() -> RGBA>,
    pub line_number_bg: Rc<dyn Fn() -> RGBA>,
    pub on_ref: Option<Rc<dyn Fn(PatchDiffRef)>>,
    /// Extra gutter columns before the line numbers, painted in each row's color.
    pub gutter_padding: Option<u32>,
    pub diff_props: DiffProps,
}

static HUNK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^@@ -([0-9]+)(?:,([0-9]+))? \+([0-9]+)(?:,([0-9]+))? @@").unwrap());

/// The gutter all hunks share: room for the largest line number of any hunk.
fn gutter_for(diff: &str, gutter_padding: u32) -> DiffGutterOptions {
    let padding_left = 1 + gutter_padding;
    let mut max = 1.0f64;
    for captures in HUNK.captures_iter(diff) {
        let number = |i: usize| captures.get(i).and_then(|m| m.as_str().parse::<f64>().ok());
        let old = number(1).unwrap_or(0.0) + number(2).unwrap_or(1.0) - 1.0;
        let new = number(3).unwrap_or(0.0) + number(4).unwrap_or(1.0) - 1.0;
        max = max.max(old).max(new);
    }
    let digits = opencode_tui_sync::js::number(max).len() as u32;
    DiffGutterOptions { padding_left: Some(padding_left), padding_right: None, min_digits: Some(digits) }
}

/// Binds one reactive `<diff>` prop.
fn bind_prop<T: 'static>(prop: &Prop<T>, set: impl Fn(T) + 'static) {
    if let Some(prop) = prop.clone() {
        bind_always(move || prop(), set);
    }
}

/// `<PatchDiff>`.
pub fn patch_diff(props: PatchDiffProps) -> View {
    let diff_text = props.diff.clone();
    let hunks = create_memo(move || Rc::new(split_patch_hunks(&diff_text())));
    let gutter = {
        let diff_text = props.diff.clone();
        let padding = props.gutter_padding.unwrap_or(0);
        create_memo(move || gutter_for(&diff_text(), padding))
    };
    let nodes: Rc<RefCell<BTreeMap<usize, Element>>> = Rc::default();
    if let Some(on_ref) = &props.on_ref {
        on_ref(PatchDiffRef { nodes: nodes.clone() });
    }
    let hunk_fg = props.hunk_fg.clone();
    let line_number_bg = props.line_number_bg.clone();
    let diff_props = Rc::new(props.diff_props);
    for_each(
        move || hunks.get().iter().cloned().enumerate().collect::<Vec<(usize, PatchHunk)>>(),
        |(index, hunk): &(usize, PatchHunk)| format!("{index}:{}", hunk.patch),
        move |item, index| {
            let hunk_fg = hunk_fg.clone();
            let line_number_bg = line_number_bg.clone();
            let diff_props = diff_props.clone();
            let nodes = nodes.clone();
            let header = {
                let line_number_bg = line_number_bg.clone();
                show(
                    move || index.get() > 0,
                    move || {
                        let row = box_();
                        row.set_width(Some(Value::Percent(100.0)));
                        row.set_height(Some(Value::Point(1.0)));
                        {
                            let bg = line_number_bg.clone();
                            bind_always(move || bg(), move |c| row.set_background_color(Some(c)));
                        }
                        let label = text();
                        insert_node(row, label, None);
                        {
                            let fg = hunk_fg.clone();
                            bind_always(move || fg(), move |c| label.set_fg(Some(c)));
                        }
                        {
                            let bg = line_number_bg.clone();
                            bind_always(move || bg(), move |c| label.set_bg(Some(c)));
                        }
                        insert(
                            label,
                            View::dynamic(move || View::from(format!(" {}", item.get().1.header.clone().unwrap_or_default()))),
                            At::Only,
                        );
                        View::Node(row)
                    },
                    None,
                )
            };
            let el = diff();
            // `{...diffProps}`, then ref, diff, minHeight, lineNumberBg and gutter.
            bind_prop(&diff_props.view, move |v| el.set_diff_view(v));
            bind_prop(&diff_props.filetype, move |v| el.set_diff_filetype(v));
            if let Some(show) = diff_props.show_line_numbers {
                el.set_diff_show_line_numbers(show);
            }
            if let Some(width) = diff_props.width {
                el.set_width(Some(width));
            }
            bind_prop(&diff_props.wrap_mode, move |v| el.set_diff_wrap_mode(Some(v)));
            bind_prop(&diff_props.fg, move |v| el.set_diff_fg(Some(v)));
            bind_prop(&diff_props.added_bg, move |v| el.set_diff_added_bg(Some(v)));
            bind_prop(&diff_props.removed_bg, move |v| el.set_diff_removed_bg(Some(v)));
            bind_prop(&diff_props.context_bg, move |v| el.set_diff_context_bg(Some(v)));
            bind_prop(&diff_props.added_sign_color, move |v| el.set_diff_added_sign_color(Some(v)));
            bind_prop(&diff_props.removed_sign_color, move |v| el.set_diff_removed_sign_color(Some(v)));
            bind_prop(&diff_props.line_number_fg, move |v| el.set_diff_line_number_fg(Some(v)));
            bind_prop(&diff_props.added_line_number_bg, move |v| el.set_diff_added_line_number_bg(Some(v)));
            bind_prop(&diff_props.removed_line_number_bg, move |v| el.set_diff_removed_line_number_bg(Some(v)));
            {
                let i = index.get_untracked();
                nodes.borrow_mut().insert(i, el);
                let nodes = nodes.clone();
                on_cleanup(move || {
                    nodes.borrow_mut().remove(&i);
                });
            }
            bind(move || item.with(|(_, h)| h.patch.clone()), move |patch| el.set_diff(&patch));
            bind(
                move || item.with(|(_, h)| h.rows),
                move |rows| el.set_min_height(rows.map(|r| Value::Point(r as f32))),
            );
            {
                let bg = line_number_bg.clone();
                bind_always(move || bg(), move |c| el.set_diff_line_number_bg(Some(c)));
            }
            bind(move || gutter.get(), move |g| el.set_diff_gutter(g));
            View::list([header, View::Node(el)])
        },
        None,
    )
}
