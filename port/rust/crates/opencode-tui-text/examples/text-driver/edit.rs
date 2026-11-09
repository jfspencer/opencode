//! The edit buffer and editor view ops of harness/ts-exec.ts (`editOp`).

use crate::json::{self, Value};
use crate::{behavior, color, draw, error, highlight, line_info, spans, spec, unit, wrap_mode};
use opencode_tui_text::{
    measure_edit_text, Extmark, ExtmarkOptions, EditBuffer, EditEvent, EditorView, LogicalCursor, SelectionOccupancy, SyntaxStyle, TextContext,
    TextError, VisualCursor, WidthMethod,
};
use std::any::Any;
use std::fmt::Write as _;
use std::rc::Rc;

pub struct EditState {
    pub ctx: TextContext,
    pub eb: EditBuffer,
    pub view: EditorView,
    pub style: SyntaxStyle,
    pub method: WidthMethod,
    pub events: Vec<EditEvent>,
}

fn visual(out: &mut String, v: VisualCursor) {
    write!(out, "[{},{},{},{},{}]", v.visual_row, v.visual_col, v.logical_row, v.logical_col, v.offset).unwrap();
}

fn logical(out: &mut String, c: LogicalCursor) {
    write!(out, "[{},{},{}]", c.row, c.col, c.offset).unwrap();
}

fn mark(out: &mut String, e: &Extmark) {
    let opt = |v: Option<u32>| v.map_or("null".to_owned(), |n| n.to_string());
    write!(out, "[{},{},{},{},{},{},{}]", e.id, e.start, e.end, e.virtual_, opt(e.style_id), opt(e.priority), e.type_id).unwrap();
}

fn marks(out: &mut String, list: Vec<Extmark>) {
    out.push('[');
    for (i, e) in list.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        mark(out, e);
    }
    out.push(']');
}

fn null(out: &mut String) {
    out.push_str("null");
}

fn any_num(out: &mut String, v: Option<Rc<dyn Any>>) {
    match v.as_ref().and_then(|v| v.downcast_ref::<f64>()) {
        // The harness only stores integers.
        Some(n) => write!(out, "{}", *n as i64).unwrap(),
        None => null(out),
    }
}

fn opt_str(out: &mut String, s: Option<String>) {
    match s {
        Some(s) => json::write_str(out, &s),
        None => null(out),
    }
}

fn result<T>(out: &mut String, r: Result<T, TextError>, f: impl FnOnce(&mut String, T)) {
    match r {
        Ok(v) => f(out, v),
        Err(e) => error(out, e),
    }
}

