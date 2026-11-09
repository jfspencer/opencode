//! Lexical scopes (scope.ts): a stack of binding maps; closures capture the stack by sharing its maps.

use crate::interp::errors::{reference_error, type_error};
use crate::interp::value::Value;
use crate::interp::{Loc, R};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

/// FxHash: identifiers are short and trusted.
#[derive(Default)]
pub struct FxHasher(u64);

impl Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0.rotate_left(5) ^ byte as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.0 = (self.0.rotate_left(5) ^ i as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn write_usize(&mut self, i: usize) {
        self.0 = (self.0.rotate_left(5) ^ i as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

pub type FxBuild = BuildHasherDefault<FxHasher>;

#[derive(Clone)]
pub struct Binding {
    pub mutable: bool,
    pub value: Value,
    /// False only for a reserved binding in its temporal dead zone.
    pub initialized: bool,
}

pub type ScopeMap = HashMap<Rc<str>, Binding, FxBuild>;

#[derive(Clone, Default)]
pub struct Scope(pub Rc<RefCell<ScopeMap>>);

impl Scope {
    pub fn new() -> Scope {
        Scope::default()
    }
    pub fn with(name: Rc<str>, binding: Binding) -> Scope {
        let scope = Scope::new();
        scope.0.borrow_mut().insert(name, binding);
        scope
    }
    pub fn has(&self, name: &str) -> bool {
        self.0.borrow().contains_key(name)
    }
    pub fn set_binding(&self, name: Rc<str>, binding: Binding) {
        self.0.borrow_mut().insert(name, binding);
    }
    pub fn ptr_eq(&self, other: &Scope) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

pub struct ScopeStack {
    scopes: RefCell<Vec<Scope>>,
}

impl ScopeStack {
    pub fn new(scopes: Vec<Scope>) -> ScopeStack {
        ScopeStack { scopes: RefCell::new(scopes) }
    }

    pub fn reserve(&self, name: &Rc<str>, mutable: bool, loc: Loc) -> R<()> {
        let scope = self.current();
        let mut map = scope.0.borrow_mut();
        if map.contains_key(&**name) {
            return Err(type_error(format!("Identifier '{name}' has already been declared.")).at(Some(loc)));
        }
        map.insert(name.clone(), Binding { mutable, value: Value::Undefined, initialized: false });
        Ok(())
    }

    pub fn initialize(&self, name: &str, value: Value, loc: Loc) -> R<()> {
        let scope = self.current();
        let mut map = scope.0.borrow_mut();
        match map.get_mut(name) {
            Some(binding) if !binding.initialized => {
                binding.value = value;
                binding.initialized = true;
                Ok(())
            }
            _ => Err(type_error(format!("Identifier '{name}' has not been reserved for initialization.")).at(Some(loc))),
        }
    }

    pub fn declare(&self, name: &Rc<str>, value: Value, mutable: bool, loc: Loc) -> R<()> {
        let scope = self.current();
        let mut map = scope.0.borrow_mut();
        if map.contains_key(&**name) {
            return Err(type_error(format!("Identifier '{name}' has already been declared.")).at(Some(loc)));
        }
        map.insert(name.clone(), Binding { mutable, value, initialized: true });
        Ok(())
    }

    /// Reads a binding, or None when the name is not bound at all.
    fn lookup<T>(&self, name: &str, read: impl FnOnce(&mut Binding) -> T) -> Option<T> {
        let scopes = self.scopes.borrow();
        for scope in scopes.iter().rev() {
            let mut map = scope.0.borrow_mut();
            if let Some(binding) = map.get_mut(name) {
                return Some(read(binding));
            }
        }
        None
    }

    pub fn get(&self, name: &str, loc: Loc) -> R<Value> {
        match self.lookup(name, |binding| {
            if binding.initialized { Ok(binding.value.clone()) } else { Err(()) }
        }) {
            None => Err(reference_error(format!("Unknown identifier '{name}'.")).at(Some(loc))),
            Some(Err(())) => Err(reference_error(format!("Cannot access '{name}' before initialization.")).at(Some(loc))),
            Some(Ok(value)) => Ok(value),
        }
    }

    pub fn set(&self, name: &str, value: Value, loc: Loc) -> R<Value> {
        let outcome = self.lookup(name, |binding| {
            if !binding.initialized {
                return Err(true);
            }
            if !binding.mutable {
                return Err(false);
            }
            binding.value = value.clone();
            Ok(())
        });
        match outcome {
            None => Err(reference_error(format!("Unknown identifier '{name}'.")).at(Some(loc))),
            Some(Err(true)) => Err(reference_error(format!("Cannot access '{name}' before initialization.")).at(Some(loc))),
            Some(Err(false)) => Err(type_error(format!("Cannot assign to constant '{name}'.")).at(Some(loc))),
            Some(Ok(())) => Ok(value),
        }
    }

    pub fn resolves(&self, name: &str) -> bool {
        self.lookup(name, |_| ()).is_some()
    }

    pub fn current(&self) -> Scope {
        self.scopes.borrow().last().cloned().expect("scope stack is never empty")
    }

    pub fn push(&self, scope: Scope) {
        self.scopes.borrow_mut().push(scope)
    }

    pub fn push_new(&self) {
        self.push(Scope::new())
    }

    pub fn pop(&self) {
        self.scopes.borrow_mut().pop();
    }

    pub fn capture(&self) -> Vec<Scope> {
        self.scopes.borrow().clone()
    }

    pub fn replace_current(&self, scope: Scope) {
        let mut scopes = self.scopes.borrow_mut();
        scopes.pop();
        scopes.push(scope);
    }
}
