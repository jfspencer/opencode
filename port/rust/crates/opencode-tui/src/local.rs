//! The agent and model selection of a new session (`context/local.tsx`: `agent.current`, `agent.color`,
//! `model.parsed`, `model.variant.current`) and the prompt's display of them (`promptDisplay`).
//!
//! TODO(port-tui): the rest of local.tsx (model preferences in `model.json`, recent and favorite cycling,
//! per-session drafts, the remembered selection per location and agent) arrives with the data contexts.
use std::rc::Rc;

use opencode_tui_core::color::RGBA;
use opencode_tui_core::reactive::Memo;
use opencode_tui_core::reactive::create_memo;

use crate::ctx::tui;
use crate::data::{AgentInfo, Data};
use crate::util::{rgba, titlecase};

/// What the prompt shows of the selection.
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub agent: Option<String>,
    pub model: String,
    pub provider: String,
    pub variant: Option<String>,
}

#[derive(Clone)]
pub struct Local {
    data: Rc<Data>,
    agent: Memo<Option<AgentInfo>>,
    selection: Memo<Option<(String, String, Option<String>)>>,
}

/// `useLocal()`.
pub fn local() -> Local {
    let t = tui();
    let data = t.data.clone();
    let args = t.args.clone();
    let agent = {
        let data = data.clone();
        let args = args.clone();
        create_memo(move || {
            // `agents()`: primary, visible agents.
            let agents: Vec<AgentInfo> = data
                .agents
                .get()
                .unwrap_or_default()
                .into_iter()
                .filter(|a| a.mode != "subagent" && !a.hidden)
                .collect();
            let selected = args.agent.clone();
            selected.and_then(|id| agents.iter().find(|a| a.id == id).cloned()).or_else(|| agents.first().cloned())
        })
    };
    let selection = {
        let data = data.clone();
        create_memo(move || {
            let models = data.models.get()?;
            let valid = |provider: &str, model: &str| models.iter().any(|m| m.provider_id == provider && m.id == model);
            let current = agent.get();
            let configured = data.configured_model.get();
            // newSessionModel: the agent's model, then the fallback (--model, configured, the first model).
            let chosen = current
                .as_ref()
                .and_then(|a| a.model.clone())
                .filter(|(p, m, _)| valid(p, m))
                .or_else(|| {
                    args.model
                        .as_deref()
                        .map(crate::data::parse_model)
                        .filter(|(p, m)| valid(p, m))
                        .map(|(p, m)| (p, m, None))
                })
                .or_else(|| configured.clone().filter(|(p, m, _)| valid(p, m)))
                .or_else(|| models.first().map(|m| (m.provider_id.clone(), m.id.clone(), None)))?;
            // preferredSelection: the agent's configured variant, else the configured model's, if the model has it.
            let (provider, model, _) = chosen.clone();
            let agent_variant = current
                .as_ref()
                .and_then(|a| a.model.clone())
                .filter(|(p, m, _)| *p == provider && *m == model)
                .and_then(|(_, _, v)| v);
            let configured_variant = configured.filter(|(p, m, _)| *p == provider && *m == model).and_then(|(_, _, v)| v);
            let variant = agent_variant.or(configured_variant);
            let info = models.iter().find(|m| m.provider_id == provider && m.id == model);
            let variant = variant.filter(|v| info.is_some_and(|i| i.variants.iter().any(|x| x == v)));
            Some((provider, model, variant))
        })
    };
    Local { data, agent, selection }
}

impl Local {
    /// `Locale.titlecase(agent.id)`.
    pub fn agent_label(&self) -> Option<String> {
        self.agent.get().map(|a| titlecase(&a.id))
    }

    /// `local.agent.color(id)`: the agent's color, else the theme's categorical color of its index.
    pub fn agent_color(&self) -> RGBA {
        let theme = tui().theme.get();
        let agents: Vec<AgentInfo> = self
            .data
            .agents
            .get()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.mode != "subagent" && !a.hidden)
            .collect();
        let colors: Vec<RGBA> = theme.categorical.iter().map(|scale| rgba(scale[500])).collect();
        let fallback = colors.first().copied().unwrap_or(RGBA::from_values(1.0, 1.0, 1.0, 1.0));
        let Some(current) = self.agent.get() else { return fallback };
        let Some(index) = agents.iter().position(|a| a.id == current.id) else { return fallback };
        if let Some(color) = agents[index].color.as_deref() {
            return RGBA::from_hex(color);
        }
        colors.get(index % colors.len().max(1)).copied().unwrap_or(fallback)
    }

    /// `promptDisplay()`.
    pub fn display(&self) -> Display {
        let Some((provider_id, model_id, variant)) = self.selection.get() else {
            return Display {
                agent: self.agent_label(),
                model: "No provider selected".into(),
                provider: "Connect a provider".into(),
                variant: None,
            };
        };
        let provider = self
            .data
            .providers
            .get()
            .unwrap_or_default()
            .into_iter()
            .find(|p| p.id == provider_id)
            .map(|p| p.name)
            .unwrap_or(provider_id.clone());
        let model = self
            .data
            .models
            .get()
            .unwrap_or_default()
            .into_iter()
            .find(|m| m.provider_id == provider_id && m.id == model_id)
            .map(|m| m.name)
            .unwrap_or(format!("{model_id} (unavailable)"));
        Display { agent: self.agent_label(), model, provider, variant }
    }

    /// Whether the lists the display needs have loaded (`loading` in promptDisplay).
    pub fn ready(&self) -> bool {
        self.data.agents.with(|a| a.is_some()) && self.data.models.with(|m| m.is_some())
    }
}