pub fn edit_op(s: &mut EditState, op: &[Value], out: &mut String) {
    let name = op[0].str();
    let eb = &s.eb;
    let v = &s.view;
    match name {
        "eSetText" => unit(out, eb.set_text(op[1].str())),
        "eReplaceText" => unit(out, eb.replace_text(op[1].str())),
        "eInsert" => unit(out, eb.insert_text(op[1].str())),
        "eDelChar" => unit(out, eb.delete_char()),
        "eBackspace" => unit(out, eb.delete_char_backward()),
        "eDelRange" => unit(out, eb.delete_range(op[1].u32(), op[2].u32(), op[3].u32(), op[4].u32())),
        "eNewLine" => unit(out, eb.new_line()),
        "eDelLine" => unit(out, eb.delete_line()),
        "eLeft" => {
            eb.move_cursor_left();
            null(out)
        }
        "eRight" => {
            eb.move_cursor_right();
            null(out)
        }
        "eUp" => {
            eb.move_cursor_up();
            null(out)
        }
        "eDown" => {
            eb.move_cursor_down();
            null(out)
        }
        "eGotoLine" => {
            eb.goto_line(op[1].u32());
            null(out)
        }
        "eSetCursor" => {
            eb.set_cursor(op[1].u32(), op[2].u32());
            null(out)
        }
        "eCursorOffset" => {
            eb.set_cursor_by_offset(op[1].u32());
            null(out)
        }
        "eTabWidth" => unit(out, eb.set_tab_width(op[1].u32())),
        "eUndo" => opt_str(out, eb.undo()),
        "eRedo" => opt_str(out, eb.redo()),
        "eClear" => {
            eb.clear();
            null(out)
        }
        "eClearHistory" => {
            eb.clear_history();
            null(out)
        }
        "eInfo" => {
            let c = eb.get_cursor_position();
            write!(out, "[{},{},[{},{},{}],{},{}]", eb.get_line_count(), eb.get_tab_width(), c.row, c.col, c.offset, eb.can_undo(), eb.can_redo())
                .unwrap();
        }
        "eText" => json::write_str(out, &eb.get_text()),
        "eRange" => json::write_str(out, &eb.get_text_range(op[1].u32(), op[2].u32())),
        "eRangeCoords" => json::write_str(out, &eb.get_text_range_by_coords(op[1].u32(), op[2].u32(), op[3].u32(), op[4].u32())),
        "eNextWord" => logical(out, eb.get_next_word_boundary()),
        "ePrevWord" => logical(out, eb.get_prev_word_boundary()),
        "eEol" => logical(out, eb.get_eol()),
        "eEof" => logical(out, eb.get_eof()),
        "ePrevChar" => logical(out, eb.get_prev_char_boundary()),
        "eOffsetToPos" => match eb.offset_to_position(op[1].u32()) {
            Some((r, c)) => write!(out, "[{r},{c}]").unwrap(),
            None => null(out),
        },
        "ePosToOffset" => write!(out, "{}", eb.position_to_offset(op[1].u32(), op[2].u32())).unwrap(),
        "eLineStart" => write!(out, "{}", eb.get_line_start_offset(op[1].u32())).unwrap(),
        "eMeasure" => result(out, eb.measure_text(op[1].str()), |out, n| write!(out, "{n}").unwrap()),
        "eMeasureStatic" => result(out, measure_edit_text(op[1].str(), s.method, op[2].u32()), |out, n| write!(out, "{n}").unwrap()),
        "eAddHl" => unit(out, eb.add_highlight(op[1].u32(), highlight(&op[2]))),
        "eAddHlRange" => unit(out, eb.add_highlight_by_char_range(highlight(&op[1]))),
        "eRemoveRef" => unit(out, eb.remove_highlights_by_ref(op[1].u32())),
        "eHl" => {
            out.push('[');
            for (i, h) in eb.get_line_highlights(op[1].u32()).iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write!(out, "[{},{},{},{},{}]", h.start, h.end, h.style_id, h.priority, h.hl_ref).unwrap();
            }
            out.push(']');
        }
        "eDefaults" => {
            let a = eb.set_default_fg(color(op.get(1)));
            let b = eb.set_default_bg(color(op.get(2)));
            let c = eb.set_default_attributes(if op[3].is_null() { None } else { Some(op[3].u32()) });
            unit(out, a.and(b).and(c))
        }
        "eSyntaxStyle" => {
            eb.set_syntax_style(if op[1].bool() { Some(&s.style) } else { None });
            null(out)
        }
        "registerStyle" => {
            let id = s.style.register_style(op[1].str(), spec(&op[2]));
            write!(out, "{id}").unwrap();
        }
        "eEvents" => {
            // Already delivered: after every op, see `run_edit_case`.
            out.push('[');
            for (i, e) in s.events.drain(..).enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write!(out, "\"{}\"", e.name()).unwrap();
            }
            out.push(']');
        }
        "vViewportSize" => unit(out, v.set_viewport_size(op[1].u32(), op[2].u32())),
        "vViewport" => unit(out, v.set_viewport(op[1].u32(), op[2].u32(), op[3].u32(), op[4].u32(), op[5].bool())),
        "vGetViewport" => {
            let vp = v.get_viewport();
            write!(out, "[{},{},{},{}]", vp.x, vp.y, vp.width, vp.height).unwrap();
        }
        "vScrollMargin" => unit(out, v.set_scroll_margin(op[1].num() as f32)),
        "vWrapMode" => {
            v.set_wrap_mode(wrap_mode(op[1].str()));
            null(out)
        }
        "vVcount" => write!(out, "{}", v.get_virtual_line_count()).unwrap(),
        "vTotal" => write!(out, "{}", v.get_total_virtual_line_count()).unwrap(),
        "vSel" => {
            v.set_selection(op[1].u32(), op[2].u32(), None, None);
            null(out)
        }
        "vUSel" => {
            v.update_selection(op[1].u32(), None, None);
            null(out)
        }
        "vRSel" => {
            v.reset_selection();
            null(out)
        }
        "vLSel" | "vULSel" => {
            let f = if name == "vLSel" { EditorView::set_local_selection } else { EditorView::update_local_selection };
            result(
                out,
                f(v, op[1].i32(), op[2].i32(), op[3].i32(), op[4].i32(), None, None, op[5].bool(), op[6].bool(), behavior(op[7].str())),
                |out, b| write!(out, "{b}").unwrap(),
            );
        }
        "vRLSel" => {
            v.reset_local_selection();
            null(out)
        }
        "vCell" => write!(out, "{}", v.convert_selection_to_cell()).unwrap(),
        "vOcc" => {
            v.set_selection_occupancy(if op[1].str() == "boundary" { SelectionOccupancy::Boundary } else { SelectionOccupancy::Cell });
            null(out)
        }
        "vSelInclusive" => {
            v.set_selection_inclusive(op[1].u32(), op[2].u32(), None, None);
            null(out)
        }
        "vSelinfo" => {
            out.push('[');
            match v.get_selection() {
                Some((a, b)) => write!(out, "[{a},{b}]").unwrap(),
                None => null(out),
            }
            out.push(',');
            json::write_str(out, &v.get_selected_text());
            let occ = match v.get_selection_occupancy() {
                SelectionOccupancy::Cell => "cell",
                SelectionOccupancy::Boundary => "boundary",
            };
            write!(out, ",\"{occ}\"]").unwrap();
        }
        "vCursor" => {
            let (r, c) = v.get_cursor();
            write!(out, "[{r},{c}]").unwrap();
        }
        "vVisual" => visual(out, v.get_visual_cursor()),
        "vUp" => {
            v.move_up_visual();
            null(out)
        }
        "vDown" => {
            v.move_down_visual();
            null(out)
        }
        "vDelSel" => unit(out, v.delete_selected_text()),
        "vReplaceSel" => result(out, v.replace_selected_text(op[1].str()), |out, r| {
            write!(out, "{{\"deleted\":{},\"inserted\":{}}}", r.deleted, r.inserted).unwrap()
        }),
        "vCursorOffset" => {
            v.set_cursor_by_offset(op[1].u32());
            null(out)
        }
        "vNextWord" => visual(out, v.get_next_word_boundary()),
        "vPrevWord" => visual(out, v.get_prev_word_boundary()),
        "vEol" => visual(out, v.get_eol()),
        "vSol" => visual(out, v.get_visual_sol()),
        "vVEol" => visual(out, v.get_visual_eol()),
        "vGotoEnd" => {
            v.goto_visual_line_end();
            null(out)
        }
        "vLineInfo" => line_info(out, &v.get_line_info()),
        "vLogical" => line_info(out, &v.get_logical_line_info()),
        "vPlaceholder" => {
            let chunks: Vec<(&str, Option<[u16; 4]>, Option<[u16; 4]>, u32)> = op[1]
                .arr()
                .iter()
                .map(|c| (c.get("text").unwrap().str(), color(c.get("fg")), color(c.get("bg")), c.get("attributes").map_or(0, |a| a.u32())))
                .collect();
            unit(out, v.set_placeholder_styled_text(&chunks));
        }
        "vTabIndicator" => unit(out, v.set_tab_indicator(op[1].u32())),
        "vMeasure" => result(out, v.measure_for_dimensions(op[1].u32(), op[2].u32()), |out, m| {
            write!(out, "[{},{}]", m.line_count, m.width_cols_max).unwrap()
        }),
        "vDraw" => {
            let (bw, bh, x, y) = (op[1].u32(), op[2].u32(), op[3].i32(), op[4].i32());
            let mut grid = draw::Grid::new(bw, bh, [0, 0, 0, 255]);
            // `drawEditorViewOn` returns before touching the view when the origin is outside the target.
            if x < bw as i32 && y < bh as i32 {
                v.draw(y, |src| draw::draw(src, &mut grid, x, y));
            }
            spans(out, &grid);
        }
        "xCreate" => {
            let o = &op[1];
            let num = |k: &str| o.get(k).filter(|v| !v.is_null()).map(|v| v.num());
            let options = ExtmarkOptions {
                start: num("start").unwrap() as i64,
                end: num("end").unwrap() as i64,
                virtual_: o.get("virtual").is_some_and(|v| v.bool()),
                style_id: num("styleId").map(|n| n as u32),
                priority: num("priority").map(|n| n as u32),
                type_id: num("typeId").map(|n| n as u32),
                data: num("data").map(|n| Rc::new(n) as Rc<dyn Any>),
                metadata: num("meta").map(|n| Rc::new(n) as Rc<dyn Any>),
            };
            result(out, v.extmarks().create(options), |out, id| write!(out, "{id}").unwrap());
        }
        "xMeta" => any_num(out, v.extmarks().get_metadata_for(op[1].u32())),
        "xData" => any_num(out, v.extmarks().get(op[1].u32()).and_then(|e| e.data)),
        "xAdjustDel" => {
            v.extmarks().adjust_extmarks_after_deletion(op[1].num() as i64, op[2].num() as i64, op[3].bool());
            null(out)
        }
        "xDelete" => result(out, v.extmarks().delete(op[1].u32()), |out, b| write!(out, "{b}").unwrap()),
        "xGet" => match v.extmarks().get(op[1].u32()) {
            Some(e) => mark(out, &e),
            None => null(out),
        },
        "xAll" => marks(out, v.extmarks().get_all()),
        "xVirtual" => marks(out, v.extmarks().get_virtual()),
        "xAtOffset" => marks(out, v.extmarks().get_at_offset(op[1].num() as i64)),
        "xForType" => marks(out, v.extmarks().get_all_for_type_id(op[1].u32())),
        "xClear" => {
            v.extmarks().clear();
            null(out)
        }
        "xRegisterType" => result(out, v.extmarks().register_type(op[1].str()), |out, id| write!(out, "{id}").unwrap()),
        "xTypeId" => match v.extmarks().get_type_id(op[1].str()) {
            Some(id) => write!(out, "{id}").unwrap(),
            None => null(out),
        },
        "xTypeName" => opt_str(out, v.extmarks().get_type_name(op[1].u32())),
        "xDestroy" => {
            v.extmarks().destroy();
            null(out)
        }
        "vNewView" => {
            s.view = EditorView::create(&s.eb, op[1].u32(), op[2].u32()).unwrap();
            null(out)
        }
        _ => panic!("unknown op {name}"),
    }
    let _ = &s.ctx;
}

pub fn run_edit_case(case: &Value, out: &mut String) {
    let method = WidthMethod::parse(case.get("m").unwrap().str()).unwrap();
    let ctx = TextContext::new();
    let eb = EditBuffer::create(method, &ctx);
    let view = EditorView::create(&eb, case.get("w").unwrap().u32(), case.get("h").unwrap().u32()).unwrap();
    let mut state = EditState { ctx, eb, view, style: SyntaxStyle::create(), method, events: Vec::new() };
    out.push('[');
    for (i, op) in case.get("ops").unwrap().arr().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        edit_op(&mut state, op.arr(), out);
        // TS delivers events (and the extmarks content-changed listener) in microtasks, which run at the
        // harness's `await` after every op.
        let events = &mut state.events;
        state.eb.dispatch_events(|e| events.push(e));
    }
    out.push(']');
}
