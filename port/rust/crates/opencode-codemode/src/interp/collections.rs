//! Host collections with JS semantics: insertion-ordered Map and Set whose iteration is live (deleted entries are
//! skipped, added ones visited), keyed by SameValueZero, and the weak tables behind WeakMap and WeakSet.

use crate::interp::value::{MapKey, Obj, ObjInner, Value};
use std::collections::HashMap;
use std::rc::Weak;

fn normalize(key: Value) -> Value {
    match key {
        Value::Num(n) if n == 0.0 => Value::Num(0.0),
        other => other,
    }
}

#[derive(Default)]
pub struct OrderedMap {
    entries: Vec<Option<(Value, Value)>>,
    index: HashMap<MapKey, usize>,
    live: usize,
}

impl OrderedMap {
    pub fn len(&self) -> usize {
        self.live
    }
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }
    pub fn get(&self, key: &Value) -> Option<Value> {
        let at = *self.index.get(&MapKey(key.clone()))?;
        self.entries[at].as_ref().map(|(_, value)| value.clone())
    }
    pub fn has(&self, key: &Value) -> bool {
        self.index.contains_key(&MapKey(key.clone()))
    }
    pub fn set(&mut self, key: Value, value: Value) {
        let key = normalize(key);
        if let Some(&at) = self.index.get(&MapKey(key.clone())) {
            if let Some(entry) = self.entries[at].as_mut() {
                entry.1 = value;
            }
            return;
        }
        self.index.insert(MapKey(key.clone()), self.entries.len());
        self.entries.push(Some((key, value)));
        self.live += 1;
    }
    pub fn delete(&mut self, key: &Value) -> bool {
        match self.index.remove(&MapKey(key.clone())) {
            Some(at) => {
                self.entries[at] = None;
                self.live -= 1;
                true
            }
            None => false,
        }
    }
    pub fn clear(&mut self) {
        for entry in self.entries.iter_mut() {
            *entry = None;
        }
        self.index.clear();
        self.live = 0;
    }
    /// The entry at or after `position`, and the position after it: how a live iterator advances.
    pub fn entry_from(&self, position: usize) -> Option<(usize, Value, Value)> {
        let mut at = position;
        while at < self.entries.len() {
            if let Some((key, value)) = &self.entries[at] {
                return Some((at + 1, key.clone(), value.clone()));
            }
            at += 1;
        }
        None
    }
    pub fn entries(&self) -> Vec<(Value, Value)> {
        self.entries.iter().flatten().cloned().collect()
    }
    pub fn keys(&self) -> Vec<Value> {
        self.entries.iter().flatten().map(|(key, _)| key.clone()).collect()
    }
}

#[derive(Default)]
pub struct OrderedSet {
    map: OrderedMap,
}

impl OrderedSet {
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn has(&self, value: &Value) -> bool {
        self.map.has(value)
    }
    pub fn add(&mut self, value: Value) {
        if !self.map.has(&value) {
            self.map.set(value, Value::Undefined);
        }
    }
    pub fn delete(&mut self, value: &Value) -> bool {
        self.map.delete(value)
    }
    pub fn clear(&mut self) {
        self.map.clear()
    }
    pub fn value_from(&self, position: usize) -> Option<(usize, Value)> {
        self.map.entry_from(position).map(|(next, key, _)| (next, key))
    }
    pub fn values(&self) -> Vec<Value> {
        self.map.keys()
    }
}

/// Entries held by a weak handle on their object key.
#[derive(Default)]
pub struct WeakTable {
    entries: HashMap<usize, (Weak<ObjInner>, Value)>,
    inserts: usize,
}

impl WeakTable {
    fn live(&self, key: &Obj) -> Option<&(Weak<ObjInner>, Value)> {
        let entry = self.entries.get(&key.addr())?;
        if entry.0.upgrade().is_some_and(|inner| std::ptr::eq(std::rc::Rc::as_ptr(&inner), std::rc::Rc::as_ptr(&key.0))) {
            Some(entry)
        } else {
            None
        }
    }
    pub fn get(&self, key: &Obj) -> Option<Value> {
        self.live(key).map(|entry| entry.1.clone())
    }
    pub fn has(&self, key: &Obj) -> bool {
        self.live(key).is_some()
    }
    pub fn set(&mut self, key: &Obj, value: Value) {
        self.inserts += 1;
        if self.inserts % 256 == 0 {
            self.entries.retain(|_, entry| entry.0.strong_count() > 0);
        }
        self.entries.insert(key.addr(), (key.downgrade(), value));
    }
    pub fn delete(&mut self, key: &Obj) -> bool {
        if self.live(key).is_none() {
            return false;
        }
        self.entries.remove(&key.addr());
        true
    }
}
