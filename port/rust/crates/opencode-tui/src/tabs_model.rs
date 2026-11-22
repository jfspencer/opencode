//! `context/session-tabs-model.ts`: the session tab list, the recently-closed stack, the navigation history,
//! the status wording, and the adaptive strip layout. Pure functions, as in TS.

pub const NEW_SESSION_TAB_TITLE: &str = "New session";

pub const SESSION_TAB_WIDTH: i64 = 22;
pub const SESSION_TAB_MAX_WIDTH: i64 = 32;
pub const SESSION_TAB_MIN_WIDTH: i64 = 8;

/// A session shown as a tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTab {
    pub session_id: String,
    pub title: Option<String>,
}

impl SessionTab {
    pub fn new(session_id: impl Into<String>, title: Option<String>) -> SessionTab {
        SessionTab { session_id: session_id.into(), title }
    }
}

/// `SessionTabUnread`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unread {
    Activity,
    Error,
}

/// `sessionTabNumberLabel(index)`.
pub fn session_tab_number_label(index: usize) -> String {
    (index + 1).to_string()
}

/// `sessionTabDetail(project, current, defaultBranch, worktree)`.
pub fn session_tab_detail(project: &str, current: &str, default_branch: Option<&str>, worktree: bool) -> Option<String> {
    let branch = (worktree && Some(current) != default_branch).then_some(current);
    match (branch, project.is_empty()) {
        (Some(branch), false) => Some(format!("{project} ⎇ {branch}")),
        (Some(branch), true) => Some(branch.to_string()),
        (None, false) => Some(project.to_string()),
        (None, true) => None,
    }
}

/// `sessionTabComplete(unread, busy)`.
pub fn session_tab_complete(unread: Option<Unread>, busy: bool) -> bool {
    unread == Some(Unread::Activity) && !busy
}

/// `sessionTabOverflowWidth(count)`: the arrow and count plus the gap beside them.
pub fn session_tab_overflow_width(count: i64) -> i64 {
    count.to_string().len() as i64 + 2
}

/// `openSessionTab(tabs, tab)`: appended when new, retitled in place when the title is known and differs.
pub fn open_session_tab(tabs: &[SessionTab], tab: SessionTab) -> Vec<SessionTab> {
    match tabs.iter().position(|item| item.session_id == tab.session_id) {
        None => tabs.iter().cloned().chain(std::iter::once(tab)).collect(),
        Some(index) => {
            if tab.title.is_none() || tabs[index].title == tab.title {
                return tabs.to_vec();
            }
            let mut next = tabs.to_vec();
            next[index].title = tab.title;
            next
        }
    }
}

/// `closeSessionTab(tabs, sessionID, lastIndex)`: the remaining tabs, whether they changed (identity in TS), and the
/// neighbour that takes the closed tab's place.
pub struct Closed {
    pub tabs: Vec<SessionTab>,
    pub changed: bool,
    pub next: Option<String>,
}

pub fn close_session_tab(tabs: &[SessionTab], session_id: &str, last_index: Option<usize>) -> Closed {
    let Some(index) = tabs.iter().position(|tab| tab.session_id == session_id) else {
        let next = last_index.and_then(|last| tabs.get(last.min(tabs.len().saturating_sub(1))).map(|tab| tab.session_id.clone()));
        return Closed { tabs: tabs.to_vec(), changed: false, next };
    };
    Closed {
        tabs: tabs.iter().filter(|tab| tab.session_id != session_id).cloned().collect(),
        changed: true,
        next: tabs.get(index + 1).or_else(|| index.checked_sub(1).and_then(|before| tabs.get(before))).map(|tab| tab.session_id.clone()),
    }
}

/// `ClosedSessionTab`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedSessionTab {
    pub tab: SessionTab,
    pub index: usize,
}

const CLOSED_SESSION_TAB_LIMIT: usize = 25;

