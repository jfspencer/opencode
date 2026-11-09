//! `<DiffViewerFileTree>` (`feature-plugins/system/diff-viewer-file-tree.tsx`): the source header with the review
//! count, then the changed files as a tree (or a list), with the selection, rails, markers and status letters.
use std::collections::BTreeSet;
use std::rc::Rc;

use opencode_tui_core::color::text_attributes;
use opencode_tui_core::elements::{box_, scrollbox, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::scrollbox::ScrollBarOptions;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection, MouseEvent, MouseEventType};

use super::tree_utils::{FileTreeItem, FileTreeRow, NodeKind, build_file_tree, flatten_file_tree};
use crate::ctx::tui;
use crate::util::{rgba, string_width, tint, truncate_file_path};

const FILE_TREE_STATUS_WIDTH: f32 = 1.0;

/// `DiffViewerFileTreeProps`.
pub struct FileTreeProps {
    pub width: Rc<dyn Fn() -> i64>,
    pub files: Rc<dyn Fn() -> Vec<FileTreeItem>>,
    pub loading: Rc<dyn Fn() -> bool>,
    pub error: Rc<dyn Fn() -> bool>,
    pub list: bool,
    pub selected_file_index: Rc<dyn Fn() -> Option<usize>>,
    pub reviewed_file_names: Rc<dyn Fn() -> BTreeSet<String>>,
    pub expanded_nodes: Option<Rc<dyn Fn() -> BTreeSet<usize>>>,
    pub on_row_click: Option<Rc<dyn Fn(&FileTreeRow)>>,
    pub on_file_context_menu: Option<Rc<dyn Fn(usize, &mut MouseEvent)>>,
    pub source: Option<Rc<dyn Fn() -> String>>,
    pub source_detail: Option<Rc<dyn Fn() -> Option<String>>>,
    pub on_switch_source: Option<Rc<dyn Fn()>>,
    pub footer: Option<Rc<dyn Fn() -> View>>,
}

/// `truncateDirectoryChain(name, maxWidth)`: whole leading segments drop (`a/b/c/d` narrows to `…/c/d`).
fn truncate_directory_chain(name: &str, max_width: i64) -> String {
    if (string_width(name) as i64) <= max_width {
        return name.to_string();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut width = string_width("…/") as i64;
    for segment in name.split('/').rev() {
        let next = string_width(segment) as i64 + if kept.is_empty() { 0 } else { 1 };
        if width + next > max_width {
            break;
        }
        kept.insert(0, segment);
        width += next;
    }
    if kept.is_empty() {
        return truncate_file_path(name, max_width);
    }
    format!("…/{}", kept.join("/"))
}

/// `fileTreeRowStatus(row, files, reviewed)`.
fn row_status(row: &FileTreeRow, files: &[FileTreeItem], reviewed: bool) -> &'static str {
    let Some(index) = row.file_index else { return "" };
    if reviewed {
        return "✓";
    }
    match files.get(index).and_then(|f| f.status.as_deref()) {
        Some("modified") => "M",
        Some("added") => "A",
        Some("deleted") => "D",
        _ => "?",
    }
}

/// `scrollFileTreeRowIntoView(scroll, top, height)`.
fn scroll_row_into_view(scroll: Option<Element>, top: f64, height: f64) {
    let Some(scroll) = scroll.filter(|s| s.is_alive()) else { return };
    if top < scroll.scroll_top() {
        scroll.scroll_to(top);
        return;
    }
    let viewport = scroll.scroll_viewport().height;
    if top + height > scroll.scroll_top() + viewport {
        scroll.scroll_to(top + height - viewport);
    }
}

