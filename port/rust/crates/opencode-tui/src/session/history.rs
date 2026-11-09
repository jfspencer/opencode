//! `routes/session/history.ts`: `createHistoryPrepend`, one older page at a time, with the scroll and the
//! continuation applied once the page is laid out.
use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

type Continuation = Box<dyn FnOnce()>;

struct Pending {
    id: u64,
    scroll_by: f64,
    continuation: Option<Continuation>,
    after: Option<Continuation>,
}

/// The inputs of `createHistoryPrepend`.
pub struct HistoryInput {
    pub session_id: Rc<dyn Fn() -> String>,
    pub more: Rc<dyn Fn(&str) -> bool>,
    /// Loads the next older page; `Err` when it failed.
    pub load_more: Rc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<(), ()>>>>>,
    pub after_layout: Rc<dyn Fn(Continuation)>,
    pub active: Rc<dyn Fn(&str) -> bool>,
    pub scroll_by: Rc<dyn Fn(f64)>,
}

/// `createHistoryPrepend(input)`: call it (`prepend(scroll_by, continuation)`), `cancel()` or `after(f)`.
#[derive(Clone)]
pub struct HistoryPrepend {
    input: Rc<HistoryInput>,
    pending: Rc<RefCell<Option<Pending>>>,
    next: Rc<std::cell::Cell<u64>>,
}

impl HistoryPrepend {
    pub fn new(input: HistoryInput) -> HistoryPrepend {
        HistoryPrepend { input: Rc::new(input), pending: Rc::new(RefCell::new(None)), next: Rc::new(std::cell::Cell::new(0)) }
    }

    /// The prepend: true when a page is (or was already) on its way.
    pub fn prepend(&self, scroll_by: f64, continuation: Option<Continuation>) -> bool {
        let session_id = (self.input.session_id)();
        {
            let mut pending = self.pending.borrow_mut();
            if let Some(pending) = pending.as_mut() {
                if continuation.is_some() || (pending.scroll_by == 0.0 && scroll_by != 0.0) {
                    pending.scroll_by = scroll_by;
                    pending.continuation = continuation;
                    return true;
                }
                return false;
            }
        }
        if !(self.input.more)(&session_id) {
            return false;
        }
        let id = self.next.get() + 1;
        self.next.set(id);
        *self.pending.borrow_mut() = Some(Pending { id, scroll_by, continuation, after: None });
        let load = (self.input.load_more)(&session_id);
        let this = self.clone();
        tokio::task::spawn_local(async move {
            let result = load.await;
            let current = move |pending: &Option<Pending>| pending.as_ref().is_some_and(|p| p.id == id);
            match result {
                Ok(()) => {
                    let inner = this.clone();
                    let session_id = session_id.clone();
                    (this.input.after_layout)(Box::new(move || {
                        let Some(pending) = ({
                            let mut slot = inner.pending.borrow_mut();
                            if current(&slot) { slot.take() } else { None }
                        }) else {
                            return;
                        };
                        if !(inner.input.active)(&session_id) {
                            return;
                        }
                        (inner.input.scroll_by)(pending.scroll_by);
                        if let Some(continuation) = pending.continuation {
                            continuation();
                        }
                        if let Some(after) = pending.after {
                            after();
                        }
                    }));
                }
                Err(()) => {
                    let Some(pending) = ({
                        let mut slot = this.pending.borrow_mut();
                        if current(&slot) { slot.take() } else { None }
                    }) else {
                        return;
                    };
                    if (this.input.active)(&session_id) {
                        if let Some(after) = pending.after {
                            after();
                        }
                    }
                }
            }
        });
        true
    }

    /// `cancel()`.
    pub fn cancel(&self) {
        if let Some(pending) = self.pending.borrow_mut().as_mut() {
            pending.continuation = None;
            pending.after = None;
        }
    }

    /// `after(continuation)`: a jump supersedes deferred scrolling, but waits until the prepended page is laid out.
    pub fn after(&self, continuation: Continuation) {
        let mut slot = self.pending.borrow_mut();
        match slot.as_mut() {
            None => {
                drop(slot);
                continuation()
            }
            Some(pending) => {
                pending.scroll_by = 0.0;
                pending.after = Some(continuation);
            }
        }
    }
}
