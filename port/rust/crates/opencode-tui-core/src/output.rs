//! Diffed frame output: `renderer.zig`'s `CliRenderer` (double buffer, the frame encoder
//! `prepareRenderFrameWithWriter`, color emission, cursor and pointer state, resize, palette state) with
//! `renderer-output.zig`/`stdout-writer.zig`'s framing, and the scene's hit rectangles (`scene.zig`: `Hit`,
//! `topmostHit`, presented hits).
//!
//! [`Renderer::render`] diffs the next buffer against the current one and appends the frame's bytes to a
//! caller-owned `Vec`, exactly as the native renderer writes them: a frame opens with synchronized output
//! (`?2026h`, when the terminal decision says so) and a hidden cursor, writes each changed run as a move, its
//! colors and attributes, and its text, closes with `\x1b[0m`, restores the cursor (style, color, position)
//! and the mouse pointer (OSC 22) when they changed, and closes synchronized output. A frame with nothing to
//! change writes nothing. After a frame the next buffer is cleared to the background color, as the renderer
//! does for frames it does not retain (the TS scene retains its frames for damage tracking, which changes which
//! rows it repaints, never the bytes: an unchanged row writes nothing either way).
//!
//! Colors follow the terminal's capabilities: a default-intent color is `39`/`49`, a transparent background
//! `49`, an indexed color `38;5;n` on a 256-color terminal, a literal color its nearest palette entry where the
//! terminal has 256 colors but no true color, and `38;2;r;g;b` otherwise.
//!
//! Divergences: image placements (kitty, sixel), the split-footer/scrollback modes, the debug overlay and the
//! output backend's backpressure (frames skipped while the output queue is full, frames dropped that exceed it)
//! are not ported: the TUI renders on the alternate screen without images, and the caller writes each frame's
//! bytes to the terminal itself.
use std::collections::HashMap;

use crate::buffer::{Cell, OptimizedBuffer};
use crate::color::{self, ColorIntent, RGBA, base_attributes, get_link_id, text_attributes as attr};
use crate::pool;
use crate::terminal::{CursorStyle, MousePointerStyle, Terminal};
use crate::unicode::WidthMethod;

/// The char the current buffer starts with and returns to on resize, so the first frame writes every cell
/// (`CLEAR_CHAR`).
const CLEAR_CHAR: u32 = 0x0a00;

/// What one render did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Cells written (`cellsUpdated`).
    pub cells_updated: u32,
    /// Whether the frame wrote anything.
    pub wrote: bool,
}

/// A hit rectangle: the cells a renderable covers, by paint order (`Hit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub id: u32,
}

/// The renderer's output state (`CliRenderer`), with the terminal it writes for.
pub struct Renderer {
    width: u32,
    height: u32,
    current: OptimizedBuffer,
    next: OptimizedBuffer,
    background_color: RGBA,
    render_offset: u32,
    /// The terminal's capabilities, modes, cursor and pointer (`terminal`).
    pub terminal: Terminal,
    palette: [RGBA; 256],
    default_fg: RGBA,
    default_bg: RGBA,
    palette_epoch: u32,
    last_rendered_palette_epoch: Option<u32>,
    palette_index_cache: HashMap<u64, u8>,
    force_full_repaint: bool,
    last_cursor_style: Option<CursorStyle>,
    last_cursor_blinking: Option<bool>,
    last_cursor_color: Option<[u8; 3]>,
    last_cursor_x: Option<u32>,
    last_cursor_y: Option<u32>,
    last_cursor_visible: Option<bool>,
    last_mouse_pointer: MousePointerStyle,
    mouse_pointer_valid: bool,
    hits: Vec<Hit>,
    presented_hits: Vec<Hit>,
    frame_count: u64,
}

