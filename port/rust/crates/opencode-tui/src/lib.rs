//! The opencode TUI (`packages/tui/src`), ported onto `opencode-tui-core`.
//!
//! # Modules
//!
//! - [`app`]: `run` and `<App>` (app.tsx); [`commands`]: App's command registry; [`ctx`]: what the providers give
//!   components; [`home`], [`prompt`], [`local`], [`session`], [`data`]: the home route and what it reads;
//!   [`keymap`], [`interactivity`]: keybinds; [`toast`]: toasts; [`util`]: locale, path and color helpers.
//! - [`theme`]: `@opencode/theme` and `packages/tui/src/theme`; [`config`]: `packages/tui/src/config` (cli.json,
//!   `Config.resolve`, keybind definitions).
//!
//! # Framework guide: porting a `.tsx` file
//!
//! The stack under a component is `opencode-tui-core`: Solid's reactive core ([`opencode_tui_core::reactive`], the
//! same queues and timing as solid-js 1.9.17, checked by its harness), `@opentui/solid`'s insertion
//! ([`opencode_tui_core::solid`]), the renderables ([`opencode_tui_core::elements`], `tree`, `renderables`) and the
//! renderer ([`opencode_tui_core::renderer`]). Port a file top to bottom, keeping its order: the order effects are
//! created in is the order they run in.
//!
//! **A component** is a Rust function returning a [`View`](opencode_tui_core::solid::View). Its body runs once,
//! untracked, like a Solid component; call it through `component(|| my_component(props))` where the JSX does
//! `<MyComponent .../>`. Props are a struct; a prop TS reads reactively (`props.width` behind a getter) is a closure
//! field (`Rc<dyn Fn() -> T>`), a constant one a plain field. `props.children` is a `Rc<dyn Fn() -> View>`.
//!
//! **JSX** becomes the calls the Solid compiler emits (see `opencode_tui_core::solid`'s table):
//! ```ignore
//! // <box flexGrow={1} paddingLeft={narrow() ? 1 : 2}><text fg={theme.text.muted}>{label()}</text></box>
//! let el = box_();
//! let label_el = text();
//! insert_node(el, label_el, None);                         // static children first
//! el.set_flex_grow(Some(1.0));                              // static props: typed setters
//! bind(move || if narrow() { 1.0 } else { 2.0 }, move |p| el.set_padding(Edge::Left, Some(Value::Point(p))));
//! bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| label_el.set_fg(Some(c)));
//! insert(label_el, View::dynamic(move || View::from(label())), At::Only);   // {expr}: a function child
//! View::Node(el)
//! ```
//! - `bind` is the compiler's dynamic prop effect (a render effect that calls the setter when the value changed);
//!   `bind_always` is for values without `PartialEq` (colors).
//! - `insert(parent, view, At::Only | At::End | At::Before(sibling))` is `insert(parent, expr, marker)`: `At::Only`
//!   when the expression is the element's only child (it owns all children), `At::End` when it follows static
//!   siblings (`null` marker), `At::Before(el)` when a static sibling follows it.
//! - Static text in a `<text>` is `insert_node(text_el, text_node_from_string("..."), None)`; a `<span style>` is
//!   `span()` with `set_span_style(fg, bg, attributes, previous_attributes)`.
//! - Event props: `el.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |event| ...)))`,
//!   `set_on_key_down`, `set_on_paste`, `set_on_size_change`; `ref={r => ...}` is just using the `Element` handle;
//!   `focused` is `el.focus()`.
//!
//! **Control flow**: `show(when, children, fallback)` for `<Show when>`, `show_value` for `<Show when>{(v) =>}`
//! (children get a memo of the value), `show_keyed` for `keyed`, `for_each(each, key, |item, index| ...)` for
//! `<For>` (rows keyed by `key`; each row gets its item as a signal, so a changed item with the same key updates in
//! place, as a store proxy would), `index` for `<Index>`, `switch(vec![when(cond, children), ...], fallback)` for
//! `<Switch>/<Match>`. A `cond ? a : b` in JSX is `memo_view(move || if cond() { a } else { b })`.
//! `<Portal ref>{children}</Portal>` is `portal(None, Some(on_ref), Rc::new(children))`: the children go into a box
//! appended to the root.
//!
//! **State**: `create_signal` / `create_memo` / `create_effect` / `create_render_effect` / `create_computed` / `on`
//! / `batch` / `untrack` / `on_mount` / `on_cleanup` (snake_case, same semantics; signals compare with
//! `PartialEq`, `create_signal_with(v, None)` notifies on every write). A `createStore` becomes a struct of signals
//! (one per key the components read) or one signal updated with `update` (`produce`); `reconcile` is a `set` (equal
//! values do not notify). Contexts: `create_context`, `provide_context` inside the provider's owner, `use_context`.
//!
//! **Reaching renderables**: every intrinsic constructor returns an [`Element`](opencode_tui_core::tree::Element)
//! (`Copy`), the handle the TS keeps as a ref: geometry (`x`, `y`, `width`, `height`), `focus`/`blur`,
//! `children`, `find_descendant_by_id`, the kind's setters (`set_content`, `set_wrap_mode`, `textarea_text`,
//! `editor_view`, ...). A kind's own state is behind `el.with_kind::<K, _>(|k| ...)`. A custom renderable
//! (`extend({ name: XRenderable })`) is a `Kind` made with `tree::create`; a TextRenderable subclass is
//! `renderables::text::create_text_with(|text| Box::new(Sub { text, .. }))`, delegating to the inner `TextKind`.
//!
//! **Keybinds** ([`keymap`], `context/keymap.tsx` over `opencode-tui-input`'s `@opentui/keymap`): a layer is
//! `keymap::create_layer(move || Layer { mode, enabled, target, priority, commands, bindings, .. })`, re-run in an
//! effect like `Keymap.createLayer`; `mode: None` is the base mode, `Some("global")` any mode, `Some("modal")` only
//! while a dialog is open (`keymap::mode_push`). A [`keymap::Command`] mirrors `KeymapCommand` (`Command::new(id,
//! run).title(..).group(..).bind(Bind::Off).palette(true).slash(..)`); its `run` returns false to let the key fall
//! through. `bindings: vec!["id", ..]` applies the config keybinds of those ids. `keymap::dispatch(id, input)`,
//! `keymap::use_commands()` (the palette's list), `keymap::use_shortcuts().get/all(id)` (hints, formatted as TS
//! formats them). App's command registry is [`commands`].
//!
//! **The theme** is `tui().theme` (a signal of [`ResolvedTheme`](theme::ResolvedTheme), which derefs to the token
//! tree: `theme.text.muted`, `theme.background.raised.base`); `util::rgba(color)` converts a token to a renderable
//! color. **The client** is `tui().data.client` ([`opencode_tui_sync::client::Client`], every operation by name:
//! `client.call_named("session.create", &input, RequestOptions::default()).await`); spawn calls with
//! `tokio::task::spawn_local` and write their results into signals. TODO(port-tui): the sync slice's data store
//! (data.ts) replaces [`data::Data`]'s direct loads; its `reactive::Reactivity` maps onto signals.
//!
//! **Event handlers run without an owner** (as in Solid, where the renderer calls them outside any computation):
//! `tui()` and `use_context` return nothing there, so take what a handler needs (`tui().toast`, signals, the data)
//! when the component is created, and move it into the closure.
//!
//! **Config**: `tui().config` is the config at startup; toggles that commands change read
//! `tui().config_context.data()` (tracked) and write with `tui().config_context.update(|draft| ..)`, which edits
//! cli.json through the CLI config service and resolves it again ([`config_context`], config/index.tsx).
//!
//! **Toasts**: `tui().toast.show(ToastInput::new(Variant::Info, "...").duration(3000))`, `tui().toast.error(..)`
//! ([`toast`], ui/toast.tsx); App draws `<Toast>` last.
//!
//! **Timers and ticks**: `renderer::set_timeout(ms, f)` / `clear_timeout`, `renderer::next_tick(f)` (also
//! `queueMicrotask`); the renderer draws on demand, so anything that changes a renderable asks for a frame itself.
//! `renderer::on_animation_tick(fps, f)` / `off_animation_tick` is the renderer's shared animation clock, and
//! `el.set_animation_fps(Some(fps))` (`animationFps`) ticks a kind's `Kind::on_animation_tick` on it while drawn.
//! `renderer::on_terminal_focus(f)` / `off_terminal_focus` are the renderer's `focus`/`blur` events.
//!
//! **Plugins**: only the built-in feature plugins are in scope, so a `<Slot path>` renders the built-in plugin's
//! content for that path directly (see [`home`] and [`prompt`]).
//!
//! Divergences and stand-ins are listed at the top of each module and marked `TODO(port-tui)`.
pub mod app;
pub mod commands;
pub mod config;
pub mod config_context;
pub mod ctx;
pub mod data;
pub mod form;
pub mod home;
pub mod interactivity;
pub mod keymap;
pub mod local;
pub mod permission;
pub mod prompt;
pub mod session;
pub mod theme;
pub mod toast;
pub mod tools;
pub mod util;
