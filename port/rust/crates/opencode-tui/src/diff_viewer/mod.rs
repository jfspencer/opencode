//! The diff viewer (`feature-plugins/system/diff-viewer*.tsx`, the built-in `opencode.diffs` plugin): a
//! session's changes (the branch, its commits, the working tree, or the last turn) beside the session or full
//! screen: the source header, a file tree on wide screens, one card per file with its sticky header and
//! PatchDiff, hunk and file navigation, reviewed files, single-file mode, split or unified views, the footer.
//!
//! - [`tree_utils`]: `diff-viewer-file-tree-utils.ts`; [`file_tree`]: `diff-viewer-file-tree.tsx`.
//!
//! The plugin's context is the TUI's own (data, client, keymap, config); its panel input is [`PanelInput`] (the
//! tabs slice's `PanelSlotInput`).
//!
//! TODO(port-tui): the base-branch picker (`DiffBaseDialog`) and the file menu (`diff-viewer-file-menu.tsx`); the
//! tabs slice's `crate::storage` (`memory("review")` is a thread-local signal until it lands); PatchDiff's virtual
//! chunks for very large new files (they render whole here, with the same rows as the reference); the full-screen
//! plugin route ("diff") for `/diff` outside a session. An image preview draws the block its `fit` occupies, since
//! the terminal image protocol is not ported.
pub mod file_tree;
pub mod tree_utils;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::border::{BorderCharacters, border_chars_to_array};
use opencode_tui_core::color::{RGBA, text_attributes};
use opencode_tui_core::elements::{box_, scrollbox, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::diff::DiffView;
use opencode_tui_core::renderables::scrollbox::{ScrollBarOptions, ScrollUnit};
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection, MouseEventType};
use opencode_tui_sync::client::RequestOptions;
use opencode_tui_sync::data::SessionStatus;
use opencode_tui_sync::json::JsonExt;

use self::file_tree::{FileTreeProps, file_tree};
use self::tree_utils::*;
use crate::ctx::tui;
use crate::keymap::{Command, Layer, create_layer, use_shortcuts};
use crate::tools::filetype::filetype;
use crate::tools::patch_diff::{DiffProps, PatchDiffProps, PatchDiffRef, patch_diff};
use crate::tools::scroll::get_scroll_acceleration;
use crate::util::{rgba, string_width, truncate_file_path};

pub const ROUTE: &str = "diff";
const MIN_SPLIT_WIDTH: i64 = 100;
const FILE_TREE_MIN_WIDTH: i64 = 30;
const FILE_TREE_MAX_WIDTH: i64 = 40;
const FULL_INSET: i64 = 2;
/// `SESSION_PANEL_SPLIT_MIN_WIDTH` (ui/layout.ts): 64 + 44.
const SESSION_PANEL_SPLIT_MIN_WIDTH: i64 = 64 + 44;
// The full layout keeps its file tree and inset cards only while its patches stay as wide as the narrowest docked
// panel, an equal split at the host's split threshold. Narrower full screens use the compact panel layout.
const FULL_LAYOUT_MIN_WIDTH: i64 = FILE_TREE_MIN_WIDTH + FULL_INSET * 2 + SESSION_PANEL_SPLIT_MIN_WIDTH / 2;
const FILE_HEADER_HEIGHT: f64 = 2.0;
const VCS_DIFF_CONTEXT_LINES: f64 = 12.0;

/// `DiffSource`: `branch | committed | working | turn`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffMode {
    Branch,
    Committed,
    Working,
    Turn,
}

impl DiffMode {
    pub fn parse(value: &str) -> Option<DiffMode> {
        Some(match value {
            "branch" => DiffMode::Branch,
            "committed" => DiffMode::Committed,
            "working" => DiffMode::Working,
            "turn" => DiffMode::Turn,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            DiffMode::Branch => "branch",
            DiffMode::Committed => "committed",
            DiffMode::Working => "working",
            DiffMode::Turn => "turn",
        }
    }
    /// `DIFF_SOURCES[mode]`: (label, description).
    pub fn source(self) -> (&'static str, &'static str) {
        match self {
            DiffMode::Branch => ("All", "Branch + local changes"),
            DiffMode::Committed => ("Committed", "Branch commits only"),
            DiffMode::Working => ("Uncommitted", "Local changes only"),
            DiffMode::Turn => ("Last turn", "Latest session turn"),
        }
    }
    fn needs_base(self) -> bool {
        matches!(self, DiffMode::Branch | DiffMode::Committed)
    }
}

/// `DiffFile`.
#[derive(Clone, Debug, PartialEq)]
pub struct DiffFile {
    pub file: String,
    pub patch: Option<String>,
    pub additions: f64,
    pub deletions: f64,
    pub status: String,
}

/// The patch order of a file list: the directory tree's file rows, in display order.
fn patch_indexes_of(files: &[DiffFile]) -> Vec<usize> {
    let tree = build_file_tree(&files.iter().map(|file| FileTreeItem { file: file.file.clone(), status: Some(file.status.clone()) }).collect::<Vec<_>>());
    ordered_patch_file_indexes(&flatten_file_tree(&tree, None))
}

/// `normalizeDiffs(diffs)`.
fn normalize_diffs(value: Option<&Json>) -> Vec<DiffFile> {
    let Some(Json::Array(items)) = value else { return Vec::new() };
    items
        .iter()
        .map(|item| DiffFile {
            file: item.str("file").unwrap_or("").to_string(),
            patch: item.str("patch").map(str::to_string),
            additions: item.num("additions").unwrap_or(0.0),
            deletions: item.num("deletions").unwrap_or(0.0),
            status: item.str("status").unwrap_or("modified").to_string(),
        })
        .collect()
}

/// `isDiffImageFile(file)`.
pub fn is_diff_image_file(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".webp", ".gif"].iter().any(|ext| lower.ends_with(ext))
}

/// `PanelInput`: the panel the viewer is docked in (`presentation` "panel"), or full screen.
#[derive(Clone)]
pub struct PanelInput {
    pub name: String,
    pub session_id: String,
    pub width: Rc<dyn Fn() -> i64>,
    pub docked: Rc<dyn Fn() -> bool>,
    pub focused: Rc<dyn Fn() -> bool>,
    pub focus: Rc<dyn Fn()>,
    pub close: Rc<dyn Fn()>,
    pub toggle_fullscreen: Rc<dyn Fn()>,
}

/// `{ source?, bases }`, the viewer's TUI-lifetime memory.
#[derive(Clone, Default, PartialEq)]
struct ReviewMemory {
    source: Option<DiffMode>,
    bases: HashMap<String, String>,
}

/// The viewer's UI state. It is separate from [`ReviewMemory`] because the diff resource tracks that memory, and a
/// UI change must not refetch the diff; it survives a move between the docked panel and full screen (one component
/// in TS).
#[derive(Clone, Default, PartialEq)]
struct UiMemory {
    single: Option<bool>,
    view: Option<String>,
    tree: Option<bool>,
    reviewed: std::collections::BTreeSet<String>,
    selected: Option<String>,
}

thread_local! {
    static REVIEW: Cell<Option<Signal<ReviewMemory>>> = const { Cell::new(None) };
}

thread_local! {
    static UI: Cell<Option<Signal<UiMemory>>> = const { Cell::new(None) };
}

/// The viewer's UI memory, for the TUI's lifetime.
fn ui_memory() -> Signal<UiMemory> {
    UI.with(|u| match u.get() {
        Some(signal) if signal.is_alive() => signal,
        _ => {
            let signal = create_root(|_| create_signal_with(UiMemory::default(), None).0);
            u.set(Some(signal));
            signal
        }
    })
}

/// `context.storage.memory("review", { initial: { bases: {} } })`.
fn review_memory() -> Signal<ReviewMemory> {
    REVIEW.with(|r| match r.get() {
        Some(signal) if signal.is_alive() => signal,
        _ => {
            let signal = create_root(|_| create_signal_with(ReviewMemory::default(), None).0);
            r.set(Some(signal));
            signal
        }
    })
}

/// The diff a source resolves to.
#[derive(Clone, Debug, PartialEq, Default)]
struct DiffResult {
    /// `{ name, ref }` of the base, when one is reported.
    base: Option<(String, String)>,
    files: Vec<DiffFile>,
}

#[derive(Clone, Debug, PartialEq, Default)]
struct Resource {
    loading: bool,
    error: bool,
    value: Option<DiffResult>,
}