/// `<DiffViewerFileTree>`.
pub fn file_tree(props: FileTreeProps) -> View {
    let theme = tui().theme;
    let (source_hovered, set_source_hovered) = create_signal(false);
    let list = props.list;
    let files = props.files.clone();
    let tree = {
        let files = files.clone();
        create_memo(move || build_file_tree(&files()))
    };
    let expanded = props.expanded_nodes.clone();
    let rows = create_memo(move || {
        let tree = tree.get();
        if list {
            return flatten_file_tree(&tree, None).into_iter().filter(|r| r.file_index.is_some()).collect::<Vec<_>>();
        }
        let expanded = expanded.as_ref().map(|e| e());
        flatten_file_tree(&tree, expanded.as_ref())
    });
    // Quieter than muted text: markers are affordances, not content.
    let faint = create_memo_equals(move || theme.with(|t| tint(rgba(t.text.muted), rgba(t.background.raised.base), 0.45)), None);
    // Rails are pure texture; keep them barely above the surface.
    let rail = create_memo_equals(move || theme.with(|t| tint(rgba(t.text.muted), rgba(t.background.raised.base), 0.7)), None);
    let reviewed_count = {
        let files = files.clone();
        let reviewed = props.reviewed_file_names.clone();
        create_memo(move || {
            let reviewed = reviewed();
            files().iter().filter(|f| reviewed.contains(&f.file)).count()
        })
    };
    let width = props.width.clone();
    let content_width = Rc::new(move || 0.max(width() - 4 - FILE_TREE_STATUS_WIDTH as i64 - 1));
    let scroll: Rc<std::cell::Cell<Option<Element>>> = Rc::default();

    {
        let selected = props.selected_file_index.clone();
        let scroll = scroll.clone();
        create_effect(move || {
            let selected = selected();
            let Some(index) = rows.with(|r| r.iter().position(|row| row.file_index.is_some() && row.file_index == selected)) else {
                return;
            };
            let top = (index * if list { 3 } else { 1 }) as f64;
            let height = if list { 2.0 } else { 1.0 };
            scroll_row_into_view(scroll.get(), top, height);
            let scroll = scroll.clone();
            renderer::on_frame(move || scroll_row_into_view(scroll.get(), top, height), true);
            renderer::request_render();
        });
    }

    let root = box_();
    {
        let width = props.width.clone();
        bind(move || width(), move |w| root.set_width(Some(Value::Point(w as f32))));
    }
    root.set_height(Some(Value::Percent(100.0)));
    root.set_min_width(Some(Value::Point(0.0)));
    root.set_min_height(Some(Value::Point(0.0)));
    root.set_flex_shrink(Some(0.0));
    root.set_flex_direction(FlexDirection::Column);
    let edge = box_();
    insert_node(root, edge, None);
    edge.set_id("diff-tree-top-edge");
    edge.set_height(Some(Value::Point(1.0)));
    edge.set_flex_shrink(Some(0.0));
    bind_always(move || rgba(theme.with(|t| t.background.raised.base)), move |c| edge.set_background_color(Some(c)));
    let body = box_();
    insert_node(root, body, None);
    body.set_flex_grow(Some(1.0));
    body.set_min_width(Some(Value::Point(0.0)));
    body.set_min_height(Some(Value::Point(0.0)));
    body.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    body.set_padding(Edge::Left, Some(Value::Point(2.0)));
    body.set_padding(Edge::Right, Some(Value::Point(2.0)));
    bind_always(move || rgba(theme.with(|t| t.background.raised.base)), move |c| body.set_background_color(Some(c)));

    let header = box_();
    insert_node(body, header, None);
    header.set_id("diff-source-header");
    header.set_height(Some(Value::Point(1.0)));
    header.set_flex_shrink(Some(0.0));
    header.set_flex_direction(FlexDirection::Row);
    header.set_margin(Edge::Bottom, Some(Value::Point(1.0)));
    header.set_gap(2, Some(Value::Point(1.0)));
    let switch_box = box_();
    insert_node(header, switch_box, None);
    switch_box.set_id("diff-source-switch");
    switch_box.set_flex_direction(FlexDirection::Row);
    switch_box.set_flex_grow(Some(1.0));
    switch_box.set_min_width(Some(Value::Point(0.0)));
    switch_box.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| set_source_hovered.set(true))));
    switch_box.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| set_source_hovered.set(false))));
    {
        let on_switch = props.on_switch_source.clone();
        switch_box.set_on_mouse(
            Some(MouseEventType::Up),
            Some(Rc::new(move |event| {
                if event.button != 0 {
                    return;
                }
                event.stop_propagation();
                if let Some(f) = &on_switch {
                    f();
                }
            })),
        );
    }
    let source_label = text();
    insert_node(switch_box, source_label, None);
    let has_switch = props.on_switch_source.is_some();
    bind_always(
        move || {
            theme.with(|t| {
                if has_switch {
                    if source_hovered.get() { rgba(t.text.action.secondary.hovered) } else { rgba(t.text.action.secondary.base) }
                } else {
                    rgba(t.text.base)
                }
            })
        },
        move |c| source_label.set_fg(Some(c)),
    );
    source_label.set_attributes(Some(text_attributes::BOLD));
    source_label.set_flex_shrink(Some(0.0));
    source_label.set_wrap_mode(WrapMode::None);
    source_label.set_selectable(false);
    {
        let source = props.source.clone();
        insert(source_label, View::dynamic(move || View::from(source.as_ref().map(|s| s()).unwrap_or_else(|| "Files".into()))), At::Only);
    }
    if let Some(detail) = props.source_detail.clone() {
        let when = detail.clone();
        insert(
            switch_box,
            show(
                move || when().is_some(),
                move || {
                    let el = text();
                    bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| el.set_fg(Some(c)));
                    el.set_selectable(false);
                    el.set_flex_grow(Some(1.0));
                    el.set_min_width(Some(Value::Point(0.0)));
                    el.set_wrap_mode(WrapMode::None);
                    el.set_truncate(true);
                    let detail = detail.clone();
                    insert(el, View::dynamic(move || View::from(format!(" · {}", detail().unwrap_or_default()))), At::Only);
                    View::Node(el)
                },
                None,
            ),
            At::End,
        );
    }
    let count = text();
    insert_node(header, count, None);
    count.set_id("diff-review-count");
    bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| count.set_fg(Some(c)));
    count.set_wrap_mode(WrapMode::None);
    count.set_flex_shrink(Some(0.0));
    {
        let files = files.clone();
        let has_source = props.source.is_some();
        insert(
            count,
            View::dynamic(move || View::from(format!("{}/{}{}", reviewed_count.get(), files().len(), if has_source { "" } else { " reviewed" }))),
            At::Only,
        );
    }

    let scroll_el = scrollbox();
    insert_node(body, scroll_el, None);
    scroll.set(Some(scroll_el));
    scroll_el.set_id("diff-files");
    scroll_el.set_flex_grow(Some(1.0));
    scroll_el.set_min_height(Some(Value::Point(0.0)));
    scroll_el.set_scrollbar_options(Some(true), &ScrollBarOptions { visible: Some(Some(false)), ..Default::default() });
    scroll_el.set_scrollbar_options(Some(false), &ScrollBarOptions { visible: Some(Some(false)), ..Default::default() });

    let loading = props.loading.clone();
    let error = props.error.clone();
    let files_empty = {
        let files = files.clone();
        move || files().is_empty()
    };
    let rows_view = {
        let files = files.clone();
        let selected = props.selected_file_index.clone();
        let reviewed = props.reviewed_file_names.clone();
        let expanded = props.expanded_nodes.clone();
        let on_row_click = props.on_row_click.clone();
        let on_menu = props.on_file_context_menu.clone();
        let content_width = content_width.clone();
        move || {
            let wrapper = box_();
            wrapper.set_flex_shrink(Some(0.0));
            wrapper.set_gap(2, Some(Value::Point(if list { 1.0 } else { 0.0 })));
            let files = files.clone();
            let selected = selected.clone();
            let reviewed = reviewed.clone();
            let expanded = expanded.clone();
            let on_row_click = on_row_click.clone();
            let on_menu = on_menu.clone();
            let content_width = content_width.clone();
            insert(
                wrapper,
                for_each(
                    move || rows.get(),
                    |row: &FileTreeRow| format!("{}:{}:{}", row.id, row.depth, row.name),
                    move |row, _| {
                        let (hovered, set_hovered) = create_signal(false);
                        let r = row.get_untracked();
                        let is_selected = {
                            let selected = selected.clone();
                            let file_index = r.file_index;
                            move || file_index.is_some() && selected() == file_index
                        };
                        let is_reviewed = {
                            let files = files.clone();
                            let reviewed = reviewed.clone();
                            let file_index = r.file_index;
                            move || file_index.and_then(|i| files().get(i).map(|f| f.file.clone())).is_some_and(|f| reviewed().contains(&f))
                        };
                        let marker = {
                            let expanded = expanded.clone();
                            let r = r.clone();
                            move || -> String {
                                if r.kind != NodeKind::Directory {
                                    return "≡ ".into();
                                }
                                match &expanded {
                                    Some(e) if !e().contains(&r.id) => "▸ ".into(),
                                    _ => "▾ ".into(),
                                }
                            }
                        };
                        let indent = {
                            let content_width = content_width.clone();
                            let depth = r.depth as i64;
                            create_memo(move || {
                                if list {
                                    return String::new();
                                }
                                "│ ".repeat(0.max(depth.min((content_width() - 3).div_euclid(2))) as usize)
                            })
                        };
                        let el = box_();
                        el.set_id(&match r.file_index {
                            None => format!("diff-folder-row-{}", r.id),
                            Some(i) => format!("diff-file-row-{i}"),
                        });
                        el.set_flex_direction(FlexDirection::Column);
                        el.set_width(Some(Value::Percent(100.0)));
                        el.set_height(Some(Value::Point(if list { 2.0 } else { 1.0 })));
                        el.set_flex_shrink(Some(0.0));
                        bind_always(
                            move || theme.with(|t| if hovered.get() { rgba(t.background.raised.high) } else { rgba(t.background.raised.base) }),
                            move |c| el.set_background_color(Some(c)),
                        );
                        el.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| set_hovered.set(true))));
                        el.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| set_hovered.set(false))));
                        if let (Some(on_menu), Some(index)) = (on_menu.clone(), r.file_index) {
                            el.set_on_mouse(Some(MouseEventType::Down), Some(Rc::new(move |event| on_menu(index, event))));
                        }
                        {
                            let on_row_click = on_row_click.clone();
                            el.set_on_mouse(
                                Some(MouseEventType::Up),
                                Some(Rc::new(move |event| {
                                    if event.button != 0 {
                                        return;
                                    }
                                    event.stop_propagation();
                                    if let Some(f) = &on_row_click {
                                        f(&row.get_untracked());
                                    }
                                })),
                            );
                        }
                        let line = box_();
                        insert_node(el, line, None);
                        line.set_flex_direction(FlexDirection::Row);
                        line.set_height(Some(Value::Point(1.0)));
                        let lead = text();
                        insert_node(line, lead, None);
                        lead.set_wrap_mode(WrapMode::None);
                        lead.set_flex_shrink(Some(0.0));
                        let rail_span = span();
                        insert_node(lead, rail_span, None);
                        bind_always(move || rail.get(), move |c| rail_span.set_span_style(Some(c), None, 0, 0));
                        insert(rail_span, View::dynamic(move || View::from(indent.get())), At::Only);
                        let marker_span = span();
                        insert_node(lead, marker_span, None);
                        bind_always(move || faint.get(), move |c| marker_span.set_span_style(Some(c), None, 0, 0));
                        {
                            let marker = marker.clone();
                            insert(marker_span, View::dynamic(move || View::from(marker())), At::Only);
                        }
                        let name_box = box_();
                        insert_node(line, name_box, None);
                        name_box.set_flex_grow(Some(1.0));
                        name_box.set_min_width(Some(Value::Point(0.0)));
                        name_box.set_margin(Edge::Right, Some(Value::Point(1.0)));
                        let name = text();
                        insert_node(name_box, name, None);
                        let foreground = {
                            let is_selected = is_selected.clone();
                            let is_reviewed = is_reviewed.clone();
                            let kind = r.kind;
                            move || {
                                theme.with(|t| {
                                    if kind == NodeKind::Directory {
                                        rgba(t.text.muted)
                                    } else if is_selected() {
                                        rgba(t.text.action.primary.selected)
                                    } else if is_reviewed() {
                                        rgba(t.text.muted)
                                    } else {
                                        rgba(t.text.base)
                                    }
                                })
                            }
                        };
                        {
                            let foreground = foreground.clone();
                            bind_always(foreground, move |c| name.set_fg(Some(c)));
                        }
                        {
                            let is_selected = is_selected.clone();
                            bind(is_selected, move |s| name.set_attributes(s.then_some(text_attributes::BOLD)));
                        }
                        name.set_wrap_mode(WrapMode::None);
                        name.set_truncate(true);
                        {
                            let content_width = content_width.clone();
                            let marker = marker.clone();
                            let r = r.clone();
                            insert(
                                name,
                                View::dynamic(move || {
                                    let width = content_width() - string_width(&indent.get()) as i64 - string_width(&marker()) as i64;
                                    View::from(if r.kind == NodeKind::Directory {
                                        truncate_directory_chain(&r.name, width)
                                    } else {
                                        truncate_file_path(&r.name, width)
                                    })
                                }),
                                At::Only,
                            );
                        }
                        let status = text();
                        insert_node(line, status, None);
                        {
                            let is_reviewed = is_reviewed.clone();
                            let files = files.clone();
                            let file_index = r.file_index;
                            bind_always(
                                move || {
                                    theme.with(|t| {
                                        if is_reviewed() {
                                            return rgba(t.text.muted);
                                        }
                                        match file_index.and_then(|i| files().get(i).and_then(|f| f.status.clone())).as_deref() {
                                            Some("added") => rgba(t.diff.text.added),
                                            Some("deleted") => rgba(t.diff.text.removed),
                                            _ => rgba(t.text.muted),
                                        }
                                    })
                                },
                                move |c| status.set_fg(Some(c)),
                            );
                        }
                        status.set_wrap_mode(WrapMode::None);
                        status.set_width(Some(Value::Point(FILE_TREE_STATUS_WIDTH)));
                        status.set_flex_shrink(Some(0.0));
                        {
                            let files = files.clone();
                            let is_reviewed = is_reviewed.clone();
                            let r = r.clone();
                            insert(status, View::dynamic(move || View::from(row_status(&r, &files(), is_reviewed()))), At::Only);
                        }
                        if list {
                            let parent = text();
                            insert_node(el, parent, None);
                            bind_always(foreground, move |c| parent.set_fg(Some(c)));
                            bind(move || is_selected() || hovered.get(), move |dim| parent.set_attributes(dim.then_some(text_attributes::DIM)));
                            parent.set_margin(Edge::Left, Some(Value::Point(string_width(&marker()) as f32)));
                            parent.set_wrap_mode(WrapMode::None);
                            parent.set_truncate(true);
                            let files = files.clone();
                            let content_width = content_width.clone();
                            let file_index = r.file_index;
                            insert(
                                parent,
                                View::dynamic(move || {
                                    let file = file_index.and_then(|i| files().get(i).map(|f| f.file.clone())).unwrap_or_default();
                                    let directory = &file[..file.rfind('/').unwrap_or(0)];
                                    View::from(if directory.is_empty() {
                                        String::new()
                                    } else {
                                        truncate_directory_chain(directory, content_width() - string_width(&marker()) as i64)
                                    })
                                }),
                                At::Only,
                            );
                        }
                        View::Node(el)
                    },
                    None,
                ),
                At::Only,
            );
            View::Node(wrapper)
        }
    };
    insert(
        scroll_el,
        switch(
            vec![
                when(move || loading() || error(), || View::Node(text())),
                when(files_empty.clone(), move || {
                    let el = text();
                    bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| el.set_fg(Some(c)));
                    insert(el, View::from("No files"), At::Only);
                    View::Node(el)
                }),
                when(move || !files_empty(), rows_view),
            ],
            None,
        ),
        At::Only,
    );
    if let Some(footer) = props.footer.clone() {
        let el = box_();
        insert_node(body, el, None);
        el.set_flex_shrink(Some(0.0));
        el.set_padding(Edge::Top, Some(Value::Point(1.0)));
        el.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
        insert(el, View::dynamic(move || footer()), At::Only);
    }
    View::Node(root)
}
