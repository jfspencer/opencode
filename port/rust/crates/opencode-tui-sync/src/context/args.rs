//! `ArgsProvider` (context/args.tsx): the arguments the TUI was started with.

use std::cell::RefCell;

/// `Args`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArgsInput {
    pub model: Option<String>,
    pub agent: Option<String>,
    pub prompt: Option<String>,
    pub r#continue: bool,
    pub session_id: Option<String>,
    pub new_session_id: Option<String>,
    pub fork: bool,
    pub auto: bool,
}

/// `useArgs()`.
#[derive(Debug)]
pub struct Args {
    pub input: ArgsInput,
    /// The first new session created from home takes this ID; later ones mint their own.
    pending: RefCell<Option<String>>,
}

impl Args {
    pub fn new(input: ArgsInput) -> Self {
        let pending = RefCell::new(input.new_session_id.clone());
        Self { input, pending }
    }

    /// `takeNewSessionID()`.
    pub fn take_new_session_id(&self) -> Option<String> {
        self.pending.borrow_mut().take()
    }

    /// `restoreNewSessionID(id)`: gives the ID back when nothing took one since.
    pub fn restore_new_session_id(&self, id: &str) {
        let mut pending = self.pending.borrow_mut();
        if pending.is_none() {
            *pending = Some(id.to_owned());
        }
    }
}

impl std::ops::Deref for Args {
    type Target = ArgsInput;
    fn deref(&self) -> &ArgsInput {
        &self.input
    }
}
