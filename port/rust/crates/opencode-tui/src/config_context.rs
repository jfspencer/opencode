//! `ConfigProvider` / `useConfig()` (`packages/tui/src/config/index.tsx`): the resolved TUI config as reactive
//! state, and `update(mutate)`, which writes the change to cli.json through the CLI config service
//! (`Config.Interface.update`: the file lock, the JSONC edit) and resolves the result again, so every reader
//! re-runs with the new values.
//!
//! `tui().config` stays the config the TUI started with (keybinds and other startup reads); components that read
//! toggles a command can change read `tui().config_context.data()` inside a memo or effect.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::reactive::{Signal, create_signal_with};

use crate::config::{CliConfig, ResolveOptions, Resolved, resolve};

#[derive(Clone)]
pub struct ConfigContext {
    data: Signal<Rc<Resolved>>,
    service: Rc<CliConfig>,
}

impl ConfigContext {
    pub fn new(service: CliConfig, initial: Rc<Resolved>) -> ConfigContext {
        ConfigContext { data: create_signal_with(initial, None).0, service: Rc::new(service) }
    }

    /// `config.data`, tracked.
    pub fn data(&self) -> Rc<Resolved> {
        self.data.get()
    }

    /// `config.data`, untracked.
    pub fn data_untracked(&self) -> Rc<Resolved> {
        self.data.get_untracked()
    }

    /// `config.update(mutate)`: `mutate` edits the cli.json document (`draft`); the written document is resolved
    /// and becomes `data`. A failed write or an invalid result leaves `data` as it was and returns the error text.
    pub fn update(&self, mutate: impl FnOnce(&mut Json)) -> Result<(), String> {
        let written = self.service.update(mutate).map_err(|error| format!("{error:?}"))?;
        let resolved = resolve(&written, &ResolveOptions::default())?;
        self.data.set(Rc::new(resolved));
        Ok(())
    }
}