/// `<DiffViewer sessionID initialMode panel onClose>`.
pub fn diff_viewer(session_id: Option<String>, initial_mode: Option<DiffMode>, panel: Option<PanelInput>, on_close: Rc<dyn Fn()>) -> View {
    let t = tui();
    let config = t.config_context.clone();
    let store = t.data.store.clone();
    let client = t.data.client_context.clone();
    let memory = review_memory();
    let sources: Rc<Vec<DiffMode>> = Rc::new(if session_id.is_some() {
        vec![DiffMode::Branch, DiffMode::Committed, DiffMode::Working, DiffMode::Turn]
    } else {
        vec![DiffMode::Branch, DiffMode::Committed, DiffMode::Working]
    });
    let initial = initial_mode
        .or_else(|| memory.with_untracked(|m| m.source))
        .or_else(|| config.data_untracked().diffs.as_ref().and_then(|d| d.source.as_deref()).and_then(DiffMode::parse))
        .unwrap_or(DiffMode::Branch);
    let (mode, set_mode) = create_signal(if sources.contains(&initial) { initial } else { DiffMode::Branch });
    let location = {
        let store = store.clone();
        let session_id = session_id.clone();
        create_memo_equals(
            move || {
                session_id
                    .as_ref()
                    .and_then(|id| store.session.get(id).and_then(|s| s.get("location").cloned()))
                    .unwrap_or_else(|| store.location.default())
            },
            Some(|a: &Json, b: &Json| a.str("directory") == b.str("directory")),
        )
    };
    let base_key = {
        let store = store.clone();
        create_memo(move || {
            let location = location.get();
            let branch = store.location.vcs.info(Some(&location)).and_then(|v| v.get("branch").and_then(|b| b.str("current")).map(str::to_string));
            opencode_tui_sync::client::stringify(&Json::Array(vec![
                Json::String(opencode_tui_sync::data::location_key(&location)),
                branch.map(Json::String).unwrap_or(Json::Null),
            ]))
        })
    };
    let selected_base = move || memory.with(|m| m.bases.get(&base_key.get()).cloned());
    // Mode changes share the same lazy base lookup until this viewer is closed.
    type BaseFuture = opencode_tui_sync::runtime::Deferred<Option<Json>>;
    let bases: Rc<RefCell<HashMap<String, BaseFuture>>> = Rc::default();
    let (reported_bases, set_reported_bases) = create_signal_with(HashMap::<String, Option<Json>>::new(), None);
    let load_base: Rc<dyn Fn(Json, String) -> BaseFuture> = {
        let bases = bases.clone();
        let client = client.clone();
        Rc::new(move |location: Json, key: String| {
            if let Some(cached) = bases.borrow().get(&key) {
                return cached.clone();
            }
            let call = client.api().call_named("vcs.base", &opencode_tui_sync::obj! { "location" => location }, RequestOptions::default());
            let key_for = key.clone();
            let pending = BaseFuture::spawn(async move {
                let data = call.await.ok().map(|r| r.json()).and_then(|r| r.get("data").cloned()).filter(|d| !matches!(d, Json::Null));
                set_reported_bases.update(|known| {
                    known.insert(key_for, data.clone());
                });
                data
            });
            bases.borrow_mut().insert(key, pending.clone());
            pending
        })
    };

    let (resource, set_resource) = create_signal_with(Resource { loading: true, error: false, value: None }, None);
    let (refetch_tick, set_refetch_tick) = create_signal(0u64);
    let generation = Rc::new(Cell::new(0u64));
    {
        let session_id = session_id.clone();
        let client = client.clone();
        let load_base = load_base.clone();
        let generation = generation.clone();
        create_effect(move || {
            refetch_tick.get();
            let current = mode.get();
            let location = location.get();
            let key = base_key.get();
            let selected = if current == DiffMode::Turn { None } else { selected_base() };
            let id = generation.get() + 1;
            generation.set(id);
            set_resource.update(|r| {
                r.loading = true;
                r.error = false;
            });
            let session_id = session_id.clone();
            let client = client.clone();
            let load_base = load_base.clone();
            let generation = generation.clone();
            tokio::task::spawn_local(async move {
                let result: Result<DiffResult, ()> = async {
                    if current == DiffMode::Turn {
                        if let Some(id) = &session_id {
                            let response = client
                                .api()
                                .call_named(
                                    "session.diff",
                                    &opencode_tui_sync::obj! { "sessionID" => id.as_str(), "context" => VCS_DIFF_CONTEXT_LINES },
                                    RequestOptions::default(),
                                )
                                .await
                                .map_err(|_| ())?
                                .json();
                            let files = match &response {
                                Json::Array(_) => normalize_diffs(Some(&response)),
                                other => normalize_diffs(other.get("data")),
                            };
                            return Ok(DiffResult { base: None, files });
                        }
                    }
                    let vcs = if current == DiffMode::Turn { DiffMode::Branch } else { current };
                    let base = if !vcs.needs_base() {
                        None
                    } else if let Some(selected) = selected.clone() {
                        Some((selected.clone(), selected))
                    } else {
                        load_base(location.clone(), key.clone())
                            .wait()
                            .await
                            .map(|b| (b.str("name").unwrap_or("").to_string(), b.str("ref").unwrap_or("").to_string()))
                    };
                    if generation.get() != id || (vcs == DiffMode::Committed && base.is_none()) {
                        return Ok(DiffResult { base: None, files: Vec::new() });
                    }
                    let mut input = opencode_tui_sync::obj! {
                        "location" => location.clone(),
                        "mode" => vcs.as_str(),
                        "context" => VCS_DIFF_CONTEXT_LINES,
                    };
                    if let Some((_, reference)) = &base {
                        input.set("base", Json::String(reference.clone()));
                    }
                    let response = client.api().call_named("vcs.diff", &input, RequestOptions::default()).await.map_err(|_| ())?.json();
                    Ok(DiffResult { base, files: normalize_diffs(response.get("data")) })
                }
                .await;
                if generation.get() != id {
                    return;
                }
                set_resource.set(match result {
                    Ok(value) => Resource { loading: false, error: false, value: Some(value) },
                    Err(()) => Resource { loading: false, error: true, value: None },
                });
            });
        });
    }
    // Each completed turn replaces the last one.
    {
        let store = store.clone();
        let session_id = session_id.clone();
        let previous: Rc<Cell<Option<SessionStatus>>> = Rc::default();
        create_effect(move || {
            if mode.get() != DiffMode::Turn {
                previous.set(None);
                return;
            }
            let status = session_id.as_ref().map(|id| store.session.status(id));
            if previous.get() == Some(SessionStatus::Running) && status == Some(SessionStatus::Idle) {
                set_refetch_tick.update(|t| *t += 1);
            }
            previous.set(status);
        });
    }
    let source_base = move || -> Option<(String, String)> {
        match selected_base() {
            Some(reference) => Some((reference.clone(), reference)),
            None => reported_bases.with(|r| {
                r.get(&base_key.get())
                    .cloned()
                    .flatten()
                    .map(|b| (b.str("name").unwrap_or("").to_string(), b.str("ref").unwrap_or("").to_string()))
            }),
        }
    };
    let result = create_memo(move || resource.with(|r| if r.error || r.loading { None } else { r.value.clone() }));
    let source_detail = create_memo(move || -> Option<String> {
        let error = resource.with(|r| r.error);
        match mode.get() {
            DiffMode::Turn => error.then(|| "Diff unavailable".into()),
            DiffMode::Working => Some("vs HEAD".into()),
            _ if error => Some("Base or diff unavailable".into()),
            _ => match result.get() {
                None => Some("Resolving diff…".into()),
                Some(r) => Some(match r.base {
                    None => "Base not reported".into(),
                    Some((name, _)) => format!("vs {name}"),
                }),
            },
        }
    });
    let on_preferences_change: Rc<dyn Fn(Preferences)> = {
        let config = config.clone();
        Rc::new(move |value: Preferences| {
            let _ = config.update(|draft| {
                let mut diffs = match draft.get("diffs") {
                    Some(Json::Object(o)) => o.clone(),
                    _ => Default::default(),
                };
                if let Some(tree) = value.tree {
                    diffs.insert("tree".into(), Json::Bool(tree));
                }
                if let Some(single) = value.single {
                    diffs.insert("single".into(), Json::Bool(single));
                }
                if let Some(view) = value.view {
                    diffs.insert("view".into(), Json::String(view.into()));
                }
                if let Some(fullscreen) = value.fullscreen {
                    diffs.insert("fullscreen".into(), Json::Bool(fullscreen));
                }
                draft.set("diffs", Json::Object(diffs));
            });
        })
    };
    let prefs = config.data_untracked().diffs.clone().unwrap_or_default();
    component(move || {
        diff_viewer_content(ContentProps {
            files: Rc::new(move || result.with(|r| r.as_ref().map(|r| r.files.clone()).unwrap_or_default())),
            loading: Rc::new(move || resource.with(|r| r.loading)),
            error: Rc::new(move || resource.with(|r| r.error)),
            mode: Rc::new(move || mode.get()),
            sources: sources.clone(),
            source_detail: Rc::new(move || source_detail.get()),
            source_base: Rc::new(source_base),
            unavailable: Rc::new(move || mode.get() == DiffMode::Committed && result.with(|r| r.as_ref().is_some_and(|r| r.base.is_none()))),
            preferences: Preferences {
                tree: prefs.tree,
                single: prefs.single,
                view: prefs.view.clone().and_then(|v| if v == "split" { Some("split") } else if v == "unified" { Some("unified") } else { None }),
                fullscreen: prefs.fullscreen,
            },
            panel: panel.clone(),
            on_preferences_change: on_preferences_change.clone(),
            on_close: on_close.clone(),
            on_switch_source: Rc::new(move |next: DiffMode| {
                memory.update(|m| m.source = Some(next));
                set_mode.set(next);
            }),
            load_image: {
                let client = client.clone();
                let location = location.clone();
                Rc::new(move |file: String| {
                    let location = location.get();
                    client.api().call_named(
                        "file.read",
                        &opencode_tui_sync::obj! { "path" => file.as_str(), "location" => location },
                        RequestOptions::default(),
                    )
                })
            },
        })
    })
}

/// `DiffPreferences`.
#[derive(Clone, Default)]
pub struct Preferences {
    pub tree: Option<bool>,
    pub single: Option<bool>,
    pub view: Option<&'static str>,
    pub fullscreen: Option<bool>,
}

struct ContentProps {
    files: Rc<dyn Fn() -> Vec<DiffFile>>,
    loading: Rc<dyn Fn() -> bool>,
    error: Rc<dyn Fn() -> bool>,
    mode: Rc<dyn Fn() -> DiffMode>,
    sources: Rc<Vec<DiffMode>>,
    source_detail: Rc<dyn Fn() -> Option<String>>,
    source_base: Rc<dyn Fn() -> Option<(String, String)>>,
    unavailable: Rc<dyn Fn() -> bool>,
    preferences: Preferences,
    panel: Option<PanelInput>,
    on_preferences_change: Rc<dyn Fn(Preferences)>,
    on_close: Rc<dyn Fn()>,
    on_switch_source: Rc<dyn Fn(DiffMode)>,
    /// `file.read` for an image preview, with the viewer's location.
    load_image: Rc<dyn Fn(String) -> opencode_tui_sync::client::Call>,
}

fn empty_border(f: impl FnOnce(&mut BorderCharacters)) -> [u32; 11] {
    let mut chars = BorderCharacters {
        top_left: "",
        top_right: "",
        bottom_left: "",
        bottom_right: "",
        horizontal: " ",
        vertical: "",
        top_t: "",
        bottom_t: "",
        left_t: "",
        right_t: "",
        cross: "",
    };
    f(&mut chars);
    border_chars_to_array(&chars)
}

/// `<CardGap glyph card surface inset>`: a half-row of base background between cards.
fn card_gap(glyph: &'static str, card: Rc<dyn Fn() -> RGBA>, surface: Rc<dyn Fn() -> RGBA>, inset: Rc<dyn Fn() -> i64>) -> View {
    let theme = tui().theme;
    let half = move |color: Rc<dyn Fn() -> RGBA>, width: Option<Rc<dyn Fn() -> i64>>| {
        let el = box_();
        el.set_height(Some(Value::Point(1.0)));
        el.set_flex_shrink(Some(0.0));
        match width {
            None => el.set_flex_grow(Some(1.0)),
            Some(width) => bind(move || width(), move |w| el.set_width(Some(Value::Point(w as f32)))),
        }
        el.set_border(BorderProp::Sides(vec![if glyph == "▄" { Side::Top } else { Side::Bottom }]));
        bind_always(move || color(), move |c| el.set_border_color(Some(c)));
        bind_always(move || rgba(theme.with(|t| t.background.base)), move |c| el.set_background_color(Some(c)));
        el.set_custom_border_chars(Some(empty_border(|c| c.horizontal = glyph)));
        View::Node(el)
    };
    let row = box_();
    row.set_height(Some(Value::Point(1.0)));
    row.set_flex_shrink(Some(0.0));
    row.set_flex_direction(FlexDirection::Row);
    // A zero-width inset still claims a cell, which would notch flush cards.
    {
        let (inset, surface) = (inset.clone(), surface.clone());
        let half = half.clone();
        let width = inset.clone();
        insert(row, show(move || inset() > 0, move || half(surface.clone(), Some(width.clone())), None), At::End);
    }
    insert(row, half(card, None), At::End);
    {
        let width = inset.clone();
        insert(row, show(move || inset() > 0, move || half(surface.clone(), Some(width.clone())), None), At::End);
    }
    View::Node(row)
}

