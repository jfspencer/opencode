//! Solid's control flow components (`Show`, `For`, `Index`, `Switch`/`Match`) over [`View`], as solid.js
//! implements them: the same memos, equality and untracking, so children are created and disposed at the same
//! moments.
//!
//! Divergence: JSX children evaluated by a component's `props.children` getter run untracked here. Solid evaluates
//! the getter inside the component's memo, but the compiler has already wrapped every reactive read of a template
//! in its own effect, so the getter itself tracks nothing in practice.
use std::hash::Hash;
use std::rc::Rc;

use super::View;
use crate::reactive::{Memo, Signal, create_memo, create_memo_equals, index_array, map_array, untrack};

pub type Children = Rc<dyn Fn() -> View>;

fn truthiness(a: &bool, b: &bool) -> bool {
    a == b
}

/// `<Show when={cond} fallback={...}>{children}</Show>` with a boolean condition.
pub fn show(when: impl Fn() -> bool + 'static, children: impl Fn() -> View + 'static, fallback: Option<Children>) -> View {
    let condition = create_memo_equals(when, Some(truthiness));
    let view = create_memo(move || {
        if condition.get() {
            untrack(&children)
        } else {
            fallback.as_ref().map(|f| untrack(|| f())).unwrap_or_default()
        }
    });
    View::dynamic(move || view.get())
}

/// `<Show when={value}>{(value) => ...}</Show>` (not keyed): children get an accessor of the current value and
/// are created again only when the value turns from absent to present.
pub fn show_value<T: Clone + PartialEq + 'static>(
    when: impl Fn() -> Option<T> + 'static,
    children: impl Fn(Memo<Option<T>>) -> View + 'static,
    fallback: Option<Children>,
) -> View {
    let condition_value = create_memo(when);
    let condition = create_memo_equals(move || condition_value.with(Option::is_some), Some(truthiness));
    let view = create_memo(move || {
        if condition.get() {
            untrack(|| children(condition_value))
        } else {
            fallback.as_ref().map(|f| untrack(|| f())).unwrap_or_default()
        }
    });
    View::dynamic(move || view.get())
}

/// `<Show when={value} keyed>{(value) => ...}</Show>`: children are created again whenever the value changes.
pub fn show_keyed<T: Clone + PartialEq + 'static>(
    when: impl Fn() -> Option<T> + 'static,
    children: impl Fn(T) -> View + 'static,
    fallback: Option<Children>,
) -> View {
    let condition = create_memo(when);
    let view = create_memo(move || match condition.get() {
        Some(value) => untrack(|| children(value)),
        None => fallback.as_ref().map(|f| untrack(|| f())).unwrap_or_default(),
    });
    View::dynamic(move || view.get())
}

/// `<For each={list}>{(item, index) => ...}</For>`: rows keyed by `key` (see `reactive::flow`).
pub fn for_each<T, K>(
    each: impl Fn() -> Vec<T> + 'static,
    key: impl Fn(&T) -> K + 'static,
    children: impl Fn(Signal<T>, Signal<usize>) -> View + 'static,
    fallback: Option<Children>,
) -> View
where
    T: Clone + PartialEq + 'static,
    K: Clone + Eq + Hash + 'static,
{
    let fallback = fallback.map(|f| Box::new(move || f()) as Box<dyn Fn() -> View>);
    let rows = create_memo(map_array(each, key, children, fallback));
    View::dynamic(move || View::List(Rc::new(rows.get().to_vec())))
}

/// `<Index each={list}>{(item, index) => ...}</Index>`.
pub fn index<T: Clone + PartialEq + 'static>(
    each: impl Fn() -> Vec<T> + 'static,
    children: impl Fn(Signal<T>, usize) -> View + 'static,
    fallback: Option<Children>,
) -> View {
    let fallback = fallback.map(|f| Box::new(move || f()) as Box<dyn Fn() -> View>);
    let rows = create_memo(index_array(each, children, fallback));
    View::dynamic(move || View::List(Rc::new(rows.get().to_vec())))
}

/// `<Match when={cond}>{children}</Match>`.
#[derive(Clone)]
pub struct Match {
    pub when: Rc<dyn Fn() -> bool>,
    pub children: Children,
}

/// `<Match when={cond}>`.
pub fn when(when: impl Fn() -> bool + 'static, children: impl Fn() -> View + 'static) -> Match {
    Match { when: Rc::new(when), children: Rc::new(children) }
}

/// `<Switch fallback={...}>{matches}</Switch>`.
pub fn switch(matches: Vec<Match>, fallback: Option<Children>) -> View {
    // switchFunc: a chain of condition memos, each undefined once an earlier match holds.
    type Func = Rc<dyn Fn() -> Option<usize>>;
    let matches = Rc::new(matches);
    let chain = {
        let matches = matches.clone();
        create_memo_equals(
            move || {
                let mut func: Func = Rc::new(|| None);
                for (index, m) in matches.iter().enumerate() {
                    let prev = func.clone();
                    let when = m.when.clone();
                    let prev_for_value = prev.clone();
                    let condition_value = create_memo(move || if prev_for_value().is_some() { false } else { when() });
                    let condition = create_memo_equals(move || condition_value.get(), Some(truthiness));
                    func = Rc::new(move || prev().or_else(|| condition.get().then_some(index)));
                }
                SwitchFunc(func)
            },
            None,
        )
    };
    let view = create_memo(move || match (chain.get().0)() {
        None => fallback.as_ref().map(|f| untrack(|| f())).unwrap_or_default(),
        Some(index) => {
            let children = matches[index].children.clone();
            untrack(|| children())
        }
    });
    View::dynamic(move || view.get())
}

#[derive(Clone)]
struct SwitchFunc(Rc<dyn Fn() -> Option<usize>>);