/// `recordClosedSessionTab(stack, tab, index)`.
pub fn record_closed_session_tab(stack: &[ClosedSessionTab], tab: SessionTab, index: usize) -> Vec<ClosedSessionTab> {
    let mut next: Vec<ClosedSessionTab> =
        stack.iter().filter(|entry| entry.tab.session_id != tab.session_id).cloned().collect();
    next.push(ClosedSessionTab { tab, index });
    let start = next.len().saturating_sub(CLOSED_SESSION_TAB_LIMIT);
    next[start..].to_vec()
}

/// What `reopenSessionTab` found.
pub struct Reopened {
    pub stack: Vec<ClosedSessionTab>,
    pub tabs: Option<Vec<SessionTab>>,
    pub session_id: Option<String>,
}

/// `reopenSessionTab(stack, tabs, sessionID)`: pop the most recent closed tab that is not open, at its old index.
pub fn reopen_session_tab(stack: &[ClosedSessionTab], tabs: &[SessionTab], session_id: Option<&str>) -> Reopened {
    let mut remaining = stack.to_vec();
    while !remaining.is_empty() {
        let index = match session_id {
            Some(session_id) => remaining.iter().position(|entry| entry.tab.session_id == session_id),
            None => Some(remaining.len() - 1),
        };
        let Some(index) = index else { break };
        let entry = remaining.remove(index);
        if tabs.iter().any(|tab| tab.session_id == entry.tab.session_id) {
            continue;
        }
        let mut next = tabs.to_vec();
        next.insert(entry.index.min(tabs.len()), entry.tab.clone());
        return Reopened { stack: remaining, tabs: Some(next), session_id: Some(entry.tab.session_id) };
    }
    Reopened { stack: remaining, tabs: None, session_id: None }
}

/// `moveSessionTab(tabs, sessionID, index)`.
pub fn move_session_tab(tabs: &[SessionTab], session_id: &str, index: i64) -> Vec<SessionTab> {
    let Some(from) = tabs.iter().position(|tab| tab.session_id == session_id) else { return tabs.to_vec() };
    let to = index.clamp(0, tabs.len() as i64 - 1) as usize;
    if from == to {
        return tabs.to_vec();
    }
    let mut next: Vec<SessionTab> = tabs.iter().filter(|tab| tab.session_id != session_id).cloned().collect();
    next.insert(to, tabs[from].clone());
    next
}

/// `cycleSessionTab(tabs, active, direction, matches)`.
pub fn cycle_session_tab(
    tabs: &[SessionTab],
    active: Option<&str>,
    direction: i64,
    matches: impl Fn(&SessionTab) -> bool,
) -> Option<SessionTab> {
    if tabs.is_empty() {
        return None;
    }
    let len = tabs.len() as i64;
    let index = active.and_then(|active| tabs.iter().position(|tab| tab.session_id == active)).map(|i| i as i64).unwrap_or(-1);
    let start = if index == -1 { if direction == 1 { -1 } else { 0 } } else { index };
    (0..len)
        .map(|offset| {
            let position = (start + direction * (offset + 1) + len * 2).rem_euclid(len);
            tabs[position as usize].clone()
        })
        .find(|tab| matches(tab))
}

/// `SessionTabHistory`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTabHistory {
    pub entries: Vec<String>,
    pub index: i64,
}

impl Default for SessionTabHistory {
    fn default() -> Self {
        SessionTabHistory { entries: Vec::new(), index: -1 }
    }
}

const SESSION_TAB_HISTORY_LIMIT: usize = 100;

/// `recordSessionTabHistory(history, sessionID)`.
pub fn record_session_tab_history(history: &SessionTabHistory, session_id: &str) -> SessionTabHistory {
    let current = history.entries.get(usize::try_from(history.index).unwrap_or(usize::MAX)).map(String::as_str);
    if current == Some(session_id) {
        return history.clone();
    }
    let end = usize::try_from(history.index + 1).unwrap_or(0).min(history.entries.len());
    let mut entries: Vec<String> = history.entries[..end].to_vec();
    entries.push(session_id.to_string());
    let start = entries.len().saturating_sub(SESSION_TAB_HISTORY_LIMIT);
    let entries = entries[start..].to_vec();
    let index = entries.len() as i64 - 1;
    SessionTabHistory { entries, index }
}

