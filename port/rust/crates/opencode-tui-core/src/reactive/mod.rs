//! Solid 1.9.17's reactive layer, so the TUI's components port nearly line for line.
//!
//! | solid-js | here |
//! |---|---|
//! | `const [a, setA] = createSignal(v)` | `let (a, set_a) = create_signal(v)` (one `Copy` handle; `PartialEq` compares) |
//! | `a()` / `untrack(a)` | `a.get()` / `a.get_untracked()`; `a.with(\|v\| ..)` borrows |
//! | `setA(v)` / `setA(p => f(p))` | `set_a.set(v)` / `set_a.set_with(\|p\| f(p))`; `set_a.update(\|v\| ..)` mutates and notifies |
//! | `createSignal(v, { equals: false })` | `create_signal_with(v, None)` |
//! | `createMemo(fn)` | `create_memo(move \|\| ..)` (`create_memo_with` gets the previous value) |
//! | `createEffect` / `createRenderEffect` / `createComputed` | `create_effect` / `create_render_effect` / `create_computed` (`_with` variants carry a value) |
//! | `on(deps, fn, { defer })` | `create_effect(on(move \|\| deps, move \|input, prev\| .., defer))` |
//! | `batch` / `untrack` / `onMount` / `onCleanup` | same names, snake_case |
//! | `createRoot(dispose => ..)` / `createRoot(() => ..)` | `create_root(\|dispose\| ..)` / `create_root_unowned(\|\| ..)` |
//! | `getOwner` / `runWithOwner` | same |
//! | `createContext` / `useContext` / `<Ctx.Provider value>` | `create_context` / `use_context` / `provide_context` (or `crate::solid::provider`) |
//! | `mapArray` / `indexArray` | `map_array` (rows keyed, see [`flow`]) / `index_array` |
//! | `createStore` / `produce` / `reconcile` | a struct of `Signal`s (one per top-level key a component reads), or one `Signal<T>` set wholesale; `produce` is a `batch` of field updates; `reconcile` is a `set` (PartialEq skips unchanged values) |
//!
//! Timing is Solid's: a render effect or memo runs when created; a user effect created inside an update (component
//! creation is one) waits until the update completes, after the render effects queued with it; a write inside a
//! batch notifies at the end of the batch; cleanups run last-registered first, before a computation re-runs.
pub mod flow;
pub mod keyed;
pub mod runtime;

pub use flow::{Mapped, index_array, map_array, on};
pub use keyed::KeyedSignals;
pub use runtime::{
    Context, Dispose, Memo, Owner, Signal, batch, create_computed, create_computed_with, create_context, create_effect,
    create_effect_with, create_memo, create_memo_equals, create_memo_with, create_render_effect,
    create_render_effect_with, create_root, create_root_unowned, create_signal, create_signal_with, get_owner,
    is_tracking, live_nodes, on_cleanup, on_mount, provide_context, run_with_owner, untrack, use_context,
};