impl Renderer {
    /// `CliRenderer.createWithOptions`: buffers of the given size, a transparent background, the standard
    /// palette, and a terminal configured from `env` (the variables `checkEnvironmentOverrides` reads).
    pub fn new(width: u32, height: u32, terminal: Terminal) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let mut current = OptimizedBuffer::new(width, height, false, WidthMethod::Unicode);
        current.id = "current buffer".to_owned();
        let mut next = OptimizedBuffer::new(width, height, false, WidthMethod::Unicode);
        next.id = "next buffer".to_owned();
        let background = RGBA::rgb(0, 0, 0, 0);
        let mut renderer = Renderer {
            width,
            height,
            current,
            next,
            background_color: background,
            render_offset: 0,
            terminal,
            palette: [RGBA::default(); 256],
            default_fg: RGBA::default(),
            default_bg: RGBA::default(),
            palette_epoch: 0,
            last_rendered_palette_epoch: None,
            palette_index_cache: HashMap::new(),
            force_full_repaint: false,
            last_cursor_style: None,
            last_cursor_blinking: None,
            last_cursor_color: None,
            last_cursor_x: None,
            last_cursor_y: None,
            last_cursor_visible: None,
            last_mouse_pointer: MousePointerStyle::Default,
            mouse_pointer_valid: true,
            hits: Vec::new(),
            presented_hits: Vec::new(),
            frame_count: 0,
        };
        renderer.sync_width_method();
        renderer.reset_fallback_palette();
        renderer.next.set_blend_backdrop_color(Some(RGBA::rgb(background.red(), background.green(), background.blue(), 255)));
        renderer.current.clear_with(background, Some(CLEAR_CHAR));
        renderer.next.clear(background);
        renderer
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The buffer the next frame is drawn into (`getNextBuffer`).
    pub fn next_buffer(&mut self) -> &mut OptimizedBuffer {
        &mut self.next
    }

    /// The last frame as the terminal shows it (`getCurrentBuffer`).
    pub fn current_buffer(&self) -> &OptimizedBuffer {
        &self.current
    }

    /// Frames rendered so far.
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// `syncWidthMethod`: both buffers measure text as the terminal does.
    pub fn sync_width_method(&mut self) {
        let method = self.terminal.capabilities().unicode;
        self.current.set_width_method(method);
        self.next.set_width_method(method);
    }

