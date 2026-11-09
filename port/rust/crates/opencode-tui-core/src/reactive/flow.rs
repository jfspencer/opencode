//! solid-js 1.9.17's list mapping (`mapArray`, `indexArray`) and `on`, generic over the mapped value; the View-level
//! control flow (`Show`, `For`, `Index`, `Switch`/`Match`) is built on them in `crate::solid`.
//!
//! Divergence: `mapArray` matches rows by item identity (`===`). Rust items are values, so rows are matched by a
//! key the caller derives from the item (a session's id, a string itself), and each row receives its item as a
//! signal: when a later list holds an item with the same key but a different value, the row's signal is set (a
//! store-backed item in Solid would be the same proxy, read reactively inside the row). The matching algorithm,
//! and so the order rows are created, moved and disposed, is Solid's.
use std::collections::HashMap;
use std::hash::Hash;

use std::rc::Rc;

use super::runtime::{Dispose, Signal, create_root, create_signal, on_cleanup, untrack};

/// A mapped list with JS array identity: equal only to itself (`===`), so a memo over it notifies exactly when
/// Solid's would (mapArray returns the same array only for an empty list that stays empty without a fallback).
#[derive(Debug)]
pub struct Mapped<U>(pub Rc<Vec<U>>);

impl<U> Clone for Mapped<U> {
    fn clone(&self) -> Self {
        Mapped(self.0.clone())
    }
}

impl<U> PartialEq for Mapped<U> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl<U> std::ops::Deref for Mapped<U> {
    type Target = [U];
    fn deref(&self) -> &[U] {
        &self.0
    }
}

/// `mapArray(list, mapFn)`: returns the mapping function a memo runs (`For` = `createMemo(mapArray(...))`).
pub fn map_array<T, K, U>(
    list: impl Fn() -> Vec<T> + 'static,
    key: impl Fn(&T) -> K + 'static,
    map_fn: impl Fn(Signal<T>, Signal<usize>) -> U + 'static,
    fallback: Option<Box<dyn Fn() -> U>>,
) -> impl FnMut() -> Mapped<U>
where
    T: Clone + PartialEq + 'static,
    K: Clone + Eq + Hash + 'static,
    U: Clone + 'static,
{
    struct Row<T, K, U> {
        key: Option<K>,
        item: Option<Signal<T>>,
        index: Option<Signal<usize>>,
        mapped: U,
        dispose: Dispose,
    }
    let rows: std::rc::Rc<std::cell::RefCell<Vec<Row<T, K, U>>>> = Default::default();
    {
        let rows = rows.clone();
        on_cleanup(move || {
            for row in std::mem::take(&mut *rows.borrow_mut()) {
                row.dispose.dispose();
            }
        });
    }
    let map_fn = std::rc::Rc::new(map_fn);
    let mut current: Mapped<U> = Mapped(Rc::new(Vec::new()));
    let make = move |item: T, j: usize, key: K| -> Row<T, K, U> {
        let map_fn = map_fn.clone();
        create_root(move |dispose| {
            let (item_signal, _) = create_signal(item);
            let (index, _) = create_signal(j);
            let mapped = map_fn(item_signal, index);
            Row { key: Some(key), item: Some(item_signal), index: Some(index), mapped, dispose }
        })
    };
    move || {
        let new_items = list();
        untrack(|| {
            let mut rows_ref = rows.borrow_mut();
            let new_len = new_items.len();
            if new_len == 0 {
                // The same array unless rows went or a fallback is (re)created.
                if !rows_ref.is_empty() {
                    for row in rows_ref.drain(..) {
                        row.dispose.dispose();
                    }
                    current = Mapped(Rc::new(Vec::new()));
                }
                if let Some(fallback) = &fallback {
                    let row = create_root(|dispose| Row {
                        key: None,
                        item: None,
                        index: None,
                        mapped: fallback(),
                        dispose,
                    });
                    rows_ref.push(row);
                    current = Mapped(Rc::new(rows_ref.iter().map(|row| row.mapped.clone()).collect()));
                }
                return current.clone();
            }
            let new_keys: Vec<K> = new_items.iter().map(&key).collect();
            // A fallback row (no key) goes like any unmatched row.
            if rows_ref.is_empty() {
                drop(rows_ref);
                let mut created = Vec::with_capacity(new_len);
                for (j, item) in new_items.into_iter().enumerate() {
                    created.push(make(item, j, new_keys[j].clone()));
                }
                let mut rows_ref = rows.borrow_mut();
                *rows_ref = created;
                current = Mapped(Rc::new(rows_ref.iter().map(|row| row.mapped.clone()).collect()));
                return current.clone();
            }
            let old = std::mem::take(&mut *rows_ref);
            drop(rows_ref);
            let len = old.len();
            let mut temp: Vec<Option<Row<T, K, U>>> = (0..new_len).map(|_| None).collect();
            let mut old: Vec<Option<Row<T, K, U>>> = old.into_iter().map(Some).collect();
            let same = |row: &Option<Row<T, K, U>>, key: &K| row.as_ref().and_then(|r| r.key.as_ref()) == Some(key);
            let mut start = 0;
            let end_min = len.min(new_len);
            while start < end_min && same(&old[start], &new_keys[start]) {
                start += 1;
            }
            // The common prefix keeps its rows in place.
            let mut end = len as isize - 1;
            let mut new_end = new_len as isize - 1;
            while end >= start as isize && new_end >= start as isize && same(&old[end as usize], &new_keys[new_end as usize]) {
                temp[new_end as usize] = old[end as usize].take();
                end -= 1;
                new_end -= 1;
            }
            let mut new_indices: HashMap<K, isize> = HashMap::new();
            let mut new_indices_next: Vec<isize> = vec![-1; (new_end + 1).max(0) as usize];
            let mut j = new_end;
            while j >= start as isize {
                let key = &new_keys[j as usize];
                let i = new_indices.get(key).copied();
                new_indices_next[j as usize] = i.unwrap_or(-1);
                new_indices.insert(key.clone(), j);
                j -= 1;
            }
            let mut i = start as isize;
            while i <= end {
                let row = old[i as usize].take();
                let found = row.as_ref().and_then(|r| r.key.as_ref()).and_then(|key| new_indices.get(key).copied());
                match (row, found) {
                    (Some(row), Some(j)) if j != -1 => {
                        let key = row.key.clone().unwrap();
                        temp[j as usize] = Some(row);
                        let next = new_indices_next[j as usize];
                        new_indices.insert(key, next);
                    }
                    (Some(row), _) => row.dispose.dispose(),
                    (None, _) => {}
                }
                i += 1;
            }
            let mut prefix = old.into_iter().take(start).flatten();
            let mut next_rows: Vec<Row<T, K, U>> = Vec::with_capacity(new_len);
            for (j, item) in new_items.into_iter().enumerate() {
                if j < start {
                    // Rows of the common prefix stay; they take their (possibly changed) values.
                    let row = prefix.next().unwrap();
                    if let Some(signal) = row.item {
                        signal.set(item);
                    }
                    next_rows.push(row);
                    continue;
                }
                match temp[j].take() {
                    Some(row) => {
                        if let Some(index) = row.index {
                            index.set(j);
                        }
                        if let Some(signal) = row.item {
                            signal.set(item);
                        }
                        next_rows.push(row);
                    }
                    None => next_rows.push(make(item, j, new_keys[j].clone())),
                }
            }
            current = Mapped(Rc::new(next_rows.iter().map(|row| row.mapped.clone()).collect()));
            *rows.borrow_mut() = next_rows;
            current.clone()
        })
    }
}