/// `<DiffViewerContent>`.
fn diff_viewer_content(props: ContentProps) -> View {
    let t = tui();
    let ui = ui_memory();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let config = t.config_context.clone();
    let shortcuts = use_shortcuts();
    let files = props.files.clone();
    let panel = props.panel.clone();
    let docked = {
        let panel = panel.clone();
        Rc::new(move || panel.as_ref().is_some_and(|p| (p.docked)()))
    };
    let width = {
        let panel = panel.clone();
        Rc::new(move || panel.as_ref().map(|p| (p.width)()).unwrap_or_else(|| dimensions.get().0 as i64))
    };
    // Docked panels and narrow full screens share the compact layout: no file tree, flush cards, and a footer.
    let compact = {
        let (docked, width) = (docked.clone(), width.clone());
        create_memo(move || docked() || width() < FULL_LAYOUT_MIN_WIDTH)
    };
    // Full screen has no pane beside it to focus, and is always the active input scope.
    let focused = {
        let (docked, panel) = (docked.clone(), panel.clone());
        Rc::new(move || !docked() || panel.as_ref().is_some_and(|p| (p.focused)()))
    };
    // The side panel sits on the slot's raised surface; full screen keeps the base background.
    let surface: Rc<dyn Fn() -> RGBA> = {
        let docked = docked.clone();
        Rc::new(move || theme.with(|t| if docked() { rgba(t.background.raised.base) } else { rgba(t.background.base) }))
    };
    // Cards are inset from the pane edges; the gaps between them run edge to edge.
    let inset: Rc<dyn Fn() -> i64> = Rc::new(move || if compact.get() { 0 } else { FULL_INSET });
    let mode = props.mode.clone();
    let (file_tree_enabled, set_file_tree_enabled) =
        create_signal(ui.with_untracked(|m| m.tree).or(props.preferences.tree).unwrap_or(true));
    let show_file_tree = {
        let files = files.clone();
        create_memo(move || !compact.get() && show_diff_viewer_file_tree(file_tree_enabled.get(), files().len()))
    };
    let (single_patch, set_single_patch) =
        create_signal(ui.with_untracked(|m| m.single).or(props.preferences.single).unwrap_or(false));
    let file_tree_width = {
        let width = width.clone();
        create_memo(move || FILE_TREE_MIN_WIDTH.max(FILE_TREE_MAX_WIDTH.min(width() / 4)))
    };
    let patch_pane_width = {
        let (width, inset) = (width.clone(), inset.clone());
        create_memo(move || width() - if show_file_tree.get() { file_tree_width.get() } else { 0 } - inset() * 2)
    };
    let split_available = create_memo(move || patch_pane_width.get() >= MIN_SPLIT_WIDTH);
    let stored_view = props.preferences.view;
    let (view_override, set_view_override) = create_signal(
        ui.with_untracked(|m| m.view.clone()).and_then(|v| if v == "split" { Some("split") } else if v == "unified" { Some("unified") } else { None }).or(stored_view),
    );
    let view = create_memo(move || {
        if !split_available.get() {
            return "unified";
        }
        view_override.get().or(stored_view).unwrap_or("split")
    });
    let file_tree_model = {
        let files = files.clone();
        create_memo(move || build_file_tree(&files().iter().map(|f| FileTreeItem { file: f.file.clone(), status: Some(f.status.clone()) }).collect::<Vec<_>>()))
    };
    let (expanded_file_nodes, set_expanded_file_nodes) = create_signal_with(BTreeSet::<usize>::new(), None);
    let (selected_file_index, set_selected_file_index) = create_signal(None::<usize>);
    // A source switch after marking files lands on the last file (the reference's scroll-derived current file).
    let select_last_on_change = Rc::new(std::cell::Cell::new(false));
    let (reviewed_file_names, set_reviewed_file_names) =
        create_signal_with(ui.with_untracked(|m| m.reviewed.clone()), None);
    let (exhausted_file_index, set_exhausted_file_index) = create_signal(None::<usize>);
    let patch_scroll_acceleration = {
        let config = config.clone();
        create_memo_equals(move || get_scroll_acceleration(&config.data()), None)
    };
    let patch_file_indexes = create_memo(move || ordered_patch_file_indexes(&flatten_file_tree(&file_tree_model.get(), None)));
    let help_shortcut = {
        let shortcuts = shortcuts.clone();
        Rc::new(move || shortcuts.get("diff.help"))
    };
    let scroll: Rc<Cell<Option<Element>>> = Rc::default();
    let patch_node_by_file_index: Rc<RefCell<HashMap<usize, Element>>> = Rc::default();
    let patch_diff_by_file_index: Rc<RefCell<HashMap<usize, PatchDiffRef>>> = Rc::default();
    #[derive(Clone, Copy, PartialEq)]
    struct SelectedHunk {
        file_index: usize,
        hunk_index: usize,
        scroll_top: f64,
    }
    let (selected_hunk, set_selected_hunk) = create_signal(None::<SelectedHunk>);
    let (pending_patch_scroll, set_pending_patch_scroll) = create_signal(None::<usize>);

    {
        let restored = std::cell::Cell::new(false);
        let file_tree_model = file_tree_model.clone();
        let files = files.clone();
        let select_last_on_change = select_last_on_change.clone();
        create_effect(move || {
            let tree = file_tree_model.get();
            // Wait for the first page: an empty tree is not the viewer's state yet.
            if files().is_empty() {
                return;
            }
            // The first run mounts this component: a move between the docked panel and full screen keeps its state.
            if !restored.replace(true) {
                let selected = ui.with_untracked(|m| m.selected.clone());
                if let Some(name) = selected {
                    let index = files().iter().position(|file| file.file == name);
                    set_selected_file_index.set(index);
                }
                set_expanded_file_nodes.set(all_expanded_directories(&tree));
                return;
            }
            batch(|| {
                set_expanded_file_nodes.set(all_expanded_directories(&tree));
                set_selected_file_index.set(if select_last_on_change.replace(false) {
                    files().len().checked_sub(1)
                } else {
                    None
                });
                set_selected_hunk.set(None);
                set_reviewed_file_names.set(BTreeSet::new());
                set_exhausted_file_index.set(None);
            });
        });
    }
    {
        let files = files.clone();
        create_effect(move || {
            let Some(index) = selected_file_index.get() else { return };
            let Some(name) = files().get(index).map(|file| file.file.clone()) else { return };
            ui.update(|m| m.selected = Some(name));
        });
    }
    let clear_patch_selection: Rc<dyn Fn()> = Rc::new(move || {
        set_pending_patch_scroll.set(None);
        set_selected_hunk.set(None);
        if !single_patch.get_untracked() {
            set_selected_file_index.set(None);
        }
    });
    let scroll_page: Rc<dyn Fn(f64, f64)> = {
        let (clear, scroll) = (clear_patch_selection.clone(), scroll.clone());
        Rc::new(move |direction: f64, divisor: f64| {
            clear();
            if let Some(scroll) = scroll.get() {
                scroll.scroll_by(direction * 1f64.max((scroll.scroll_viewport().height / divisor).floor()), ScrollUnit::Absolute);
            }
        })
    };
    let scroll_patch_node_to_top: Rc<dyn Fn(Element, Option<f64>)> = {
        let scroll = scroll.clone();
        Rc::new(move |node: Element, offset: Option<f64>| {
            let Some(scroll) = scroll.get() else { return };
            if !node.is_alive() {
                return;
            }
            let content_y = node.y() as f64 - scroll.scroll_content().y;
            // The fixed pane edge replaces the leading separator when jumping to a later file.
            scroll.scroll_to(content_y + offset.unwrap_or(if content_y > 0.0 { 1.0 } else { 0.0 }));
        })
    };
    let reveal_file_tree_file = move |file_index: usize| {
        let Some(expanded) = file_tree_file_selection(&file_tree_model.get_untracked(), file_index) else { return };
        set_expanded_file_nodes.update(|nodes| {
            for node in expanded {
                nodes.insert(node);
            }
        });
    };
    let select_patch_file: Rc<dyn Fn(usize)> = Rc::new(move |file_index: usize| {
        set_pending_patch_scroll.set(None);
        set_exhausted_file_index.set(None);
        reveal_file_tree_file(file_index);
        set_selected_file_index.set(Some(file_index));
    });
    let scroll_single_patch_to_top: Rc<dyn Fn()> = {
        let scroll = scroll.clone();
        Rc::new(move || {
            let scroll = scroll.clone();
            renderer::on_frame(
                move || {
                    if let Some(s) = scroll.get() {
                        s.scroll_to(0.0);
                    }
                    let scroll = scroll.clone();
                    renderer::on_frame(
                        move || {
                            if let Some(s) = scroll.get() {
                                s.scroll_to(0.0);
                            }
                        },
                        true,
                    );
                    renderer::request_render();
                },
                true,
            );
            renderer::request_render();
        })
    };
    let scroll_to_file_index: Rc<dyn Fn(Option<usize>)> = {
        let (select, nodes, to_top) = (select_patch_file.clone(), patch_node_by_file_index.clone(), scroll_patch_node_to_top.clone());
        Rc::new(move |file_index: Option<usize>| {
            let Some(file_index) = file_index else { return };
            select(file_index);
            let node = nodes.borrow().get(&file_index).copied();
            if let Some(node) = node {
                to_top(node, None);
            }
        })
    };
    let jump_to_file_index: Rc<dyn Fn(Option<usize>)> = {
        let (scroll_to, single_top) = (scroll_to_file_index.clone(), scroll_single_patch_to_top.clone());
        Rc::new(move |file_index: Option<usize>| {
            if file_index.is_none() {
                return;
            }
            set_selected_hunk.set(None);
            scroll_to(file_index);
            if single_patch.get_untracked() {
                single_top();
            }
        })
    };
    let current_patch_file_index: Rc<dyn Fn() -> Option<usize>> = {
        let (scroll, nodes) = (scroll.clone(), patch_node_by_file_index.clone());
        Rc::new(move || {
            let scroll = scroll.get()?;
            let top = scroll.scroll_top();
            let viewport_y = scroll.scroll_viewport().y;
            let viewport_content_y = top + 1.0;
            let mut entries: Vec<(usize, f64)> = patch_file_indexes
                .get_untracked()
                .into_iter()
                .filter_map(|i| nodes.borrow().get(&i).map(|n| (i, top + n.y() as f64 - viewport_y)))
                .collect();
            entries.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            entries.iter().rev().find(|e| e.1 <= viewport_content_y).map(|e| e.0).or_else(|| entries.first().map(|e| e.0))
        })
    };
    let first_patch_file_index = move || patch_file_indexes.with_untracked(|p| p.first().copied());
    let visible_patch_files = {
        let files = files.clone();
        let current = current_patch_file_index.clone();
        create_memo(move || {
            let files = files();
            if !single_patch.get() {
                return patch_file_indexes.get().into_iter().filter_map(|i| files.get(i).map(|f| (i, f.clone()))).collect::<Vec<_>>();
            }
            let index = single_patch_file_index(selected_file_index.get(), current(), patch_file_indexes.with(|p| p.first().copied()));
            index.and_then(|i| files.get(i).map(|f| (i, f.clone()))).into_iter().collect()
        })
    };
    let jump_relative_patch_file: Rc<dyn Fn(i64)> = {
        let (current, select, single_top, scroll_to) =
            (current_patch_file_index.clone(), select_patch_file.clone(), scroll_single_patch_to_top.clone(), scroll_to_file_index.clone());
        Rc::new(move |offset: i64| {
            set_selected_hunk.set(None);
            let next = move_patch_file_index(&patch_file_indexes.get_untracked(), selected_file_index.get_untracked().or_else(|| current()), offset);
            if single_patch.get_untracked() {
                let Some(next) = next else { return };
                select(next);
                single_top();
                return;
            }
            scroll_to(next);
        })
    };
    let jump_relative_hunk: Rc<dyn Fn(i64)> = {
        let (scroll, diffs, select) = (scroll.clone(), patch_diff_by_file_index.clone(), select_patch_file.clone());
        Rc::new(move |offset: i64| {
            let Some(patch_scroll) = scroll.get() else { return };
            let top = patch_scroll.scroll_top();
            let viewport_y = patch_scroll.scroll_viewport().y;
            let mut hunks: Vec<(usize, usize, f64)> = visible_patch_files
                .get_untracked()
                .iter()
                .flat_map(|(file_index, _)| {
                    diffs
                        .borrow()
                        .get(file_index)
                        .map(|d| d.hunks())
                        .unwrap_or_default()
                        .into_iter()
                        .enumerate()
                        .map(|(hunk_index, node)| (*file_index, hunk_index, top + node.y() as f64 - viewport_y - if hunk_index > 0 { 1.0 } else { 0.0 }))
                        .collect::<Vec<_>>()
                })
                .collect();
            hunks.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
            let selected = selected_hunk.get_untracked();
            let selected_index = selected
                .filter(|s| s.scroll_top == top)
                .and_then(|s| hunks.iter().position(|h| h.0 == s.file_index && h.1 == s.hunk_index));
            let content_top = top + FILE_HEADER_HEIGHT;
            let next = match selected_index {
                Some(i) => {
                    let j = i as i64 + offset;
                    if j < 0 { None } else { hunks.get(j as usize).copied() }
                }
                None => {
                    if offset == 1 {
                        hunks.iter().find(|h| h.2 > content_top).copied()
                    } else {
                        hunks.iter().rev().find(|h| h.2 < content_top).copied()
                    }
                }
            };
            let Some(next) = next else { return };
            select(next.0);
            patch_scroll.scroll_to(0f64.max(next.2 - FILE_HEADER_HEIGHT));
            set_selected_hunk.set(Some(SelectedHunk { file_index: next.0, hunk_index: next.1, scroll_top: patch_scroll.scroll_top() }));
        })
    };
    let ensure_highlighted_patch_file = {
        let (current, select) = (current_patch_file_index.clone(), select_patch_file.clone());
        move || {
            let Some(index) = current().or_else(|| selected_file_index.get_untracked()).or_else(first_patch_file_index) else { return };
            select(index);
        }
    };
    let scroll_to_patch_file_index_after_render: Rc<dyn Fn(usize, Option<f64>)> = {
        let (nodes, to_top) = (patch_node_by_file_index.clone(), scroll_patch_node_to_top.clone());
        Rc::new(move |file_index: usize, offset: Option<f64>| {
            set_pending_patch_scroll.set(Some(file_index));
            let (nodes, to_top) = (nodes.clone(), to_top.clone());
            renderer::on_frame(
                move || {
                    if pending_patch_scroll.get_untracked() != Some(file_index) {
                        return;
                    }
                    let node = nodes.borrow().get(&file_index).copied();
                    if let Some(node) = node {
                        to_top(node, offset);
                    }
                    let (nodes, to_top) = (nodes.clone(), to_top.clone());
                    renderer::on_frame(
                        move || {
                            if pending_patch_scroll.get_untracked() != Some(file_index) {
                                return;
                            }
                            let node = nodes.borrow().get(&file_index).copied();
                            if let Some(node) = node {
                                to_top(node, offset);
                            }
                            set_pending_patch_scroll.set(None);
                        },
                        true,
                    );
                    renderer::request_render();
                },
                true,
            );
            renderer::request_render();
        })
    };
    let register_patch_node: Rc<dyn Fn(usize, Element)> = {
        let (nodes, after) = (patch_node_by_file_index.clone(), scroll_to_patch_file_index_after_render.clone());
        Rc::new(move |file_index: usize, element: Element| {
            nodes.borrow_mut().insert(file_index, element);
            if pending_patch_scroll.get_untracked() != Some(file_index) {
                return;
            }
            after(file_index, None);
        })
    };
    let click_file_tree_row: Rc<dyn Fn(&FileTreeRow)> = {
        let jump = jump_to_file_index.clone();
        Rc::new(move |row: &FileTreeRow| {
            if row.file_index.is_some() {
                jump(row.file_index);
                return;
            }
            let id = row.id;
            set_expanded_file_nodes.update(|expanded| *expanded = toggle_directory(&file_tree_model.get_untracked(), expanded, Some(id)));
        })
    };
    let toggle_file_reviewed: Rc<dyn Fn(Option<usize>)> = {
        let files = files.clone();
        let (current, nodes, scroll, after, select) = (
            current_patch_file_index.clone(),
            patch_node_by_file_index.clone(),
            scroll.clone(),
            scroll_to_patch_file_index_after_render.clone(),
            select_patch_file.clone(),
        );
        Rc::new(move |file_index: Option<usize>| {
            let Some(file_index) = file_index else { return };
            // A memory write below can start a refetch (the resource tracks the review memory), so keep the files.
            let snapshot = files();
            let Some(file) = snapshot.get(file_index).map(|f| f.file.clone()) else { return };
            let current_index = selected_file_index.get_untracked().or_else(|| current());
            let anchor = current_index.and_then(|c| nodes.borrow().get(&c).copied());
            let offset = match (anchor, scroll.get()) {
                (Some(anchor), Some(scroll)) => Some(scroll.scroll_viewport().y - anchor.y() as f64),
                _ => None,
            };
            let reviewed = reviewed_file_names.with_untracked(|r| r.contains(&file));
            set_reviewed_file_names.update(|names| {
                if !names.remove(&file) {
                    names.insert(file.clone());
                }
            });
            let reviewed_now = reviewed_file_names.get_untracked();
            ui.update(|m| m.reviewed = reviewed_now.clone());
            // Completing another file from its menu must not navigate away from the current file.
            if Some(file_index) != current_index {
                if let (Some(current), Some(offset)) = (current_index, offset) {
                    if !single_patch.get_untracked() {
                        after(current, Some(offset));
                    }
                }
                return;
            }
            let indexes = {
                let shared = patch_file_indexes.get_untracked();
                if shared.is_empty() { patch_indexes_of(&snapshot) } else { shared }
            };
            let next = if single_patch.get_untracked() && !reviewed {
                move_patch_file_index(&indexes, Some(file_index), 1).unwrap_or(file_index)
            } else {
                file_index
            };
            let selected_name = snapshot.get(next).map(|f| f.file.clone());
            ui.update(|m| m.selected = selected_name);
            select(next);
            if single_patch.get_untracked() && !reviewed && next == file_index {
                set_exhausted_file_index.set(Some(file_index));
            }
            set_selected_hunk.set(None);
            after(next, None);
        })
    };
    let close: Rc<dyn Fn()> = {
        let on_close = props.on_close.clone();
        Rc::new(move || {
            // TODO(port-tui): `dialog.clear()` (the dialogs slice).
            on_close();
        })
    };
    let dialog = crate::ui::dialog::use_dialog();
    let sources_for_dialog = props.sources.clone();
    let base_for_dialog = props.source_base.clone();
    let switch_for_dialog = props.on_switch_source.clone();
    let mode_for_dialog = mode.clone();
    let open_switch_diff_dialog: Rc<dyn Fn()> = {
        let dialog = dialog.clone();
        Rc::new(move || {
            let dialog = dialog.clone();
            let sources = sources_for_dialog.clone();
            let base = base_for_dialog.clone();
            let switch = switch_for_dialog.clone();
            let mode = mode_for_dialog.clone();
            let select_last = select_last_on_change.clone();
            let inner_dialog = dialog.clone();
            dialog.show(move || {
                let select_last_on_change = select_last.clone();
                let dialog = inner_dialog.clone();
                let sources = sources.clone();
                let base = base.clone();
                let switch = switch.clone();
                let mode = mode.clone();
                let mut props = crate::ui::dialog_select::SelectProps::new("Diff source", move || {
                    let mut options: Vec<crate::ui::dialog_select::SelectOption<SourceChoice>> = sources
                        .iter()
                        .map(|source| {
                            let (label, description) = source.source();
                            let mut option = crate::ui::dialog_select::SelectOption::new(label, SourceChoice::Mode(*source));
                            option.description = Some(description.into());
                            option.title_view = Some({
                                let padded = format!("{label:<11}");
                                Rc::new(move || View::from(padded.clone()))
                            });
                            option
                        })
                        .collect();
                    let name = base().map(|(name, _)| name).unwrap_or_else(|| "Choose…".into());
                    let mut option = crate::ui::dialog_select::SelectOption::new("Base", SourceChoice::Base);
                    option.description = Some(name);
                    option.title_view = Some(Rc::new(|| View::from("Base       ")));
                    options.push(option);
                    options
                });
                props.render_filter = false;
                props.skip_filter = true;
                props.current = Some({
                    let mode = mode.clone();
                    Rc::new(move || Some(SourceChoice::Mode(mode())))
                });
                props.on_select = Some({
                    let dialog = dialog.clone();
                    let switch = switch.clone();
                    Rc::new(move |option: &crate::ui::dialog_select::SelectOption<SourceChoice>| {
                        let SourceChoice::Mode(source) = option.value else { return };
                        dialog.clear();
                        if source == DiffMode::Turn {
                            select_last_on_change.set(true);
                        }
                        switch(source);
                    })
                });
                crate::ui::dialog_select::dialog_select(props)
            });
        })
    };
    let open_help_dialog: Rc<dyn Fn()> = {
        let dialog = dialog.clone();
        let shortcuts = shortcuts.clone();
        let single_patch = single_patch.clone();
        let compact = compact.clone();
        let has_panel = panel.is_some();
        Rc::new(move || {
            let dialog_for_close = dialog.clone();
            let shortcuts = shortcuts.clone();
            let single = single_patch.get_untracked();
            let tree = !compact.get_untracked();
            dialog.show(move || diff_help_dialog(dialog_for_close.clone(), shortcuts.clone(), single, tree, has_panel));
            dialog.set_size(crate::ui::dialog::DialogSize::Medium);
            dialog.set_centered(true);
        })
    };

    {
        let (close, clear, scroll, scroll_page) = (close.clone(), clear_patch_selection.clone(), scroll.clone(), scroll_page.clone());
        let (jump_hunk, jump_file, toggle_reviewed, current) =
            (jump_relative_hunk.clone(), jump_relative_patch_file.clone(), toggle_file_reviewed.clone(), current_patch_file_index.clone());
        let (select, single_top, after, ensure) =
            (select_patch_file.clone(), scroll_single_patch_to_top.clone(), scroll_to_patch_file_index_after_render.clone(), Rc::new(ensure_highlighted_patch_file));
        let (open_switch, open_help, on_prefs, panel) =
            (open_switch_diff_dialog.clone(), open_help_dialog.clone(), props.on_preferences_change.clone(), panel.clone());
        let files_for_single = files.clone();
        create_layer(move || {
            let c = |id: &str, title: &str, run: Rc<dyn Fn()>| Command::new(id, move || run()).title(title).group("VCS");
            let scroll_by = |delta: f64| {
                let (clear, scroll) = (clear.clone(), scroll.clone());
                Rc::new(move || {
                    clear();
                    if let Some(s) = scroll.get() {
                        s.scroll_by(delta, ScrollUnit::Absolute);
                    }
                }) as Rc<dyn Fn()>
            };
            let page = |direction: f64, divisor: f64| {
                let scroll_page = scroll_page.clone();
                Rc::new(move || scroll_page(direction, divisor)) as Rc<dyn Fn()>
            };
            let first = {
                let (clear, scroll) = (clear.clone(), scroll.clone());
                Rc::new(move || {
                    clear();
                    if let Some(s) = scroll.get() {
                        s.scroll_to(0.0);
                    }
                }) as Rc<dyn Fn()>
            };
            let last = {
                let (clear, scroll) = (clear.clone(), scroll.clone());
                Rc::new(move || {
                    clear();
                    if let Some(s) = scroll.get() {
                        s.scroll_to(s.scroll_height());
                    }
                }) as Rc<dyn Fn()>
            };
            let mark = {
                let (toggle_reviewed, current) = (toggle_reviewed.clone(), current.clone());
                Rc::new(move || {
                    let index = selected_file_index.get_untracked().or_else(|| current());
                    if index.is_some() && index == exhausted_file_index.get_untracked() {
                        return;
                    }
                    toggle_reviewed(index);
                }) as Rc<dyn Fn()>
            };
            let tree = {
                let on_prefs = on_prefs.clone();
                Rc::new(move || {
                    if compact.get_untracked() {
                        return;
                    }
                    let next = !file_tree_enabled.get_untracked();
                    set_file_tree_enabled.set(next);
                    ui.update(|m| m.tree = Some(next));
                    on_prefs(Preferences { tree: Some(next), ..Default::default() });
                }) as Rc<dyn Fn()>
            };
            let files_for_single = files_for_single.clone();
            let single = {
                let (ensure, on_prefs, single_top, select, after, current) =
                    (ensure.clone(), on_prefs.clone(), single_top.clone(), select.clone(), after.clone(), current.clone());
                Rc::new(move || {
                    set_selected_hunk.set(None);
                    if !single_patch.get_untracked() {
                        ensure();
                        set_single_patch.set(true);
                        ui.update(|m| m.single = Some(true));
                        // Without a selected file, single-patch mode shows the first diff.
                        if selected_file_index.get_untracked().is_none() {
                            let snapshot = files_for_single();
                            let shared = patch_file_indexes.get_untracked();
                            let indexes = if shared.is_empty() { patch_indexes_of(&snapshot) } else { shared };
                            if let Some(index) = indexes.first().copied() {
                                select(index);
                            }
                        }
                        on_prefs(Preferences { single: Some(true), ..Default::default() });
                        single_top();
                        return;
                    }
                    let index = visible_patch_files.with_untracked(|v| v.first().map(|(i, _)| *i)).or_else(|| {
                        single_patch_file_index(selected_file_index.get_untracked(), current(), first_patch_file_index())
                    });
                    if let Some(index) = index {
                        select(index);
                    }
                    set_single_patch.set(false);
                    ui.update(|m| m.single = Some(false));
                    on_prefs(Preferences { single: Some(false), ..Default::default() });
                    if let Some(index) = index {
                        after(index, None);
                    }
                }) as Rc<dyn Fn()>
            };
            let toggle_view = {
                let on_prefs = on_prefs.clone();
                Rc::new(move || {
                    if !split_available.get_untracked() {
                        return;
                    }
                    set_selected_hunk.set(None);
                    let next = if view.get_untracked() == "split" { "unified" } else { "split" };
                    set_view_override.set(Some(next));
                    ui.update(|m| m.view = Some(next.to_string()));
                    on_prefs(Preferences { view: Some(next), ..Default::default() });
                }) as Rc<dyn Fn()>
            };
            let fullscreen = {
                let (panel, on_prefs) = (panel.clone(), on_prefs.clone());
                Rc::new(move || {
                    let Some(panel) = &panel else { return };
                    let before = (panel.docked)();
                    (panel.toggle_fullscreen)();
                    // A narrow terminal forces full screen and ignores the toggle; only a real toggle is remembered.
                    if (panel.docked)() == before {
                        return;
                    }
                    on_prefs(Preferences { fullscreen: Some(!(panel.docked)()), ..Default::default() });
                }) as Rc<dyn Fn()>
            };
            let has_panel = panel.is_some();
            Layer {
                commands: vec![
                    c("diff.close", "Close diff viewer", close.clone()),
                    c("diff.down", "Move diff viewer down", scroll_by(1.0)),
                    c("diff.up", "Move diff viewer up", scroll_by(-1.0)),
                    c("diff.page.down", "Page diff viewer down", page(1.0, 1.0)),
                    c("diff.page.up", "Page diff viewer up", page(-1.0, 1.0)),
                    c("diff.half_page.down", "Scroll down half a page", page(1.0, 2.0)),
                    c("diff.half_page.up", "Scroll up half a page", page(-1.0, 2.0)),
                    c("diff.first", "Go to the start of the diff", first),
                    c("diff.last", "Go to the end of the diff", last),
                    {
                        let jump_hunk = jump_hunk.clone();
                        c("diff.next_hunk", "Jump to next diff hunk", Rc::new(move || jump_hunk(1)))
                    },
                    {
                        let jump_hunk = jump_hunk.clone();
                        c("diff.previous_hunk", "Jump to previous diff hunk", Rc::new(move || jump_hunk(-1)))
                    },
                    {
                        let jump_file = jump_file.clone();
                        c("diff.next_file", "Jump to next diff file", Rc::new(move || jump_file(1)))
                    },
                    {
                        let jump_file = jump_file.clone();
                        c("diff.previous_file", "Jump to previous diff file", Rc::new(move || jump_file(-1)))
                    },
                    c("diff.mark_reviewed", "Toggle selected diff file reviewed", mark),
                    c("diff.toggle_file_tree", "Toggle diff viewer file tree", tree),
                    c("diff.single_patch", "Toggle single patch view", single),
                    c("diff.switch_source", "Switch diff viewer source", open_switch.clone()),
                    c("diff.toggle_view", "Toggle diff viewer split or unified view", toggle_view),
                    c("diff.toggle_fullscreen", "Toggle diff viewer full screen", fullscreen).enabled(has_panel),
                    c("diff.help", "Show more diff viewer shortcuts", open_help.clone()),
                    // Specific diff bindings take precedence over app.exit's Ctrl+D binding.
                    c("app.exit", "Close diff viewer", close.clone()),
                ],
                ..Layer::default()
            }
        });
    }

    let help_shortcut_view = {
        let (help_shortcut, open_help) = (help_shortcut.clone(), open_help_dialog.clone());
        Rc::new(move |compact_label: bool, label: Option<&'static str>, muted: Rc<dyn Fn() -> bool>| {
            let (help_shortcut, open_help) = (help_shortcut.clone(), open_help.clone());
            let when_shortcut = help_shortcut.clone();
            show(
                move || when_shortcut().is_some(),
                move || {
                    let el = text();
                    el.set_id("diff-help-shortcut");
                    {
                        let muted = muted.clone();
                        bind_always(
                            move || theme.with(|t| if muted() { rgba(t.text.action.primary.disabled) } else { rgba(t.text.base) }),
                            move |c| el.set_fg(Some(c)),
                        );
                    }
                    {
                        let muted = muted.clone();
                        bind(move || muted(), move |m| el.set_attributes(Some(if m { text_attributes::DIM } else { 0 })));
                    }
                    el.set_selectable(false);
                    el.set_flex_shrink(Some(0.0));
                    el.set_wrap_mode(WrapMode::None);
                    {
                        let open_help = open_help.clone();
                        el.set_on_mouse(
                            Some(MouseEventType::Up),
                            Some(Rc::new(move |event| {
                                if event.button != 0 {
                                    return;
                                }
                                event.stop_propagation();
                                open_help();
                            })),
                        );
                    }
                    let help_shortcut = help_shortcut.clone();
                    insert(el, View::dynamic(move || View::from(if compact_label { "?".to_string() } else { help_shortcut().unwrap_or_default() })), At::End);
                    if !compact_label {
                        let s = span();
                        insert_node(el, s, None);
                        let muted = muted.clone();
                        bind_always(move || theme.with(|t| if muted() { None } else { Some(rgba(t.text.muted)) }), move |c| s.set_span_style(c, None, 0, 0));
                        insert(s, View::from(format!(" {}", label.unwrap_or("help"))), At::Only);
                    }
                    View::Node(el)
                },
                None,
            )
        })
    };

    let root = box_();
    root.set_width(Some(Value::Percent(100.0)));
    root.set_height(Some(Value::Percent(100.0)));
    {
        let surface = surface.clone();
        bind_always(move || surface(), move |c| root.set_background_color(Some(c)));
    }

    // The source header (when the file tree does not show it).
    {
        let (docked, mode, files, open_switch, detail) = (docked.clone(), mode.clone(), files.clone(), open_switch_diff_dialog.clone(), props.source_detail.clone());
        insert(
            root,
            show(
                move || !show_file_tree.get(),
                move || {
                    let header = box_();
                    header.set_id("diff-source-header");
                    header.set_padding(Edge::Left, Some(Value::Point(2.0)));
                    // Compact, the header lines up with the card text below it.
                    bind(move || if compact.get() { 1.0 } else { 2.0 }, move |p| header.set_padding(Edge::Right, Some(Value::Point(p))));
                    // Only the docked panel leaves a row above the header; full screen starts at the top edge.
                    {
                        let docked = docked.clone();
                        bind(move || docked(), move |d| {
                            header.set_padding(Edge::Top, Some(Value::Point(if d { 1.0 } else { 0.0 })));
                            header.set_height(Some(Value::Point(if d { 2.0 } else { 1.0 })));
                        });
                    }
                    header.set_flex_shrink(Some(0.0));
                    header.set_flex_direction(FlexDirection::Row);
                    header.set_gap(2, Some(Value::Point(1.0)));
                    let switch_box = box_();
                    insert_node(header, switch_box, None);
                    switch_box.set_id("diff-source-switch");
                    switch_box.set_flex_direction(FlexDirection::Row);
                    switch_box.set_flex_grow(Some(1.0));
                    switch_box.set_min_width(Some(Value::Point(0.0)));
                    {
                        let open_switch = open_switch.clone();
                        switch_box.set_on_mouse(
                            Some(MouseEventType::Up),
                            Some(Rc::new(move |event| {
                                if event.button != 0 {
                                    return;
                                }
                                event.stop_propagation();
                                open_switch();
                            })),
                        );
                    }
                    let label = text();
                    insert_node(switch_box, label, None);
                    bind_always(move || rgba(theme.with(|t| t.text.action.secondary.base)), move |c| label.set_fg(Some(c)));
                    label.set_attributes(Some(text_attributes::BOLD));
                    label.set_selectable(false);
                    label.set_flex_shrink(Some(0.0));
                    label.set_wrap_mode(WrapMode::None);
                    {
                        let mode = mode.clone();
                        insert(label, View::dynamic(move || View::from(mode().source().0)), At::Only);
                    }
                    {
                        let (when_detail, detail) = (detail.clone(), detail.clone());
                        insert(
                            switch_box,
                            show(
                                move || when_detail().is_some(),
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
                    count.set_flex_shrink(Some(0.0));
                    count.set_wrap_mode(WrapMode::None);
                    let files = files.clone();
                    insert(
                        count,
                        View::dynamic(move || {
                            let files = files();
                            let reviewed = reviewed_file_names.with(|r| files.iter().filter(|f| r.contains(&f.file)).count());
                            View::from(format!("{reviewed}/{}", files.len()))
                        }),
                        At::Only,
                    );
                    View::Node(header)
                },
                None,
            ),
            At::End,
        );
    }

    // The body.
    let body = box_();
    insert_node(root, body, None);
    body.set_flex_grow(Some(1.0));
    body.set_min_height(Some(Value::Point(0.0)));
    let message = move |fg: fn(&crate::theme::ResolvedTheme) -> crate::theme::Color, value: Rc<dyn Fn() -> String>| {
        let el = box_();
        el.set_flex_grow(Some(1.0));
        el.set_padding(Edge::All, Some(Value::Point(2.0)));
        bind(move || if compact.get() { 1.0 } else { 2.0 }, move |p| el.set_padding(Edge::Top, Some(Value::Point(p))));
        let label = text();
        insert_node(el, label, None);
        bind_always(move || rgba(theme.with(|t| fg(t))), move |c| label.set_fg(Some(c)));
        insert(label, View::dynamic(move || View::from(value())), At::Only);
        View::Node(el)
    };
    let content_view = {
        let files = files.clone();
        let (props_loading, props_error) = (props.loading.clone(), props.error.clone());
        let (detail, mode) = (props.source_detail.clone(), mode.clone());
        let (open_switch, click_row, help_view) = (open_switch_diff_dialog.clone(), click_file_tree_row.clone(), help_shortcut_view.clone());
        let (scroll, register, diffs, clear, inset, surface) =
            (scroll.clone(), register_patch_node.clone(), patch_diff_by_file_index.clone(), clear_patch_selection.clone(), inset.clone(), surface.clone());
        let nodes = patch_node_by_file_index.clone();
        move || {
            let row = box_();
            row.set_flex_direction(FlexDirection::Row);
            row.set_flex_grow(Some(1.0));
            row.set_min_height(Some(Value::Point(0.0)));
            {
                let files = files.clone();
                let (props_loading, props_error, detail, mode, open_switch, click_row, help_view) =
                    (props_loading.clone(), props_error.clone(), detail.clone(), mode.clone(), open_switch.clone(), click_row.clone(), help_view.clone());
                insert(
                    row,
                    show(
                        move || show_file_tree.get(),
                        move || {
                            let files = files.clone();
                            let (mode, help_view) = (mode.clone(), help_view.clone());
                            let (props_loading, props_error, detail, open_switch, click_row) =
                                (props_loading.clone(), props_error.clone(), detail.clone(), open_switch.clone(), click_row.clone());
                            component(move || {
                                let files = files.clone();
                                let help_view = help_view.clone();
                                let mode = mode.clone();
                                file_tree(FileTreeProps {
                                    width: Rc::new(move || file_tree_width.get()),
                                    files: Rc::new(move || files().iter().map(|f| FileTreeItem { file: f.file.clone(), status: Some(f.status.clone()) }).collect()),
                                    loading: props_loading.clone(),
                                    error: props_error.clone(),
                                    list: false,
                                    selected_file_index: Rc::new(move || {
                                        selected_file_index.get().or_else(|| if single_patch.get() { visible_patch_files.with(|v| v.first().map(|(i, _)| *i)) } else { None })
                                    }),
                                    reviewed_file_names: Rc::new(move || reviewed_file_names.get()),
                                    expanded_nodes: Some(Rc::new(move || expanded_file_nodes.get())),
                                    on_row_click: Some(click_row.clone()),
                                    on_file_context_menu: None,
                                    source: Some(Rc::new(move || mode().source().0.to_string())),
                                    source_detail: Some(detail.clone()),
                                    on_switch_source: Some(open_switch.clone()),
                                    footer: Some(Rc::new(move || help_view(false, None, Rc::new(|| false)))),
                                })
                            })
                        },
                        None,
                    ),
                    At::End,
                );
            }
            let pane = box_();
            insert_node(row, pane, None);
            pane.set_flex_grow(Some(1.0));
            pane.set_min_width(Some(Value::Point(0.0)));
            pane.set_min_height(Some(Value::Point(0.0)));
            let edge = box_();
            insert_node(pane, edge, None);
            edge.set_id("diff-patch-top-edge");
            {
                let inset = inset.clone();
                bind(move || inset(), move |i| {
                    edge.set_margin(Edge::Left, Some(Value::Point(i as f32)));
                    edge.set_margin(Edge::Right, Some(Value::Point(i as f32)));
                });
            }
            edge.set_height(Some(Value::Point(1.0)));
            edge.set_flex_shrink(Some(0.0));
            edge.set_background_color(Some(rgba(theme.with_untracked(|t| t.diff.background.context))));
            {
                // The fixed edge belongs to the visible card, not the tree selection.
                let (scroll, nodes) = (scroll.clone(), nodes.clone());
                edge.set_on_lifecycle_pass(Some(Rc::new(move || {
                    let Some(scroll) = scroll.get() else { return };
                    let viewport_y = scroll.scroll_viewport().y;
                    let entry = visible_patch_files
                        .get_untracked()
                        .into_iter()
                        .rev()
                        .find(|(i, _)| nodes.borrow().get(i).map(|n| n.y() as f64).unwrap_or(f64::INFINITY) <= viewport_y);
                    let reviewed = entry.is_some_and(|(_, f)| reviewed_file_names.with_untracked(|r| r.contains(&f.file)));
                    let color = theme.with_untracked(|t| if reviewed { rgba(t.background.raised.high) } else { rgba(t.diff.background.context) });
                    edge.set_background_color(Some(color));
                })));
                on_cleanup(move || edge.set_on_lifecycle_pass(None));
            }
            let patches = scrollbox();
            insert_node(pane, patches, None);
            scroll.set(Some(patches));
            patches.set_id("diff-patches");
            patches.set_flex_grow(Some(1.0));
            patches.set_min_height(Some(Value::Point(0.0)));
            bind_always(move || patch_scroll_acceleration.get(), move |a| patches.set_scroll_acceleration(a));
            {
                let clear = clear.clone();
                patches.set_on_mouse(Some(MouseEventType::Scroll), Some(Rc::new(move |_| clear())));
            }
            patches.set_scrollbar_options(Some(true), &ScrollBarOptions { visible: Some(Some(false)), ..Default::default() });
            patches.set_scrollbar_options(Some(false), &ScrollBarOptions { visible: Some(Some(false)), ..Default::default() });
            let (scroll, register, diffs, inset, surface, mode) =
                (scroll.clone(), register.clone(), diffs.clone(), inset.clone(), surface.clone(), mode.clone());
            let load_image = props.load_image.clone();
            insert(
                patches,
                for_each(
                    move || visible_patch_files.get(),
                    |(i, f): &(usize, DiffFile)| format!("{i}:{}", f.file),
                    move |entry, index| {
                        let (file_index, file) = entry.get_untracked();
                        patch_card(PatchCard {
                            file_index,
                            file,
                            index,
                            reviewed: Rc::new(move || reviewed_file_names.with(|r| r.contains(&entry.with(|(_, f)| f.file.clone())))),
                            compact,
                            patch_pane_width,
                            view,
                            mode: mode.clone(),
                            inset: inset.clone(),
                            surface: surface.clone(),
                            scroll: scroll.clone(),
                            register: register.clone(),
                            diffs: diffs.clone(),
                            load_image: load_image.clone(),
                        })
                    },
                    None,
                ),
                At::Only,
            );
            View::Node(row)
        }
    };
    {
        let (loading, error, unavailable, source_base, mode, files) =
            (props.loading.clone(), props.error.clone(), props.unavailable.clone(), props.source_base.clone(), mode.clone(), files.clone());
        let (l1, l2, l3, l4, l5) = (loading.clone(), loading.clone(), loading.clone(), loading.clone(), loading.clone());
        let e1 = error.clone();
        let files1 = files.clone();
        insert(
            body,
            switch(
                vec![
                    when(move || l1(), move || message(|t| t.text.muted, Rc::new(|| "Loading diff…".into()))),
                    when(move || !l2() && error(), move || {
                        let (source_base, mode) = (source_base.clone(), mode.clone());
                        message(
                            |t| t.text.feedback.error.base,
                            Rc::new(move || {
                                if source_base().is_none() && mode().needs_base() {
                                    "Could not load diff. Choose a base branch from Diff source, or select Uncommitted.".into()
                                } else {
                                    "Could not load diff. Reopen the diff viewer to try again.".into()
                                }
                            }),
                        )
                    }),
                    when(move || !l3() && unavailable(), move || {
                        message(|t| t.text.muted, Rc::new(|| "Committed comparison unavailable without base metadata. Choose a base branch from Diff source.".into()))
                    }),
                    when(move || !l4() && files1().is_empty(), move || message(|t| t.text.muted, Rc::new(|| "No changes to show".into()))),
                    when(move || !l5(), content_view),
                ],
                None,
            ),
            At::Only,
        );
        let _ = e1;
    }

    // The footer of the compact layout.
    {
        let (docked, focused, shortcuts, close, panel, help_view, surface) =
            (docked.clone(), focused.clone(), shortcuts.clone(), close.clone(), panel.clone(), help_shortcut_view.clone(), surface.clone());
        insert(
            root,
            show(
                move || compact.get(),
                move || {
                    let footer = box_();
                    footer.set_id("diff-footer");
                    footer.set_flex_shrink(Some(0.0));
                    bind_always(move || rgba(theme.with(|t| t.background.base)), move |c| footer.set_background_color(Some(c)));
                    {
                        let (docked, surface) = (docked.clone(), surface.clone());
                        insert(
                            footer,
                            show(
                                move || docked(),
                                move || {
                                    let ease = box_();
                                    ease.set_height(Some(Value::Point(1.0)));
                                    ease.set_flex_shrink(Some(0.0));
                                    ease.set_border(BorderProp::Sides(vec![Side::Bottom]));
                                    let surface = surface.clone();
                                    bind_always(move || surface(), move |c| ease.set_border_color(Some(c)));
                                    bind_always(move || rgba(theme.with(|t| t.background.base)), move |c| ease.set_background_color(Some(c)));
                                    ease.set_custom_border_chars(Some(empty_border(|c| c.horizontal = "▀")));
                                    View::Node(ease)
                                },
                                None,
                            ),
                            At::End,
                        );
                    }
                    let line = box_();
                    insert_node(footer, line, None);
                    line.set_height(Some(Value::Point(1.0)));
                    line.set_flex_shrink(Some(0.0));
                    line.set_flex_direction(FlexDirection::Row);
                    line.set_padding(Edge::Left, Some(Value::Point(2.0)));
                    line.set_padding(Edge::Right, Some(Value::Point(1.0)));
                    let hint = text();
                    insert_node(line, hint, None);
                    {
                        let focused1 = focused.clone();
                        bind_always(
                            move || theme.with(|t| if focused1() { rgba(t.background.action.primary.focused) } else { rgba(t.text.action.primary.disabled) }),
                            move |c| hint.set_fg(Some(c)),
                        );
                        let focused2 = focused.clone();
                        bind(move || focused2(), move |f| hint.set_attributes(Some(if f { 0 } else { text_attributes::DIM })));
                    }
                    hint.set_flex_grow(Some(1.0));
                    hint.set_min_width(Some(Value::Point(0.0)));
                    hint.set_wrap_mode(WrapMode::None);
                    hint.set_truncate(true);
                    hint.set_selectable(false);
                    {
                        let (docked, close, panel) = (docked.clone(), close.clone(), panel.clone());
                        hint.set_on_mouse(
                            Some(MouseEventType::Up),
                            Some(Rc::new(move |event| {
                                if event.button != 0 {
                                    return;
                                }
                                if !docked() {
                                    return close();
                                }
                                if let Some(panel) = &panel {
                                    if !(panel.focused)() {
                                        (panel.focus)();
                                    }
                                }
                            })),
                        );
                    }
                    // Without a prompt beside the viewer, closing is the way back to the session.
                    let key = span();
                    insert_node(hint, key, None);
                    {
                        let (docked, shortcuts) = (docked.clone(), shortcuts.clone());
                        insert(
                            key,
                            View::dynamic(move || View::from(shortcuts.get(if docked() { "pane.focus.left" } else { "diff.close" }).unwrap_or_default())),
                            At::Only,
                        );
                    }
                    let label = span();
                    insert_node(hint, label, None);
                    {
                        let focused = focused.clone();
                        bind_always(move || theme.with(|t| if focused() { Some(rgba(t.text.muted)) } else { None }), move |c| label.set_span_style(c, None, 0, 0));
                    }
                    {
                        let docked = docked.clone();
                        insert(label, View::dynamic(move || View::from(if docked() { " focus prompt" } else { " close" })), At::Only);
                    }
                    let help = box_();
                    insert_node(line, help, None);
                    help.set_flex_shrink(Some(0.0));
                    {
                        let focused = focused.clone();
                        insert(help, help_view(false, Some("see all"), Rc::new(move || !focused())), At::Only);
                    }
                    let docked = docked.clone();
                    insert(
                        footer,
                        show(
                            move || docked(),
                            || {
                                let gap = box_();
                                gap.set_height(Some(Value::Point(1.0)));
                                gap.set_flex_shrink(Some(0.0));
                                View::Node(gap)
                            },
                            None,
                        ),
                        At::End,
                    );
                    View::Node(footer)
                },
                None,
            ),
            At::End,
        );
    }
    {
        let help_view = help_shortcut_view.clone();
        insert(
            root,
            show(
                move || !show_file_tree.get() && !compact.get(),
                move || {
                    let el = box_();
                    el.set_position_type(true);
                    el.set_positions(None, Some(Some(Value::Point(0.0))), Some(Some(Value::Point(0.0))), None);
                    el.set_width(Some(Value::Point(1.0)));
                    el.set_height(Some(Value::Point(1.0)));
                    insert(el, help_view(true, None, Rc::new(|| false)), At::Only);
                    View::Node(el)
                },
                None,
            ),
            At::End,
        );
    }
    let _ = (props.sources.clone(), string_width("x"), truncate_file_path("x", 1));
    View::Node(root)
}

struct PatchCard {
    file_index: usize,
    file: DiffFile,
    index: Signal<usize>,
    reviewed: Rc<dyn Fn() -> bool>,
    compact: Memo<bool>,
    patch_pane_width: Memo<i64>,
    view: Memo<&'static str>,
    mode: Rc<dyn Fn() -> DiffMode>,
    inset: Rc<dyn Fn() -> i64>,
    surface: Rc<dyn Fn() -> RGBA>,
    scroll: Rc<Cell<Option<Element>>>,
    register: Rc<dyn Fn(usize, Element)>,
    diffs: Rc<RefCell<HashMap<usize, PatchDiffRef>>>,
    load_image: Rc<dyn Fn(String) -> opencode_tui_sync::client::Call>,
}

/// One file's card: the gap above it, the sticky header with the counts, and its patch.
fn patch_card(props: PatchCard) -> View {
    let theme = tui().theme;
    let PatchCard { file_index, file, index, reviewed, compact, patch_pane_width, view, mode, inset, surface, scroll, register, diffs, load_image } = props;
    let background: Rc<dyn Fn() -> RGBA> = {
        let reviewed = reviewed.clone();
        Rc::new(move || theme.with(|t| if reviewed() { rgba(t.background.raised.high) } else { rgba(t.diff.background.context) }))
    };
    let image = is_diff_image_file(&file.file);
    let counts_width = {
        let reviewed = reviewed.clone();
        let (additions, deletions) = (opencode_tui_sync::js::number(file.additions).len(), opencode_tui_sync::js::number(file.deletions).len());
        move || (if image { 6 } else { additions + deletions + 5 }) as i64 + if reviewed() { 2 } else { 0 }
    };
    let card = box_();
    register(file_index, card);
    {
        let (background, surface, inset) = (background.clone(), surface.clone(), inset.clone());
        insert(card, show(move || index.get() > 0, move || card_gap("▄", background.clone(), surface.clone(), inset.clone()), None), At::End);
    }
    let body = box_();
    insert_node(card, body, None);
    {
        let background = background.clone();
        bind_always(move || background(), move |c| body.set_background_color(Some(c)));
    }
    {
        let inset = inset.clone();
        bind(move || inset(), move |i| {
            body.set_margin(Edge::Left, Some(Value::Point(i as f32)));
            body.set_margin(Edge::Right, Some(Value::Point(i as f32)));
        });
    }
    let header = box_();
    insert_node(body, header, None);
    header.set_id(&format!("diff-file-header-{file_index}"));
    {
        // Move the original title without changing flow, bounded by its own card.
        let scroll = scroll.clone();
        header.set_on_lifecycle_pass(Some(Rc::new(move || {
            let (Some(scroll), Some(parent)) = (scroll.get(), header.parent()) else { return };
            let offset = scroll.scroll_top() - (parent.y() as f64 - scroll.scroll_content().y);
            let y = 0f64.max(offset.min((parent.height() - header.height()) as f64));
            header.set_translate(None, Some(y));
        })));
        on_cleanup(move || header.set_on_lifecycle_pass(None));
    }
    header.set_flex_direction(FlexDirection::Row);
    header.set_gap(2, Some(Value::Point(1.0)));
    header.set_flex_shrink(Some(0.0));
    header.set_height(Some(Value::Point(FILE_HEADER_HEIGHT as f32)));
    header.set_z_index(1);
    {
        let background = background.clone();
        bind_always(move || background(), move |c| header.set_background_color(Some(c)));
    }
    bind(move || if compact.get() { 2.0 } else { 1.0 }, move |p| header.set_padding(Edge::Left, Some(Value::Point(p))));
    header.set_padding(Edge::Right, Some(Value::Point(1.0)));
    header.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    let path_box = box_();
    insert_node(header, path_box, None);
    path_box.set_flex_grow(Some(1.0));
    path_box.set_min_width(Some(Value::Point(0.0)));
    {
        let reviewed = reviewed.clone();
        let file_name = file.file.clone();
        let el = text();
        insert_node(path_box, el, None);
        bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| el.set_fg(Some(c)));
        el.set_wrap_mode(WrapMode::None);
        el.set_truncate(true);
        let display = create_memo(move || {
            let value = truncate_file_path(&file_name, 1.max(patch_pane_width.get() - counts_width() - 2));
            let at = value.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
            (value[..at].to_string(), value[at..].to_string())
        });
        let parent = span();
        let basename = span();
        insert_node(el, parent, None);
        insert_node(el, basename, None);
        bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| parent.set_span_style(Some(c), None, 0, 0));
        bind_always(
            move || theme.with(|t| if reviewed() { rgba(t.text.muted) } else { rgba(t.text.base) }),
            move |c| basename.set_span_style(Some(c), None, 0, 0),
        );
        insert(parent, View::dynamic(move || View::from(display.get().0)), At::Only);
        insert(basename, View::dynamic(move || View::from(display.get().1)), At::Only);
    }
    let muted_text = move |value: String| {
        let el = text();
        bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| el.set_fg(Some(c)));
        el.set_flex_shrink(Some(0.0));
        insert(el, View::from(value), At::Only);
        View::Node(el)
    };
    {
        let reviewed = reviewed.clone();
        insert(header, show(move || reviewed(), move || muted_text("✓".into()), None), At::End);
    }
    if image {
        insert(header, muted_text("Image".into()), At::End);
    } else {
        for (sign, value, added) in [("+", file.additions, true), ("-", file.deletions, false)] {
            let el = text();
            el.set_flex_shrink(Some(0.0));
            let reviewed = reviewed.clone();
            bind_always(
                move || {
                    theme.with(|t| {
                        if reviewed() {
                            rgba(t.text.muted)
                        } else if added {
                            rgba(t.diff.text.added)
                        } else {
                            rgba(t.diff.text.removed)
                        }
                    })
                },
                move |c| el.set_fg(Some(c)),
            );
            insert(el, View::from(format!("{sign}{}", opencode_tui_sync::js::number(value))), At::Only);
            insert(header, View::Node(el), At::End);
        }
    }
    {
        let (reviewed, background, surface, inset) = (reviewed.clone(), background.clone(), surface.clone(), inset.clone());
        insert(
            header,
            show(
                move || reviewed(),
                move || {
                    let el = box_();
                    el.set_position_type(true);
                    let inset = inset.clone();
                    let i = inset();
                    el.set_positions(
                        Some(Some(Value::Point(-(i as f32)))),
                        None,
                        Some(Some(Value::Point(-(i as f32)))),
                        Some(Some(Value::Point(0.0))),
                    );
                    el.set_height(Some(Value::Point(1.0)));
                    insert(el, card_gap("▀", background.clone(), surface.clone(), inset.clone()), At::Only);
                    View::Node(el)
                },
                None,
            ),
            At::End,
        );
    }
    {
        let reviewed = reviewed.clone();
        let file = file.clone();
        insert(
            body,
            show(
                move || !reviewed(),
                move || {
                    let mode_now = mode();
                    let has_patch = file.patch.as_ref().is_some_and(|p| !p.is_empty());
                    if mode_now != DiffMode::Committed && file.status != "deleted" && image {
                        let name = file.file.clone();
                        let load_image = load_image.clone();
                        return diff_image(name, load_image);
                    }
                    if !(mode_now == DiffMode::Committed && image) && has_patch {
                        let patch = file.patch.clone().unwrap_or_default();
                        let status = file.status.clone();
                        let name = file.file.clone();
                        let diffs = diffs.clone();
                        return component(move || {
                            let diffs = diffs.clone();
                            let patch = patch.clone();
                            let status = status.clone();
                            let name = name.clone();
                            patch_diff(PatchDiffProps {
                                diff: Rc::new(move || patch.clone()),
                                hunk_fg: Rc::new(move || rgba(theme.with(|t| t.diff.text.hunk_header))),
                                line_number_bg: Rc::new(move || rgba(theme.with(|t| t.diff.background.context))),
                                on_ref: Some(Rc::new(move |component: PatchDiffRef| {
                                    diffs.borrow_mut().insert(file_index, component);
                                    let diffs = diffs.clone();
                                    on_cleanup(move || {
                                        diffs.borrow_mut().remove(&file_index);
                                    });
                                })),
                                gutter_padding: Some({
                                    let compact = compact.clone();
                                    Rc::new(move || if compact.get() { 1 } else { 0 })
                                }),
                                diff_props: DiffProps::themed(
                                    theme,
                                    Rc::new(move || if status == "modified" && view.get() == "split" { DiffView::Split } else { DiffView::Unified }),
                                    Rc::new(move || filetype(Some(&name)).map(str::to_string)),
                                    Rc::new(|| WrapMode::Char),
                                ),
                            })
                        });
                    }
                    let el = box_();
                    el.set_width(Some(Value::Percent(100.0)));
                    el.set_flex_shrink(Some(0.0));
                    bind(move || if compact.get() { 2.0 } else { 1.0 }, move |p| el.set_padding(Edge::Left, Some(Value::Point(p))));
                    el.set_padding(Edge::Right, Some(Value::Point(1.0)));
                    el.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
                    let message = if mode_now == DiffMode::Committed && image {
                        "Committed image preview unavailable. The working-tree image is not shown."
                    } else if file.status == "deleted" && image {
                        "Deleted image. The previous revision is not available for preview."
                    } else {
                        "No patch available for this file."
                    };
                    insert(el, muted_text(message.into()), At::Only);
                    View::Node(el)
                },
                None,
            ),
            At::End,
        );
    }
    View::Node(card)
}