    /// `resize`: both buffers take the new size and clear (the current one so that every cell repaints), and the
    /// cursor stays inside the screen. The same size changes nothing.
    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if width == self.width && height == self.height {
            return;
        }
        self.current.resize(width, height);
        self.next.resize(width, height);
        self.width = width;
        self.height = height;
        let background = self.background_color;
        self.next.set_blend_backdrop_color(Some(RGBA::rgb(background.red(), background.green(), background.blue(), 255)));
        self.current.clear_with(RGBA::rgb(0, 0, 0, 255), Some(CLEAR_CHAR));
        self.next.clear(background);
        self.hits.clear();
        self.presented_hits.clear();
        let cursor = self.terminal.cursor_position();
        self.terminal.set_cursor_position(cursor.x.min(width), cursor.y.min(height), cursor.visible);
    }

    /// `setBackgroundColor`: what the next buffer clears to after each frame, and what translucent colors blend
    /// over (opaque) where nothing is below them.
    pub fn set_background_color(&mut self, color: RGBA) {
        self.background_color = color;
        self.next.set_blend_backdrop_color(Some(RGBA::rgb(color.red(), color.green(), color.blue(), 255)));
    }

    pub fn background_color(&self) -> RGBA {
        self.background_color
    }

    /// `renderOffset`: the first screen row the frame occupies (0 on the alternate screen).
    pub fn set_render_offset(&mut self, offset: u32) {
        self.render_offset = offset;
    }

    fn reset_fallback_palette(&mut self) {
        for (index, slot) in self.palette.iter_mut().enumerate() {
            *slot = color::fallback_ansi256_color(index as u8);
        }
        self.default_fg = RGBA::pack(255, 255, 255, 255, color::pack_meta(ColorIntent::Default, 0));
        self.default_bg = RGBA::pack(0, 0, 0, 255, color::pack_meta(ColorIntent::Default, 0));
    }

    /// `setPaletteState`: the terminal's detected palette (missing entries keep the standard ones). A new epoch
    /// repaints every cell.
    pub fn set_palette_state(&mut self, palette: &[RGBA], default_fg: RGBA, default_bg: RGBA, epoch: u32) {
        self.reset_fallback_palette();
        for (slot, color) in self.palette.iter_mut().zip(palette) {
            *slot = *color;
        }
        self.default_fg = default_fg;
        self.default_bg = default_bg;
        if self.palette_epoch != epoch {
            self.palette_epoch = epoch;
            self.force_full_repaint = true;
            self.palette_index_cache.clear();
        }
    }

    /// `invalidateTerminalState`: the next frame repaints every cell and restores cursor and pointer.
    pub fn invalidate_terminal_state(&mut self) {
        self.force_full_repaint = true;
        self.last_cursor_style = None;
        self.last_cursor_blinking = None;
        self.last_cursor_color = None;
        self.last_cursor_x = None;
        self.last_cursor_y = None;
        self.last_cursor_visible = None;
        self.mouse_pointer_valid = false;
    }

    fn nearest_palette_index(&mut self, rgba: RGBA) -> u8 {
        let key = ((self.palette_epoch as u64) << 24) | rgba.to_rgb24() as u64;
        if let Some(&index) = self.palette_index_cache.get(&key) {
            return index;
        }
        let mut best = 0u8;
        let mut best_distance = f32::INFINITY;
        for (index, candidate) in self.palette.iter().enumerate() {
            let distance = color::color_distance_squared(rgba, *candidate);
            if distance < best_distance {
                best_distance = distance;
                best = index as u8;
            }
        }
        self.palette_index_cache.insert(key, best);
        best
    }

    /// `emitColor`.
    fn emit_color(&mut self, out: &mut Vec<u8>, rgba: RGBA, background: bool) {
        let caps = self.terminal.capabilities();
        let (select, default) = if background { ("48", "\x1b[49m") } else { ("38", "\x1b[39m") };
        if rgba.intent() == ColorIntent::Default || (background && rgba.alpha() == 0) {
            out.extend_from_slice(default.as_bytes());
            return;
        }
        if rgba.intent() == ColorIntent::Indexed && caps.ansi256 {
            push_fmt(out, format_args!("\x1b[{select};5;{}m", rgba.slot()));
            return;
        }
        if !caps.rgb && caps.ansi256 {
            let index = self.nearest_palette_index(rgba);
            push_fmt(out, format_args!("\x1b[{select};5;{index}m"));
            return;
        }
        push_fmt(out, format_args!("\x1b[{select};2;{};{};{}m", rgba.red(), rgba.green(), rgba.blue()));
    }

    fn begin_frame(&self, out: &mut Vec<u8>) {
        if self.terminal.synchronized_output() {
            out.extend_from_slice(b"\x1b[?2026h");
        }
        out.extend_from_slice(b"\x1b[?25l");
    }

    /// `renderFrame` + `prepareRenderFrameWithWriter`: the frame's bytes appended to `out`; `force` writes
    /// every cell. The draft hit rectangles become the presented ones.
    pub fn render(&mut self, force: bool, out: &mut Vec<u8>) -> RenderStats {
        let palette_force = self.last_rendered_palette_epoch != Some(self.palette_epoch);
        let should_force = force || self.force_full_repaint || palette_force;
        let start_len = out.len();
        let mut cells_updated = 0u32;
        let mut frame_started = false;
        let mut current_fg: Option<RGBA> = None;
        let mut current_bg: Option<RGBA> = None;
        let mut current_attributes: Option<u32> = None;
        let mut current_link: u32 = 0;
        let hyperlinks = self.terminal.capabilities().hyperlinks;
        let mut use_row_equality = !should_force;
        let width = self.width as usize;

        for y in 0..self.height {
            let row_start = y as usize * width;
            let row_end = row_start + width;
            if use_row_equality
                && self.current.chars()[row_start..row_end] == self.next.chars()[row_start..row_end]
                && self.current.fg()[row_start..row_end] == self.next.fg()[row_start..row_end]
                && self.current.bg()[row_start..row_end] == self.next.bg()[row_start..row_end]
                && self.current.attributes()[row_start..row_end] == self.next.attributes()[row_start..row_end]
            {
                continue;
            }
            let mut run_length = 0u32;
            let before_row = cells_updated;
            for x in 0..self.width {
                let (Some(current), Some(cell)) = (self.current.get(x, y), self.next.get(x, y)) else { continue };
                let cell_type = cell.char & pool::CHAR_TYPE_MASK;
                if !should_force && current == cell {
                    if run_length > 0 {
                        out.extend_from_slice(b"\x1b[0m");
                        run_length = 0;
                    }
                    continue;
                }
                if cell_type == pool::CHAR_FLAG_CONTINUATION {
                    // A continuation has no bytes; a run starting here would place the next glyph inside the wide one.
                    self.current.sync_cell(x, y, cell);
                    cells_updated += 1;
                    continue;
                }
                if !frame_started {
                    self.begin_frame(out);
                    frame_started = true;
                }
                let same = current_fg == Some(cell.fg) && current_bg == Some(cell.bg) && current_attributes == Some(cell.attributes);
                let link = if hyperlinks { get_link_id(cell.attributes) } else { 0 };
                if hyperlinks && link != current_link {
                    if current_link != 0 {
                        out.extend_from_slice(b"\x1b]8;;\x1b\\");
                    }
                    current_link = link;
                    if link != 0 {
                        match pool::with_links(|links| links.get(link).map(str::to_owned)) {
                            Some(url) => push_fmt(out, format_args!("\x1b]8;id={link};{url}\x1b\\")),
                            None => current_link = 0,
                        }
                    }
                }
                if !same || run_length == 0 {
                    if run_length > 0 {
                        out.extend_from_slice(b"\x1b[0m");
                    }
                    run_length = 0;
                    current_fg = Some(cell.fg);
                    current_bg = Some(cell.bg);
                    current_attributes = Some(cell.attributes);
                    push_fmt(out, format_args!("\x1b[{};{}H", y + 1 + self.render_offset, x + 1));
                    self.emit_color(out, cell.fg, false);
                    self.emit_color(out, cell.bg, true);
                    apply_attributes(out, cell.attributes);
                }
                match cell_type {
                    pool::CHAR_FLAG_GRAPHEME => {
                        let bytes = pool::grapheme_bytes(pool::grapheme_id_from_char(cell.char)).unwrap_or_default();
                        if !bytes.is_empty() {
                            let caps = self.terminal.capabilities();
                            let grapheme_width = pool::char_right_extent(cell.char) + 1;
                            if caps.explicit_width {
                                // OSC 66 takes w up to 7; spaces fill the rest of the glyph.
                                push_fmt(out, format_args!("\x1b]66;w={};", grapheme_width.min(7)));
                                out.extend_from_slice(&bytes);
                                out.extend_from_slice(b"\x1b\\");
                                for _ in 7..grapheme_width.max(7) {
                                    out.push(b' ');
                                }
                            } else {
                                out.extend_from_slice(&bytes);
                                if caps.explicit_cursor_positioning {
                                    let next_x = x + grapheme_width;
                                    if next_x < self.width {
                                        push_fmt(out, format_args!("\x1b[{};{}H", y + 1 + self.render_offset, next_x + 1));
                                    }
                                }
                            }
                        }
                    }
                    pool::CHAR_FLAG_IMAGE => {}
                    _ => {
                        if (32..=126).contains(&cell.char) {
                            out.push(cell.char as u8);
                        } else {
                            let mut utf8 = [0u8; 4];
                            match char::from_u32(cell.char) {
                                Some(c) => out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes()),
                                // utf8Encode fails on a surrogate or out-of-range value; the fallback length 1
                                // writes the buffer's first, zeroed byte.
                                None => out.push(0),
                            }
                        }
                    }
                }
                run_length += 1;
                self.current.sync_cell(x, y, cell);
                cells_updated += 1;
            }
            // A new run sets colors but only turns attributes on, so an attributed run closes here.
            if run_length > 0 && base_attributes(current_attributes.unwrap_or(0)) != 0 {
                out.extend_from_slice(b"\x1b[0m");
            }
            if cells_updated - before_row == self.width {
                use_row_equality = false;
            }
        }

        if hyperlinks && current_link != 0 {
            if !frame_started {
                self.begin_frame(out);
                frame_started = true;
            }
            out.extend_from_slice(b"\x1b]8;;\x1b\\");
        }
        if frame_started {
            out.extend_from_slice(b"\x1b[0m");
        }

        let cursor = self.terminal.cursor_position();
        let style = self.terminal.cursor_style();
        let cursor_color = self.terminal.cursor_color();
        if cursor.visible {
            let style_code: &[u8] = match (style.style, style.blinking) {
                (CursorStyle::Block, true) => b"\x1b[1 q",
                (CursorStyle::Block, false) => b"\x1b[2 q",
                (CursorStyle::Line, true) => b"\x1b[5 q",
                (CursorStyle::Line, false) => b"\x1b[6 q",
                (CursorStyle::Underline, true) => b"\x1b[3 q",
                (CursorStyle::Underline, false) => b"\x1b[4 q",
                (CursorStyle::Default, _) => b"\x1b[0 q",
            };
            let rgb = [cursor_color.red(), cursor_color.green(), cursor_color.blue()];
            let style_changed = self.last_cursor_style != Some(style.style) || self.last_cursor_blinking != Some(style.blinking);
            let color_changed = self.last_cursor_color != Some(rgb);
            let cursor_x = cursor.x;
            let cursor_y = cursor.y + self.render_offset;
            let position_changed = self.last_cursor_x != Some(cursor_x) || self.last_cursor_y != Some(cursor_y);
            let visibility_changed = self.last_cursor_visible != Some(true);
            if frame_started || style_changed || color_changed || position_changed || visibility_changed {
                if !frame_started {
                    self.begin_frame(out);
                    frame_started = true;
                }
                if color_changed {
                    push_fmt(out, format_args!("\x1b]12;#{:02x}{:02x}{:02x}\x07", rgb[0], rgb[1], rgb[2]));
                    self.last_cursor_color = Some(rgb);
                }
                if style_changed {
                    out.extend_from_slice(style_code);
                    self.last_cursor_style = Some(style.style);
                    self.last_cursor_blinking = Some(style.blinking);
                }
                push_fmt(out, format_args!("\x1b[{cursor_y};{cursor_x}H"));
                out.extend_from_slice(b"\x1b[?25h");
            }
            self.last_cursor_x = Some(cursor_x);
            self.last_cursor_y = Some(cursor_y);
            self.last_cursor_visible = Some(true);
        } else {
            if !frame_started && self.last_cursor_visible != Some(false) {
                self.begin_frame(out);
                frame_started = true;
                out.extend_from_slice(b"\x1b[?25l");
            }
            self.last_cursor_style = None;
            self.last_cursor_blinking = None;
            self.last_cursor_color = None;
            self.last_cursor_x = None;
            self.last_cursor_y = None;
            self.last_cursor_visible = Some(false);
        }

        let pointer = self.terminal.mouse_pointer();
        if !self.mouse_pointer_valid || pointer != self.last_mouse_pointer {
            if !frame_started {
                self.begin_frame(out);
                frame_started = true;
            }
            push_fmt(out, format_args!("\x1b]22;{}\x1b\\", pointer.name()));
            self.last_mouse_pointer = pointer;
            self.mouse_pointer_valid = true;
        }
        if frame_started && self.terminal.synchronized_output() {
            out.extend_from_slice(b"\x1b[?2026l");
        }

        self.last_rendered_palette_epoch = Some(self.palette_epoch);
        self.force_full_repaint = false;
        self.frame_count += 1;
        std::mem::swap(&mut self.hits, &mut self.presented_hits);
        self.hits.clear();
        let background = self.background_color;
        self.next.clear(background);
        RenderStats { cells_updated, wrote: out.len() > start_len }
    }

    // ---- hit rectangles ----

    /// Adds a hit rectangle to the frame being drawn (cells outside the screen are dropped); a later one covers
    /// an earlier one, as a grid every member overwrote in paint order would.
    pub fn add_to_hit_grid(&mut self, x: i32, y: i32, width: u32, height: u32, id: u32) {
        let x0 = (x as i64).max(0);
        let y0 = (y as i64).max(0);
        let x1 = (x as i64 + width as i64).min(self.width as i64);
        let y1 = (y as i64 + height as i64).min(self.height as i64);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        self.hits.push(Hit { x: x0 as u32, y: y0 as u32, width: (x1 - x0) as u32, height: (y1 - y0) as u32, id });
    }

    /// The id of the topmost presented hit rectangle at a cell, or 0 (`hitToken` / `checkHit`).
    pub fn check_hit(&self, x: u32, y: u32) -> u32 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.presented_hits
            .iter()
            .rev()
            .find(|hit| x >= hit.x && y >= hit.y && x - hit.x < hit.width && y - hit.y < hit.height)
            .map_or(0, |hit| hit.id)
    }

    /// The presented hit rectangles, in paint order.
    pub fn presented_hits(&self) -> &[Hit] {
        &self.presented_hits
    }

    /// The link URL of the cell at a position (`getLinkAt`), from the presented frame.
    pub fn link_at(&self, x: u32, y: u32) -> Option<String> {
        let cell: Cell = self.current.get(x, y)?;
        let link = get_link_id(cell.attributes);
        if link == 0 {
            return None;
        }
        pool::with_links(|links| links.get(link).map(str::to_owned))
    }
}

/// `applyAttributesOutputWriter`.
fn apply_attributes(out: &mut Vec<u8>, attributes: u32) {
    let base = base_attributes(attributes);
    for (bit, code) in [
        (attr::BOLD, &b"\x1b[1m"[..]),
        (attr::DIM, b"\x1b[2m"),
        (attr::ITALIC, b"\x1b[3m"),
        (attr::UNDERLINE, b"\x1b[4m"),
        (attr::BLINK, b"\x1b[5m"),
        (attr::INVERSE, b"\x1b[7m"),
        (attr::HIDDEN, b"\x1b[8m"),
        (attr::STRIKETHROUGH, b"\x1b[9m"),
    ] {
        if base & bit != 0 {
            out.extend_from_slice(code);
        }
    }
}

pub(crate) fn push_fmt(out: &mut Vec<u8>, args: std::fmt::Arguments) {
    use std::io::Write;
    let _ = out.write_fmt(args);
}