/// `moveSessionTabHistory(history, tabs, active, direction)`.
pub fn move_session_tab_history(
    history: &SessionTabHistory,
    tabs: &[SessionTab],
    active: Option<&str>,
    direction: i64,
) -> (SessionTabHistory, Option<String>) {
    let Some(active) = active else {
        let session_id = history.entries.get(usize::try_from(history.index).unwrap_or(usize::MAX)).cloned();
        let present = session_id.as_deref().is_some_and(|id| tabs.iter().any(|tab| tab.session_id == id));
        return (history.clone(), present.then_some(session_id).flatten());
    };
    let candidates: Vec<(i64, &String)> = if direction == -1 {
        let end = usize::try_from(history.index).unwrap_or(0).min(history.entries.len());
        history.entries[..end].iter().enumerate().rev().map(|(index, id)| (index as i64, id)).collect()
    } else {
        let start = usize::try_from(history.index + 1).unwrap_or(0).min(history.entries.len());
        history.entries[start..].iter().enumerate().map(|(offset, id)| ((start + offset) as i64, id)).collect()
    };
    let target = candidates
        .into_iter()
        .find(|(_, id)| id.as_str() != active && tabs.iter().any(|tab| tab.session_id == id.as_str()));
    match target {
        None => (history.clone(), None),
        Some((index, id)) => (SessionTabHistory { entries: history.entries.clone(), index }, Some(id.clone())),
    }
}

/// `adaptiveSessionTabLayout`'s result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabLayout {
    pub tabs: Vec<SessionTab>,
    pub widths: Vec<i64>,
    pub before: i64,
    pub after: i64,
    pub start: i64,
    pub total: i64,
}

impl Default for TabLayout {
    fn default() -> Self {
        TabLayout { tabs: Vec::new(), widths: Vec::new(), before: 0, after: 0, start: 0, total: 0 }
    }
}

