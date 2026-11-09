//! The text of a native scene Text node (`Context.Text` and its `scene*Text*` calls in `context.zig`): what
//! `TextBufferRenderable` drives through `NativeScene` (`setText`, `setTextOptions`, `setTextSelection`,
//! `getTextMetrics`, `getTextLineInfo`, ...) when it does not own a shared buffer and view.
//!
//! The scene owns Yoga and layout; it calls [`SceneText::set_options`] with the node's style size,
//! [`SceneText::apply_layout`] after a layout, and [`SceneText::yoga_measure`] from the measure function.

use crate::text::{decode_text, prepare_styled, validate_text_bytes, DrawSource, StyledChunk, TextContext};
use crate::text_buffer::TextBufferCore;
use crate::text_buffer_view::{LineInfo, MeasureResult, SelectionBehavior, TextAlign, TextBufferViewCore, Viewport, WrapMode};
use crate::utf8::{ControlDisplay, WidthMethod};
use crate::{link, Rgba, TextError};

/// `NativeSceneTextOptions` (scroll offsets as the f64 TS passes).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SceneTextOptions {
    pub fg: Rgba,
    pub bg: Rgba,
    pub attributes: u32,
    pub wrap_mode: WrapMode,
    pub text_align: TextAlign,
    pub truncate: bool,
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub first_line_offset: u32,
    /// A code point; 0 for none.
    pub tab_indicator: u32,
    pub tab_indicator_color: Option<Rgba>,
}

/// What a scene must invalidate after an options change.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct OptionsChange {
    /// The Yoga measure depends on the changed options: mark the node dirty.
    pub measure_changed: bool,
    /// The prepared view changed: repaint.
    pub prepare_changed: bool,
}

/// `NativeSceneTextMetrics`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SceneTextMetrics {
    pub byte_length: u32,
    pub text_length: u32,
    pub line_count: u32,
    pub virtual_line_count: u32,
    pub width_cols_max: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectionOperation {
    Reset,
    Set,
    Update,
}

/// A scene Text node's buffer, view and scroll state.
pub struct SceneText {
    tb: TextBufferCore,
    view: TextBufferViewCore,
    input_mem_id: Option<u8>,
    initial_viewport: Option<Viewport>,
    scroll_x: f64,
    scroll_y: f64,
}

impl SceneText {
    /// A Text node's resource, with the renderer's width method (`createTextResource`).
    pub fn new(width_method: WidthMethod, ctx: &TextContext) -> SceneText {
        SceneText {
            tb: TextBufferCore::new(width_method, ControlDisplay::Hidden, ctx.links.clone()),
            view: TextBufferViewCore::new(),
            input_mem_id: None,
            initial_viewport: None,
            scroll_x: 0.0,
            scroll_y: 0.0,
        }
    }

    fn holds_bytes(&self, bytes: &[u8]) -> bool {
        let Some(id) = self.input_mem_id else { return false };
        let registry = &self.tb.mem_registry;
        registry.len() == 1 && self.tb.byte_size() as usize == bytes.len() && registry.get(id).is_some_and(|b| b == bytes)
    }

    /// `sceneSetText`. Returns whether the bytes changed (the scene re-measures then; equal bytes repaint).
    pub fn set_text(&mut self, text: &str) -> Result<bool, TextError> {
        validate_text_bytes(self.tb.tab_width, text.len(), 0)?;
        let changed = !self.holds_bytes(text.as_bytes());
        self.input_mem_id = Some(self.tb.replace_text(text.as_bytes().to_vec(), self.input_mem_id)?);
        Ok(changed)
    }

    /// `sceneSetStyledText` (TS drops empty chunks before the call).
    pub fn set_styled_text(&mut self, chunks: &[StyledChunk]) -> Result<bool, TextError> {
        let chunks: Vec<StyledChunk> = chunks.iter().filter(|c| !c.text.is_empty()).copied().collect();
        let (bytes, ranges, links) = prepare_styled(&self.tb, &chunks, false, true)?;
        let changed = !self.holds_bytes(&bytes);
        self.input_mem_id = Some(self.tb.replace_owned_styled_text(bytes, self.input_mem_id, &ranges, links)?);
        Ok(changed)
    }

