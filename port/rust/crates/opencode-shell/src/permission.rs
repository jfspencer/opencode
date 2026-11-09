//! The pure rule evaluation of `packages/core/src/permission.ts` (and `whollyDisabled` of `core/src/tool.ts`): which
//! rule decides a request. Asking, replies and saved permissions are the server's.

use crate::js::{self, JsString};
use crate::wildcard;

/// `Permission.Effect`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Allow,
    Deny,
    Ask,
}

impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Effect::Allow => "allow",
            Effect::Deny => "deny",
            Effect::Ask => "ask",
        }
    }
}

/// `Permission.Rule`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub action: String,
    pub resource: String,
    pub effect: Effect,
}

/// A rule compiled for matching.
#[derive(Clone, Debug)]
struct Compiled {
    action: JsString,
    resource: JsString,
}

fn compile(rules: &[Rule]) -> Vec<Compiled> {
    rules.iter().map(|rule| Compiled { action: js::units(&rule.action), resource: js::units(&rule.resource) }).collect()
}

fn find_last<'a>(action: &[u16], resource: &[u16], rules: &'a [Rule], compiled: &[Compiled]) -> Option<&'a Rule> {
    (0..rules.len())
        .rev()
        .find(|&i| {
            wildcard::matches_units(action, &compiled[i].action) && wildcard::matches_units(resource, &compiled[i].resource)
        })
        .map(|i| &rules[i])
}

/// `Permission.evaluate(action, resource, ...rulesets)`: the last matching rule, else `{ action, resource: "*",
/// effect: "ask" }`. Pass the rulesets concatenated in order.
pub fn evaluate(action: &str, resource: &str, rules: &[Rule]) -> Rule {
    let compiled = compile(rules);
    find_last(&js::units(action), &js::units(resource), rules, &compiled)
        .cloned()
        .unwrap_or_else(|| Rule { action: action.into(), resource: "*".into(), effect: Effect::Ask })
}

fn effects<'a>(action: &str, resources: &'a [String], rules: &'a [Rule]) -> impl Iterator<Item = Effect> + 'a {
    let compiled = compile(rules);
    let action = js::units(action);
    resources.iter().map(move |resource| {
        find_last(&action, &js::units(resource), rules, &compiled).map_or(Effect::Ask, |rule| rule.effect)
    })
}

/// `denied`: some resource evaluates to deny.
pub fn denied(action: &str, resources: &[String], rules: &[Rule]) -> bool {
    effects(action, resources, rules).any(|effect| effect == Effect::Deny)
}

/// `relevant`: the rules for the action, as a `Permission.BlockedError` lists them.
pub fn relevant(action: &str, rules: &[Rule]) -> Vec<Rule> {
    let action = js::units(action);
    rules.iter().filter(|rule| wildcard::matches_units(&action, &js::units(&rule.action))).cloned().collect()
}

/// The decision `Permission` reaches before plugin hooks and unattended rules: deny when the configured rules
/// (agent, then session) deny any resource; else ask when any resource asks under the configured rules followed by
/// the saved ones (each an `allow`); else allow. No resources allow.
pub fn decide(action: &str, resources: &[String], configured: &[Rule], saved: &[Rule]) -> Effect {
    if denied(action, resources, configured) {
        return Effect::Deny;
    }
    let all: Vec<Rule> = configured.iter().chain(saved).cloned().collect();
    if effects(action, resources, &all).any(|effect| effect == Effect::Ask) { Effect::Ask } else { Effect::Allow }
}

/// For unattended work: whether every resource is allowed by the rules followed by the unattended ones.
pub fn allowed_unattended(action: &str, resources: &[String], rules: &[Rule], unattended: &[Rule]) -> bool {
    let settled: Vec<Rule> = rules.iter().chain(unattended).cloned().collect();
    effects(action, resources, &settled).all(|effect| effect == Effect::Allow)
}

/// `whollyDisabled` of tool.ts: the last rule for the action denies every resource, so the tool is hidden.
pub fn wholly_disabled(action: &str, rules: &[Rule]) -> bool {
    let action = js::units(action);
    rules
        .iter()
        .rev()
        .find(|rule| wildcard::matches_units(&action, &js::units(&rule.action)))
        .is_some_and(|rule| rule.resource == "*" && rule.effect == Effect::Deny)
}
