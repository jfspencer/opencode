//! The addons the TUI registers (`addons/universal/*`, `addons/opentui/base-layout.ts`): the default keys, the
//! `enabled` and metadata fields, comma bindings, Escape and Backspace on a pending sequence, the base-layout
//! fallback, and the (timed) leader.
use super::*;
use std::cell::Cell;

/// `registerDefaultKeys`: the default binding parser and event matcher.
pub fn register_default_keys<T: Clone + PartialEq + 'static>(keymap: &Keymap<T>) -> Dispose {
    let off_parser = keymap.append_binding_parser(Rc::new(default_binding_parser));
    let off_resolver = keymap.append_event_match_resolver(Rc::new(default_event_match_resolver));
    Box::new(move || {
        off_resolver();
        off_parser();
    })
}

/// `defaultEventMatchResolver`: the event's own stroke.
pub fn default_event_match_resolver(
    event: &dyn KeymapEvent,
    resolve_key: &dyn Fn(&KeyStrokeInput) -> Result<KeyMatch, String>,
) -> Result<Vec<KeyMatch>, String> {
    Ok(vec![resolve_key(&super::event_stroke_input(event))?])
}

fn enabled_compiler() -> FieldCompiler {
    Rc::new(|value, ctx| match value {
        Field::Bool(true) => Ok(()),
        Field::Bool(false) => {
            ctx.active_when(Rc::new(|| false));
            Ok(())
        }
        Field::Fn(matcher) => {
            ctx.active_when(matcher.clone());
            Ok(())
        }
        _ => Err("Keymap enabled field \"enabled\" must be a boolean, a function, or a reactive matcher".to_owned()),
    })
}

/// `registerEnabledFields`: `enabled` on layers and commands.
pub fn register_enabled_fields<T: Clone + PartialEq + 'static>(keymap: &Keymap<T>) -> Dispose {
    let target = keymap.clone();
    keymap.acquire_resource("keymap:enabled-fields", move || {
        let off_layer = target.register_layer_fields(vec![("enabled", enabled_compiler())]);
        let off_command = target.register_command_fields(vec![("enabled", enabled_compiler())]);
        Box::new(move || {
            off_command();
            off_layer();
        })
    })
}

fn metadata_compiler(name: &'static str) -> FieldCompiler {
    Rc::new(move |value, ctx| {
        let Field::Str(text) = value else {
            return Err(format!("Keymap metadata field \"{name}\" must be a string"));
        };
        let trimmed = js_trim(text);
        if trimmed.is_empty() {
            return Err(format!("Keymap metadata field \"{name}\" cannot be empty"));
        }
        ctx.attr(name, trimmed.to_owned())
    })
}

/// `registerMetadataFields`: `desc`/`group` on bindings, `desc`/`title`/`category` on commands, as attrs.
pub fn register_metadata_fields<T: Clone + PartialEq + 'static>(keymap: &Keymap<T>) -> Dispose {
    let target = keymap.clone();
    keymap.acquire_resource("keymap:metadata-fields", move || {
        let off_binding = target
            .register_binding_fields(vec![("desc", metadata_compiler("desc")), ("group", metadata_compiler("group"))]);
        let off_command = target.register_command_fields(vec![
            ("desc", metadata_compiler("desc")),
            ("title", metadata_compiler("title")),
            ("category", metadata_compiler("category")),
        ]);
        Box::new(move || {
            off_command();
            off_binding();
        })
    })
}

/// `commaBindingExpander`: `j,k` is two bindings.
pub fn comma_binding_expander(ctx: &ExpanderContext<'_>) -> Result<Option<Vec<Expansion>>, String> {
    if !ctx.input.contains(',') {
        return Ok(None);
    }
    let parts: Vec<&str> = ctx.input.split(',').map(js_trim).collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(format!(
            "Invalid key sequence \"{}\": comma-separated bindings cannot contain empty entries",
            ctx.input
        ));
    }
    let plain = || parts.iter().map(|key| Expansion { key: (*key).to_owned(), displays: None }).collect();
    let Some(displays) = ctx.displays else { return Ok(Some(plain())) };
    let display_parts: Vec<String> = if displays.len() == 1 {
        displays[0].split(',').map(|part| js_trim(part).to_owned()).collect()
    } else {
        displays.to_vec()
    };
    if display_parts.len() != parts.len() || display_parts.iter().any(String::is_empty) {
        return Ok(Some(plain()));
    }
    Ok(Some(
        parts
            .iter()
            .zip(&display_parts)
            .map(|(key, display)| Expansion {
                key: (*key).to_owned(),
                displays: Some(
                    js_trim(display)
                        .split(crate::js::is_js_space)
                        .filter(|piece| !piece.is_empty())
                        .map(str::to_owned)
                        .collect(),
                ),
            })
            .collect(),
    ))
}

/// `registerCommaBindings`.
pub fn register_comma_bindings<T: Clone + PartialEq + 'static>(keymap: &Keymap<T>) -> Dispose {
    let target = keymap.clone();
    keymap.acquire_resource("keymap:comma-bindings", move || {
        target.append_binding_expander(Rc::new(comma_binding_expander))
    })
}

