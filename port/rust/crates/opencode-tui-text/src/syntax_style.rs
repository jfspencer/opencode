//! Syntax styles: the native table (`syntax-style.zig`) and the TS wrapper over it (`syntax-style.ts`), whose
//! definition and merge caches live on the TS side.

use crate::Rgba;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Foreground, background and text attributes of a style (`StyleDefinition` in Zig).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct StyleDefinition {
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub attributes: u32,
}

/// The first id registration never returns (`OT_STYLE_RUN_FLAG`): ids with this bit name a buffer's run styles.
pub const ID_LIMIT: u32 = 0x8000_0000;

/// The native style table (`SyntaxStyle` in `syntax-style.zig`).
#[derive(Debug)]
pub struct SyntaxStyleCore {
    name_to_id: HashMap<Box<str>, u32>,
    id_to_style: HashMap<u32, StyleDefinition>,
    next_id: u32,
}

impl Default for SyntaxStyleCore {
    fn default() -> Self {
        SyntaxStyleCore { name_to_id: HashMap::new(), id_to_style: HashMap::new(), next_id: 1 }
    }
}

impl SyntaxStyleCore {
    pub fn register(&mut self, name: &str, definition: StyleDefinition) -> Option<u32> {
        if let Some(&id) = self.name_to_id.get(name) {
            self.id_to_style.insert(id, definition);
            return Some(id);
        }
        let id = self.next_id;
        if id == 0 || id >= ID_LIMIT {
            return None;
        }
        self.name_to_id.insert(name.into(), id);
        self.id_to_style.insert(id, definition);
        self.next_id += 1;
        Some(id)
    }

    pub fn resolve_by_id(&self, id: u32) -> Option<StyleDefinition> {
        self.id_to_style.get(&id).copied()
    }

    pub fn resolve_by_name(&self, name: &str) -> Option<u32> {
        self.name_to_id.get(name).copied()
    }

    pub fn style_count(&self) -> usize {
        self.id_to_style.len()
    }
}

/// Attribute bits (`TextAttributes`).
pub mod attr {
    pub const BOLD: u32 = 1 << 0;
    pub const DIM: u32 = 1 << 1;
    pub const ITALIC: u32 = 1 << 2;
    pub const UNDERLINE: u32 = 1 << 3;
    pub const BLINK: u32 = 1 << 4;
    pub const INVERSE: u32 = 1 << 5;
    pub const HIDDEN: u32 = 1 << 6;
    pub const STRIKETHROUGH: u32 = 1 << 7;
}

/// A style as TS registers it (`StyleDefinition` in `syntax-style.ts`).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct StyleSpec {
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub dim: Option<bool>,
}

/// `createTextAttributes` (`utils.ts`).
pub fn create_text_attributes(bold: bool, italic: bool, underline: bool, dim: bool) -> u32 {
    let mut a = 0;
    if bold {
        a |= attr::BOLD;
    }
    if italic {
        a |= attr::ITALIC;
    }
    if underline {
        a |= attr::UNDERLINE;
    }
    if dim {
        a |= attr::DIM;
    }
    a
}

/// `MergedStyle`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MergedStyle {
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub attributes: u32,
}

struct SyntaxStyleState {
    name_cache: HashMap<String, u32>,
    style_defs: indexmap::IndexMap<String, StyleSpec>,
    merged_cache: HashMap<String, MergedStyle>,
}

/// `SyntaxStyle` (`syntax-style.ts`): shared by the text buffers that draw with it.
#[derive(Clone)]
pub struct SyntaxStyle {
    pub(crate) core: Rc<RefCell<SyntaxStyleCore>>,
    state: Rc<RefCell<SyntaxStyleState>>,
}

impl SyntaxStyle {
    pub fn create() -> SyntaxStyle {
        SyntaxStyle {
            core: Rc::new(RefCell::new(SyntaxStyleCore::default())),
            state: Rc::new(RefCell::new(SyntaxStyleState {
                name_cache: HashMap::new(),
                style_defs: indexmap::IndexMap::new(),
                merged_cache: HashMap::new(),
            })),
        }
    }