/// `indexArray(list, mapFn)`: one row per index, its item a signal (`Index`).
pub fn index_array<T, U>(
    list: impl Fn() -> Vec<T> + 'static,
    map_fn: impl Fn(Signal<T>, usize) -> U + 'static,
    fallback: Option<Box<dyn Fn() -> U>>,
) -> impl FnMut() -> Mapped<U>
where
    T: Clone + PartialEq + 'static,
    U: Clone + 'static,
{
    struct Row<T, U> {
        /// None for the fallback row (Solid's FALLBACK item).
        item: Option<Signal<T>>,
        mapped: U,
        dispose: Dispose,
    }
    let rows: Rc<std::cell::RefCell<Vec<Row<T, U>>>> = Default::default();
    {
        let rows = rows.clone();
        on_cleanup(move || {
            for row in std::mem::take(&mut *rows.borrow_mut()) {
                row.dispose.dispose();
            }
        });
    }
    let map_fn = Rc::new(map_fn);
    let mut current: Mapped<U> = Mapped(Rc::new(Vec::new()));
    move || {
        let new_items = list();
        untrack(|| {
            let new_len = new_items.len();
            if new_len == 0 {
                let removed: Vec<_> = std::mem::take(&mut *rows.borrow_mut());
                if !removed.is_empty() {
                    for row in removed {
                        row.dispose.dispose();
                    }
                    current = Mapped(Rc::new(Vec::new()));
                }
                if let Some(fallback) = &fallback {
                    let row = create_root(|dispose| Row { item: None, mapped: fallback(), dispose });
                    current = Mapped(Rc::new(vec![row.mapped.clone()]));
                    rows.borrow_mut().push(row);
                }
                return current.clone();
            }
            let is_fallback = rows.borrow().first().is_some_and(|row| row.item.is_none());
            if is_fallback {
                for row in std::mem::take(&mut *rows.borrow_mut()) {
                    row.dispose.dispose();
                }
            }
            let old_len = rows.borrow().len();
            for (i, item) in new_items.into_iter().enumerate() {
                if i < old_len {
                    let signal = rows.borrow()[i].item;
                    if let Some(signal) = signal {
                        signal.set(item);
                    }
                } else {
                    let map_fn = map_fn.clone();
                    let row = create_root(move |dispose| {
                        let (signal, _) = create_signal(item);
                        Row { item: Some(signal), mapped: map_fn(signal, i), dispose }
                    });
                    rows.borrow_mut().push(row);
                }
            }
            let removed: Vec<_> = {
                let mut rows_ref = rows.borrow_mut();
                if rows_ref.len() > new_len { rows_ref.drain(new_len..).collect() } else { Vec::new() }
            };
            for row in removed {
                row.dispose.dispose();
            }
            current = Mapped(Rc::new(rows.borrow().iter().map(|row| row.mapped.clone()).collect()));
            current.clone()
        })
    }
}

/// `on(deps, fn, { defer })` for an effect: `deps` is tracked, `f(input, prevInput)` runs untracked.
pub fn on<D: Clone + 'static>(
    deps: impl Fn() -> D + 'static,
    mut f: impl FnMut(&D, Option<&D>) + 'static,
    defer: bool,
) -> impl FnMut() + 'static {
    let mut prev: Option<D> = None;
    let mut defer = defer;
    move || {
        let input = deps();
        if defer {
            defer = false;
            // Solid keeps prevInput unset on the deferred run.
            return;
        }
        untrack(|| f(&input, prev.as_ref()));
        prev = Some(input);
    }
}