    /// `sceneSetTextOptions`. `style_size` gives the node's Yoga style width and height when they are points
    /// (`None` for auto, percent or undefined); it is read only before the first viewport.
    pub fn set_options(
        &mut self,
        options: SceneTextOptions,
        style_size: impl FnOnce() -> [Option<f32>; 2],
    ) -> Result<OptionsChange, TextError> {
        if options.attributes & !link::attributes::BASE_MASK != 0
            || options.first_line_offset > i32::MAX as u32
            || !options.scroll_x.is_finite()
            || options.scroll_x < 0.0
            || options.scroll_x > i32::MAX as f64
            || !options.scroll_y.is_finite()
            || options.scroll_y < 0.0
            || options.scroll_y > i32::MAX as f64
        {
            return Err(TextError::InvalidOptions);
        }
        let indicator = (options.tab_indicator != 0).then_some(options.tab_indicator);
        crate::text::validate_tab_indicator(indicator)?;
        let previous = self.view.viewport;
        let mut has_viewport = previous.is_some();
        let mut viewport = match previous.or(self.initial_viewport) {
            Some(vp) => vp,
            None => {
                let mut initial = Viewport { x: 0, y: 0, width: 0, height: 0 };
                has_viewport = true;
                for (kind, size) in style_size().into_iter().enumerate() {
                    match size {
                        Some(value) if value >= 0.0 => {
                            if !value.is_finite() || value as f64 > i32::MAX as f64 {
                                return Err(TextError::InvalidOptions);
                            }
                            if kind == 0 {
                                initial.width = value as u32;
                            } else {
                                initial.height = value as u32;
                            }
                            if value == 0.0 {
                                has_viewport = false;
                            }
                        }
                        _ => has_viewport = false,
                    }
                }
                initial
            }
        };
        let wrap_changed = self.view.wrap_mode != options.wrap_mode;
        let scroll_changed = self.scroll_x != options.scroll_x || self.scroll_y != options.scroll_y;
        let measure_changed = wrap_changed || self.view.first_line_offset != options.first_line_offset;
        let prepare_changed = measure_changed || scroll_changed || self.view.truncate != options.truncate;
        self.tb.default_fg = Some(options.fg);
        self.tb.default_bg = Some(options.bg);
        self.tb.default_attributes = Some(options.attributes);
        self.view.set_wrap_mode(options.wrap_mode);
        self.view.text_align = options.text_align;
        self.view.set_truncate(options.truncate);
        self.view.set_first_line_offset(options.first_line_offset);
        self.view.tab_indicator = indicator;
        self.view.tab_indicator_color = options.tab_indicator_color;
        self.scroll_x = options.scroll_x;
        self.scroll_y = options.scroll_y;
        viewport.x = options.scroll_x as u32;
        viewport.y = options.scroll_y as u32;
        if has_viewport {
            if previous.is_some() && wrap_changed && options.wrap_mode != WrapMode::None {
                self.view.set_wrap_width((viewport.width > 0).then_some(viewport.width));
            }
            if previous.is_none() || scroll_changed {
                self.view.set_viewport(Some(viewport));
            }
        } else {
            self.initial_viewport = Some(viewport);
            self.view.set_wrap_width((viewport.width > 0).then_some(viewport.width));
        }
        Ok(OptionsChange { measure_changed, prepare_changed })
    }

    /// The scene's resize hook: the laid-out size in cells becomes the viewport size.
    pub fn apply_layout(&mut self, width: u32, height: u32) {
        let mut viewport = self.view.viewport.or(self.initial_viewport).unwrap_or(Viewport { x: 0, y: 0, width: 0, height: 0 });
        viewport.width = width;
        viewport.height = height;
        self.view.set_viewport(Some(viewport));
        self.view.virtual_lines(&self.tb);
        self.initial_viewport = None;
    }