    /// `SyntaxStyle.fromStyles`.
    pub fn from_styles<'a>(styles: impl IntoIterator<Item = (&'a str, StyleSpec)>) -> SyntaxStyle {
        let style = SyntaxStyle::create();
        for (name, spec) in styles {
            style.register_style(name, spec);
        }
        style
    }

    pub fn ptr_eq(&self, other: &SyntaxStyle) -> bool {
        Rc::ptr_eq(&self.core, &other.core)
    }

    /// `registerStyle`: returns the style id (0 when the native table is full).
    pub fn register_style(&self, name: &str, spec: StyleSpec) -> u32 {
        let attributes = create_text_attributes(
            spec.bold.unwrap_or(false),
            spec.italic.unwrap_or(false),
            spec.underline.unwrap_or(false),
            spec.dim.unwrap_or(false),
        );
        let id = self.core.borrow_mut().register(name, StyleDefinition { fg: spec.fg, bg: spec.bg, attributes }).unwrap_or(0);
        let mut state = self.state.borrow_mut();
        state.name_cache.insert(name.to_owned(), id);
        state.style_defs.insert(name.to_owned(), spec);
        state.merged_cache.clear();
        id
    }

    pub fn resolve_style_id(&self, name: &str) -> Option<u32> {
        if let Some(&id) = self.state.borrow().name_cache.get(name) {
            return Some(id);
        }
        let id = self.core.borrow().resolve_by_name(name)?;
        self.state.borrow_mut().name_cache.insert(name.to_owned(), id);
        Some(id)
    }

    pub fn get_style_id(&self, name: &str) -> Option<u32> {
        if let Some(id) = self.resolve_style_id(name) {
            return Some(id);
        }
        if name.contains('.') {
            return self.resolve_style_id(name.split('.').next().unwrap());
        }
        None
    }

    pub fn get_style_count(&self) -> usize {
        self.core.borrow().style_count()
    }

    pub fn clear_name_cache(&self) {
        self.state.borrow_mut().name_cache.clear();
    }

    pub fn get_style(&self, name: &str) -> Option<StyleSpec> {
        let state = self.state.borrow();
        if let Some(spec) = state.style_defs.get(name) {
            return Some(*spec);
        }
        if name.contains('.') {
            return state.style_defs.get(name.split('.').next().unwrap()).copied();
        }
        None
    }

    pub fn merge_styles(&self, names: &[&str]) -> MergedStyle {
        let key = names.join(":");
        if let Some(cached) = self.state.borrow().merged_cache.get(&key) {
            return *cached;
        }
        let mut merged = StyleSpec::default();
        for name in names {
            let Some(style) = self.get_style(name) else { continue };
            if style.fg.is_some() {
                merged.fg = style.fg;
            }
            if style.bg.is_some() {
                merged.bg = style.bg;
            }
            if style.bold.is_some() {
                merged.bold = style.bold;
            }
            if style.italic.is_some() {
                merged.italic = style.italic;
            }
            if style.underline.is_some() {
                merged.underline = style.underline;
            }
            if style.dim.is_some() {
                merged.dim = style.dim;
            }
        }
        let result = MergedStyle {
            fg: merged.fg,
            bg: merged.bg,
            attributes: create_text_attributes(
                merged.bold.unwrap_or(false),
                merged.italic.unwrap_or(false),
                merged.underline.unwrap_or(false),
                merged.dim.unwrap_or(false),
            ),
        };
        self.state.borrow_mut().merged_cache.insert(key, result);
        result
    }

    pub fn clear_cache(&self) {
        self.state.borrow_mut().merged_cache.clear();
    }

    pub fn get_cache_size(&self) -> usize {
        self.state.borrow().merged_cache.len()
    }

    pub fn get_registered_names(&self) -> Vec<String> {
        self.state.borrow().style_defs.keys().cloned().collect()
    }

    pub fn get_all_styles(&self) -> Vec<(String, StyleSpec)> {
        self.state.borrow().style_defs.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }
}