/// An image preview's loaded bytes.
#[derive(Clone, PartialEq)]
enum ImageState {
    Loading,
    Ready(Rc<Vec<u8>>),
    Error,
}

/// `<DiffViewerImage>` (`diff-viewer-image.tsx`): the working-tree preview of a changed image. The terminal's image
/// protocol is not ported; a loaded image shows the block its `fit` would occupy, which is what a text capture has.
fn diff_image(file: String, load: Rc<dyn Fn(String) -> opencode_tui_sync::client::Call>) -> View {
    let t = tui();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let state = create_signal(ImageState::Loading).0;
    {
        let load = load.clone();
        create_effect(move || {
            let path = file.clone();
            let call = load(path);
            state.set(ImageState::Loading);
            tokio::task::spawn_local(async move {
                state.set(match call.await {
                    Ok(value) => ImageState::Ready(Rc::new(value.bytes())),
                    Err(_) => ImageState::Error,
                });
            });
        });
    }
    let height = create_memo(move || ((dimensions.get().1 / 4) as i64).clamp(3, 8));
    let root = box_();
    root.set_width(Some(Value::Percent(100.0)));
    root.set_flex_direction(FlexDirection::Column);
    root.set_flex_shrink(Some(0.0));
    root.set_gap(2, Some(Value::Point(1.0)));
    root.set_padding(Edge::Left, Some(Value::Point(1.0)));
    root.set_padding(Edge::Right, Some(Value::Point(1.0)));
    root.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    let label = text();
    insert_node(root, label, None);
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| label.set_fg(Some(c)));
    insert_node(label, opencode_tui_core::renderables::text::text_node_from_string("Working tree preview"), None);
    let body = box_();
    insert_node(root, body, None);
    body.set_flex_direction(FlexDirection::Column);
    body.set_flex_shrink(Some(0.0));
    body.set_gap(2, Some(Value::Point(1.0)));
    {
        let height = height.clone();
        bind(move || height.get() as f32 + 2.0, move |h| body.set_height(Some(Value::Point(h))));
    }
    let error = text();
    insert_node(body, error, None);
    bind_always(move || rgba(theme.with(|th| th.text.feedback.error.base)), move |c| error.set_fg(Some(c)));
    insert_node(error, opencode_tui_core::renderables::text::text_node_from_string("Could not load image"), None);
    bind(move || matches!(state.get(), ImageState::Error), move |visible: bool| error.set_visible(visible));
    let loading = text();
    insert_node(body, loading, None);
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| loading.set_fg(Some(c)));
    insert_node(loading, opencode_tui_core::renderables::text::text_node_from_string("Loading image…"), None);
    bind(move || matches!(state.get(), ImageState::Loading), move |visible: bool| loading.set_visible(visible));
    let image_box = box_();
    insert_node(body, image_box, None);
    image_box.set_width(Some(Value::Percent(100.0)));
    image_box.set_flex_shrink(Some(0.0));
    image_box.set_align_items(opencode_tui_core::tree::Align::Center);
    {
        let height = height.clone();
        bind(move || height.get() as f32, move |h| image_box.set_height(Some(Value::Point(h))));
    }
    {
        let state = state.clone();
        bind(move || matches!(state.get(), ImageState::Ready(_)), move |visible: bool| image_box.set_visible(visible));
    }
    let picture = text();
    insert_node(image_box, picture, None);
    picture.set_wrap_mode(WrapMode::None);
    {
        let state = state.clone();
        let height = height.clone();
        bind(move || {
            let ImageState::Ready(bytes) = state.get() else { return String::new() };
            let (width, tall) = image_dimensions(&bytes);
            let rows = height.get();
            let columns = (rows as f64 * (width as f64 / tall.max(1) as f64 * 2.0)).round().max(1.0) as usize;
            vec!["█".repeat(columns); rows as usize].join("\n")
        }, move |value| picture.set_content(&value));
    }
    let details = box_();
    insert_node(body, details, None);
    details.set_flex_direction(FlexDirection::Row);
    details.set_height(Some(Value::Point(1.0)));
    details.set_flex_shrink(Some(0.0));
    details.set_justify_content(opencode_tui_core::tree::Justify::SpaceBetween);
    {
        let state = state.clone();
        bind(move || matches!(state.get(), ImageState::Ready(_)), move |visible: bool| details.set_visible(visible));
    }
    let size = text();
    insert_node(details, size, None);
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| size.set_fg(Some(c)));
    {
        let state = state.clone();
        bind(move || match state.get() {
            ImageState::Ready(bytes) => {
                let (width, height) = image_dimensions(&bytes);
                format!("{width} x {height}")
            }
            _ => String::new(),
        }, move |value| size.set_content(&value));
    }
    let enlarge = text();
    insert_node(details, enlarge, None);
    bind_always(move || rgba(theme.with(|th| th.text.action.secondary.base)), move |c| enlarge.set_fg(Some(c)));
    insert_node(enlarge, opencode_tui_core::renderables::text::text_node_from_string("Click to enlarge"), None);
    View::Node(root)
}

