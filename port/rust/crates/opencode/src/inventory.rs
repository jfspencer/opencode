//! A location's inventory: its resolved config and what the config and the built-in plugins register in it
//! (packages/core/src/config*.ts, agent.ts, provider.ts, model.ts, command.ts, skill.ts and the plugins under
//! packages/core/src/plugin that fill them). Loaded when the location boots; the wire types are the protocol's.
//!
//! TODO(port): everything. This is the interface the rest of the server builds on: the execution loop reads
//! agents, models and providers from here.
#![allow(dead_code)]

use opencode_protocol::Json;
use opencode_protocol::types as t;

use crate::bus::LocationRef;
use crate::http::Failure;

#[derive(Debug)]
pub struct Inventory {
    /// The location's resolved config (`Config.Info` as JSON), every source merged.
    pub config: Json,
    /// Agents in list order: the default agent first, then the rest in registration order.
    pub agents: Vec<t::AgentInfo>,
    pub providers: Vec<t::ProviderInfo>,
    pub models: Vec<t::ModelInfo>,
}

impl Inventory {
    /// Loads the inventory of a location whose project is resolved.
    pub fn load(_location: &LocationRef) -> Result<Inventory, Failure> {
        Ok(Inventory { config: Json::Object(Default::default()), agents: vec![], providers: vec![], models: vec![] })
    }

    pub fn agent(&self, id: &str) -> Option<&t::AgentInfo> {
        self.agents.iter().find(|a| a.id.0 == id)
    }

    /// The agent a session runs without one chosen (`Agent.select`): the first in list order.
    pub fn default_agent(&self) -> Option<&t::AgentInfo> {
        self.agents.first()
    }

    pub fn provider(&self, id: &str) -> Option<&t::ProviderInfo> {
        self.providers.iter().find(|p| p.id.0 == id)
    }

    pub fn model(&self, provider: &str, id: &str) -> Option<&t::ModelInfo> {
        self.models.iter().find(|m| m.provider_id.0 == provider && m.id == id)
    }
}