    /// Whether preparing the view at this layout size would change it (`viewNeedsPrepare`).
    pub fn needs_prepare(&self, width: u32, height: u32) -> bool {
        match self.view.viewport {
            Some(vp) => vp.width != width || vp.height != height || self.view.needs_prepare(&self.tb),
            None => true,
        }
    }

    /// `sceneSetTextSelection`.
    #[allow(clippy::too_many_arguments)]
    pub fn set_selection(
        &mut self,
        operation: SelectionOperation,
        behavior: SelectionBehavior,
        anchor_x: i32,
        anchor_y: i32,
        focus_x: i32,
        focus_y: i32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
    ) -> Result<bool, TextError> {
        if operation == SelectionOperation::Reset {
            let had = self.view.selection.is_some();
            self.view.reset_local_selection();
            return Ok(had);
        }
        if let Some(vp) = self.view.viewport {
            let x = if self.view.wrap_mode == WrapMode::None { vp.x as i32 } else { 0 };
            let y = vp.y as i32;
            for (value, offset) in [(anchor_x, x), (focus_x, x), (anchor_y, y), (focus_y, y)] {
                value.checked_add(offset).ok_or(TextError::InvalidOptions)?;
            }
        }
        if self.tb.line_width_cols_max() > i32::MAX as u32 {
            return Err(TextError::InvalidOptions);
        }
        self.view.virtual_lines(&self.tb);
        if self.view.virtual_lines_all().len() > i32::MAX as usize {
            return Err(TextError::InvalidOptions);
        }
        Ok(match operation {
            SelectionOperation::Set => {
                self.view.set_local_selection(&self.tb, anchor_x, anchor_y, focus_x, focus_y, bg, fg, behavior)
            }
            _ => self.view.update_local_selection(&self.tb, anchor_x, anchor_y, focus_x, focus_y, bg, fg, behavior),
        })
    }

    /// `sceneGetTextSelection`: `None` for no or an empty selection.
    pub fn selection(&self) -> Option<(u32, u32)> {
        self.view.selection_info()
    }

    pub fn selected_text(&self) -> String {
        decode_text(self.view.selected_text(&self.tb))
    }

    /// `sceneGetText`.
    pub fn text(&self) -> String {
        decode_text(self.tb.plain_text())
    }

    /// `sceneGetTextInfo`.
    pub fn metrics(&mut self) -> SceneTextMetrics {
        self.view.virtual_lines(&self.tb);
        SceneTextMetrics {
            byte_length: self.tb.byte_size(),
            text_length: self.tb.length(),
            line_count: self.tb.line_count(),
            virtual_line_count: self.view.virtual_lines_all().len() as u32,
            width_cols_max: self.tb.line_width_cols_max(),
        }
    }

    /// `getTextLineInfo`: `count` visual lines from `first`, with the document's widest line.
    pub fn line_info(&mut self, first: u32, count: u32) -> LineInfo {
        self.view.virtual_lines(&self.tb);
        self.view.logical_line_info_window(&self.tb, first as usize, count as usize)
    }

    pub fn measure_for_dimensions(&mut self, width: u32) -> MeasureResult {
        self.view.measure_for_dimensions(&self.tb, width)
    }

    /// Yoga's measure function for the node (`NativeRenderable.measure`).
    pub fn yoga_measure(&mut self, width: f32, width_mode: u32, height: f32, absolute: bool) -> (f32, f32) {
        crate::text::yoga_measure_with(width, width_mode, height, absolute, |w| self.view.measure_for_dimensions(&self.tb, w))
    }

    pub fn viewport(&self) -> Option<Viewport> {
        self.view.viewport
    }

    pub fn draw<R>(&mut self, draw_y: i32, f: impl FnOnce(&DrawSource) -> R) -> R {
        self.view.set_draw_y(draw_y);
        self.view.virtual_lines(&self.tb);
        f(&DrawSource { tb: &self.tb, view: &self.view, default_bg_fill: None })
    }
}