/// A PNG's pixel size from its IHDR chunk; other formats report 1 x 1 (their block still shows).
fn image_dimensions(bytes: &[u8]) -> (u32, u32) {
    if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]).max(1);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]).max(1);
        return (width, height);
    }
    (1, 1)
}

/// `DiffMode | "base"` in the source picker.
#[derive(Clone, Copy, PartialEq)]
enum SourceChoice {
    Mode(DiffMode),
    Base,
}

/// `DiffViewerHelpDialog`.
fn diff_help_dialog(
    dialog: crate::ui::dialog::DialogContext,
    shortcuts: crate::keymap::Shortcuts,
    single: bool,
    tree: bool,
    fullscreen: bool,
) -> View {
    let theme = crate::ui::theme::use_theme().surface(crate::theme::SurfaceName::Dialog);
    let key = {
        let shortcuts = shortcuts.clone();
        move |ids: &[&str]| {
            ids.iter()
                .filter_map(|id| shortcuts.get(id))
                .collect::<Vec<_>>()
                .join(" / ")
        }
    };
    let every = {
        let shortcuts = shortcuts.clone();
        move |id: &str| shortcuts.list(id).join(" / ")
    };
    let mut groups: Vec<(&str, Vec<(String, &'static str)>)> = vec![
        (
            "Review",
            vec![
                (every("diff.next_file"), "Next file"),
                (every("diff.previous_file"), "Previous file"),
                (
                    key(&["diff.mark_reviewed"]),
                    if single { "Review + next / reopen" } else { "Review + collapse / reopen" },
                ),
                (key(&["diff.next_hunk", "diff.previous_hunk"]), "Next / previous change"),
                ("right-click".into(), "File menu (heading or tree)"),
            ],
        ),
        (
            "Scroll",
            vec![
                (key(&["diff.down", "diff.up"]), "Down / up"),
                (key(&["diff.half_page.down", "diff.half_page.up"]), "Half page down / up"),
                (key(&["diff.page.down", "diff.page.up"]), "Page down / up"),
                (key(&["diff.first", "diff.last"]), "First / last"),
            ],
        ),
    ];
    let mut view_rows = vec![
        (key(&["diff.toggle_view"]), "Split / unified"),
        (key(&["diff.single_patch"]), "All files / single file"),
    ];
    if tree {
        view_rows.push((key(&["diff.toggle_file_tree"]), "Show / hide file tree"));
    }
    view_rows.push((key(&["diff.switch_source"]), "Switch diff source"));
    if fullscreen {
        view_rows.push((key(&["diff.toggle_fullscreen"]), "Full screen / split view"));
    }
    view_rows.push((every("diff.close"), "Close diff viewer"));
    groups.push(("View", view_rows));

    let body = box_();
    body.set_flex_direction(FlexDirection::Column);
    body.set_gap(2, Some(Value::Point(1.0)));
    for (title, rows) in groups {
        let group = box_();
        group.set_flex_direction(FlexDirection::Column);
        group.set_flex_shrink(Some(0.0));
        let heading = text();
        insert_node(group, heading, None);
        bind_always(move || rgba(theme.get().text.base), move |c| heading.set_fg(Some(c)));
        heading.set_attributes(Some(opencode_tui_core::color::text_attributes::BOLD));
        insert_node(heading, opencode_tui_core::renderables::text::text_node_from_string(title), None);
        for (shortcut, label) in rows {
            let row = box_();
            insert_node(group, row, None);
            row.set_flex_direction(FlexDirection::Row);
            row.set_gap(2, Some(Value::Point(2.0)));
            let keys = text();
            insert_node(row, keys, None);
            keys.set_width(Some(Value::Point(17.0)));
            keys.set_flex_shrink(Some(0.0));
            bind_always(move || rgba(theme.get().text.base), move |c| keys.set_fg(Some(c)));
            insert_node(keys, opencode_tui_core::renderables::text::text_node_from_string(&shortcut), None);
            let description = text();
            insert_node(row, description, None);
            description.set_flex_grow(Some(1.0));
            description.set_min_width(Some(Value::Point(0.0)));
            bind_always(move || rgba(theme.get().text.muted), move |c| description.set_fg(Some(c)));
            insert_node(description, opencode_tui_core::renderables::text::text_node_from_string(label), None);
        }
        insert_node(body, group, None);
    }
    let close = dialog.clone();
    crate::ui::dialog::dialog_layout(crate::ui::dialog::LayoutProps {
        header: Box::new(move || {
            let close = close.clone();
            component(move || {
                let close = close.clone();
                crate::ui::dialog::dialog_header(crate::ui::dialog::HeaderProps {
                    title: Some(Rc::new(|| "Diff shortcuts".to_string())),
                    close_label: Some("esc close".into()),
                    on_close: Some(Rc::new(move || close.clear())),
                    ..crate::ui::dialog::HeaderProps::default()
                })
            })
        }),
        children: Box::new(move || View::Node(body)),
        ..crate::ui::dialog::LayoutProps::default()
    })
}