/// `Math.floor(a / b)` for a positive divisor.
fn floor_div(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

/// `adaptiveSessionTabLayout(tabs, active, available, previousStart)`.
pub fn adaptive_session_tab_layout(
    tabs: &[SessionTab],
    active: Option<&str>,
    available: i64,
    previous_start: i64,
) -> TabLayout {
    if tabs.is_empty() {
        return TabLayout::default();
    }
    let len = tabs.len() as i64;
    let active_index = active
        .and_then(|active| tabs.iter().position(|tab| tab.session_id == active))
        .map(|i| i as i64)
        .unwrap_or(-1);
    let fit = |width: i64| -> i64 {
        let value = if active_index == -1 {
            floor_div(width.max(0), SESSION_TAB_MIN_WIDTH)
        } else {
            1 + floor_div(width.max(0) - SESSION_TAB_WIDTH, SESSION_TAB_MIN_WIDTH)
        };
        len.min(value.max(1))
    };
    fn solve(
        tabs: &[SessionTab],
        available: i64,
        active_index: i64,
        fit: &dyn Fn(i64) -> i64,
        count: i64,
        start: i64,
        attempts: i64,
    ) -> (i64, i64) {
        let len = tabs.len() as i64;
        let bounded_start = start.max(0).min(len - count);
        let next_start = if active_index == -1 {
            bounded_start
        } else if active_index < bounded_start {
            active_index
        } else if active_index >= bounded_start + count {
            active_index - count + 1
        } else {
            bounded_start
        }
        .max(0)
        .min(len - count);
        let markers = (if next_start > 0 { session_tab_overflow_width(next_start) } else { 0 })
            + (if next_start + count < len { session_tab_overflow_width(len - next_start - count) } else { 0 });
        let next_count = fit(available - markers);
        if next_count == count || attempts == 0 {
            return (count, next_start);
        }
        solve(tabs, available, active_index, fit, next_count, next_start, attempts - 1)
    }
    let (count, start) = solve(tabs, available, active_index, &fit, fit(available), previous_start, 3);
    let visible: Vec<SessionTab> = tabs[start.max(0) as usize..(start + count).max(0) as usize].to_vec();
    let before = start;
    let after = len - start - count;
    let content_width = (available
        - (if before > 0 { session_tab_overflow_width(before) } else { 0 })
        - (if after > 0 { session_tab_overflow_width(after) } else { 0 }))
    .max(1);
    let roomy = content_width >= SESSION_TAB_WIDTH * visible.len() as i64;
    let total = if roomy { content_width.min(SESSION_TAB_MAX_WIDTH * visible.len() as i64) } else { content_width };
    let count = visible.len() as i64;
    if roomy || active_index == -1 {
        let width = floor_div(total, count);
        let remainder = total - width * count;
        return TabLayout {
            widths: (0..count).map(|index| width + i64::from(index < remainder)).collect(),
            tabs: visible,
            before,
            after,
            start,
            total,
        };
    }
    let inactive_width = if count == 1 {
        0
    } else {
        SESSION_TAB_WIDTH.min(
            SESSION_TAB_MIN_WIDTH.max(floor_div(total - SESSION_TAB_WIDTH.min(total), count - 1)),
        )
    };
    let active_width = if count == 1 { total } else { total - inactive_width * (count - 1) };
    TabLayout {
        widths: visible
            .iter()
            .map(|tab| if Some(tab.session_id.as_str()) == active { active_width } else { inactive_width })
            .collect(),
        tabs: visible,
        before,
        after,
        start,
        total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(ids: &[&str]) -> Vec<SessionTab> {
        ids.iter().map(|id| SessionTab::new(*id, Some(id.to_string()))).collect()
    }

    #[test]
    fn the_strip_layout_matches_the_reference_widths() {
        let list = tabs(&["a", "b", "c", "d"]);
        let layout = adaptive_session_tab_layout(&list, Some("d"), 97, 0);
        assert_eq!(layout.widths, vec![25, 24, 24, 24]);
        assert_eq!(layout.total, 97);
        let layout = adaptive_session_tab_layout(&list, Some("d"), 57, 0);
        assert_eq!(layout.widths, vec![11, 11, 11, 24]);
        let layout = adaptive_session_tab_layout(&list, Some("a"), 57, 0);
        assert_eq!(layout.widths, vec![24, 11, 11, 11]);
    }

    #[test]
    fn closing_returns_the_neighbour_that_takes_the_place() {
        let list = tabs(&["a", "b", "c"]);
        let closed = close_session_tab(&list, "b", None);
        assert_eq!(closed.tabs, tabs(&["a", "c"]));
        assert_eq!(closed.next.as_deref(), Some("c"));
        let closed = close_session_tab(&list, "c", None);
        assert_eq!(closed.next.as_deref(), Some("b"));
        let closed = close_session_tab(&list, "gone", Some(1));
        assert!(!closed.changed);
        assert_eq!(closed.next.as_deref(), Some("b"));
    }

    #[test]
    fn reopening_restores_the_old_position_and_skips_open_tabs() {
        let stack = vec![
            ClosedSessionTab { tab: tabs(&["a"])[0].clone(), index: 0 },
            ClosedSessionTab { tab: tabs(&["b"])[0].clone(), index: 1 },
        ];
        let reopened = reopen_session_tab(&stack, &tabs(&["c"]), None);
        assert_eq!(reopened.tabs, Some(tabs(&["c", "b"])));
        let reopened = reopen_session_tab(&stack, &tabs(&["b"]), None);
        assert_eq!(reopened.tabs, Some(tabs(&["a", "b"])));
    }

    #[test]
    fn history_walks_back_and_forward() {
        let mut history = SessionTabHistory::default();
        for id in ["a", "b", "c"] {
            history = record_session_tab_history(&history, id);
        }
        assert_eq!(history.entries, vec!["a", "b", "c"]);
        let list = tabs(&["a", "b", "c"]);
        let (next, target) = move_session_tab_history(&history, &list, Some("c"), -1);
        assert_eq!(target.as_deref(), Some("b"));
        assert_eq!(next.index, 1);
        let (next, target) = move_session_tab_history(&history, &list, Some("b"), -1);
        assert_eq!(target.as_deref(), Some("a"));
        let (after, target) = move_session_tab_history(&next, &list, Some("a"), 1);
        assert_eq!(target.as_deref(), Some("b"));
        assert_eq!(after.index, 1);
    }
}