/// `registerEscapeClearsPendingSequence`: Escape cancels a pending sequence (and is consumed unless
/// `prevent_default` is false).
pub fn register_escape_clears_pending_sequence<T: Clone + PartialEq + 'static>(
    keymap: &Keymap<T>,
    prevent_default: bool,
    priority: i32,
) -> Dispose {
    let target = keymap.clone();
    keymap.intercept_key(
        Rc::new(move |ctx| {
            if ctx.event.name() != "escape" || !target.has_pending_sequence() {
                return;
            }
            target.clear_pending_sequence();
            if prevent_default {
                ctx.consume(true, true);
            }
        }),
        priority,
        false,
    )
}

/// `registerBackspacePopsPendingSequence`: Backspace steps back through a pending sequence.
pub fn register_backspace_pops_pending_sequence<T: Clone + PartialEq + 'static>(
    keymap: &Keymap<T>,
    prevent_default: bool,
    priority: i32,
) -> Dispose {
    let target = keymap.clone();
    keymap.intercept_key(
        Rc::new(move |ctx| {
            if ctx.event.name() != "backspace" || !target.pop_pending_sequence() {
                return;
            }
            if prevent_default {
                ctx.consume(true, true);
            }
        }),
        priority,
        false,
    )
}

/// `getBaseLayoutKeyName`: the base-layout key of a typed non-ASCII character, or of a digit base key.
pub fn base_layout_key_name(typed: &str, base_code: Option<u32>) -> Option<String> {
    let base = base_code?;
    if base <= 32 || base == 127 {
        return None;
    }
    let mut chars = typed.chars();
    let single_non_ascii = matches!((chars.next(), chars.next()), (Some(c), None) if c as u32 > 0x7f);
    if !single_non_ascii && !(48..=57).contains(&base) {
        return None;
    }
    if base > 0x10ffff {
        return None;
    }
    let name = crate::js::from_code_point(base);
    if name.is_ascii_uppercase() {
        return Some(name.to_ascii_lowercase().to_string());
    }
    Some(name.to_string())
}

/// `registerBaseLayoutFallback`: Korean Ctrl+ㅊ matches `ctrl+c`, AZERTY Ctrl+& matches `ctrl+1`.
pub fn register_base_layout_fallback<T: Clone + PartialEq + 'static>(keymap: &Keymap<T>) -> Dispose {
    let target = keymap.clone();
    keymap.acquire_resource("keymap:base-layout-fallback", move || {
        target.append_event_match_resolver(Rc::new(|event, resolve_key| {
            let Some(name) = base_layout_key_name(event.name(), event.base_code()) else { return Ok(Vec::new()) };
            Ok(vec![resolve_key(&KeyStrokeInput {
                name,
                ctrl: Some(event.ctrl()),
                shift: Some(event.shift()),
                meta: Some(event.meta()),
                super_: Some(event.super_key()),
                hyper: event.hyper().then_some(true),
            })?])
        }))
    })
}

/// `registerLeader`: a `<name>` token (default `leader`) for the trigger key.
pub fn register_leader<T: Clone + PartialEq + 'static>(
    keymap: &Keymap<T>,
    trigger: &KeyLike,
    name: Option<&str>,
) -> Dispose {
    keymap.register_token(name.unwrap_or("leader"), trigger)
}

/// `registerTimedLeader`: the leader token, and a timeout (default 1500 ms, on the host's timers) that clears a
/// pending sequence that began with the trigger, re-armed at each pending-sequence change.
pub fn register_timed_leader<T: Clone + PartialEq + 'static>(
    keymap: &Keymap<T>,
    trigger: &KeyLike,
    name: Option<&str>,
    timeout_ms: Option<f64>,
) -> Result<Dispose, String> {
    let matcher = keymap.create_key_matcher(trigger)?;
    let timeout_ms = timeout_ms.unwrap_or(1500.0);
    let timer: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
    let sync = {
        let keymap = keymap.clone();
        let timer = timer.clone();
        Rc::new(move |sequence: &[KeySequencePart]| {
            if let Some(id) = timer.take() {
                keymap.host().clear_timeout(id);
            }
            if matcher.matches_part(sequence.first()) {
                let target = keymap.clone();
                let fired = timer.clone();
                let id = keymap.host().set_timeout(
                    timeout_ms,
                    Box::new(move || {
                        fired.set(None);
                        target.clear_pending_sequence();
                    }),
                );
                timer.set(Some(id));
            }
        })
    };
    let off_leader = register_leader(keymap, trigger, name);
    let off_pending = keymap.on_pending_sequence(sync.clone());
    sync(&keymap.get_pending_sequence());
    let keymap = keymap.clone();
    Ok(Box::new(move || {
        if let Some(id) = timer.take() {
            keymap.host().clear_timeout(id);
        }
        off_pending();
        off_leader();
    }))
}
