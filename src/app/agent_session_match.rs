//! Attributes agent session reports whose sender cannot name its pane.
//!
//! Some agents run hooks inside a shared background server that inherited the
//! environment of whichever pane started it, so the pane it would name is not
//! trustworthy. Such agents report a session at the start of that session's
//! first turn, so the report belongs to the only eligible pane whose turn
//! started close to it. The decision waits until the window closes, and
//! anything ambiguous binds nothing.

use std::time::{Duration, Instant};

use crate::agent_resume::AgentSessionRef;
use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;
use crate::terminal::TerminalId;

use super::actions::PaneStateUpdate;
use super::state::AppState;

const MATCH_WINDOW: Duration = Duration::from_secs(2);
/// Screen observations can still be queued when the window closes.
const DECISION_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub(crate) struct UnattributedSessionReport {
    pub agent: Agent,
    /// Panes running this agent in the reported cwd when the report arrived.
    pub eligible: Vec<PaneId>,
    pub source: String,
    pub agent_label: String,
    pub seq: Option<u64>,
    pub session_ref: AgentSessionRef,
    pub session_start_source: Option<String>,
}

#[derive(Debug)]
struct TurnStart {
    pane_id: PaneId,
    terminal_id: TerminalId,
    agent: Agent,
    at: Instant,
}

#[derive(Debug, Default)]
pub(crate) struct AgentSessionMatcher {
    turn_starts: Vec<TurnStart>,
    pending: Vec<(Instant, UnattributedSessionReport)>,
}

impl AgentSessionMatcher {
    fn record_turn_start(&mut self, start: TurnStart) {
        self.prune(start.at);
        self.turn_starts.push(start);
    }

    fn prune(&mut self, now: Instant) {
        // A report decided now may still claim turns up to one window before it.
        let keep = MATCH_WINDOW * 2 + DECISION_GRACE;
        self.turn_starts
            .retain(|start| now.saturating_duration_since(start.at) <= keep);
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .iter()
            .map(|(received, _)| *received + MATCH_WINDOW + DECISION_GRACE)
            .min()
    }

    fn take_due(&mut self, now: Instant) -> Vec<(Instant, UnattributedSessionReport)> {
        let (due, pending) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|(received, _)| now >= *received + MATCH_WINDOW + DECISION_GRACE);
        self.pending = pending;
        self.prune(now);
        due
    }

    /// Distinct panes with an eligible turn start inside the window.
    fn candidates(
        &self,
        received: Instant,
        report: &UnattributedSessionReport,
    ) -> Vec<(PaneId, TerminalId)> {
        let mut candidates: Vec<(PaneId, TerminalId)> = Vec::new();
        for start in &self.turn_starts {
            if start.agent == report.agent
                && report.eligible.contains(&start.pane_id)
                && abs_diff(start.at, received) <= MATCH_WINDOW
                && !candidates
                    .iter()
                    .any(|(pane_id, _)| *pane_id == start.pane_id)
            {
                candidates.push((start.pane_id, start.terminal_id.clone()));
            }
        }
        candidates
    }
}

fn abs_diff(a: Instant, b: Instant) -> Duration {
    a.saturating_duration_since(b)
        .max(b.saturating_duration_since(a))
}

impl AppState {
    pub(crate) fn report_unattributed_agent_session(
        &mut self,
        report: UnattributedSessionReport,
        received: Instant,
    ) -> Vec<PaneStateUpdate> {
        // A pane that already owns this session, for example one Herdr launched
        // with `resume`, keeps it regardless of which pane was active.
        if let Some(pane_id) = self.pane_owning_agent_session(&report) {
            return self
                .apply_unattributed_agent_session(pane_id, report)
                .into_iter()
                .collect();
        }
        self.agent_session_matcher.pending.push((received, report));
        Vec::new()
    }

    pub(crate) fn next_agent_session_match_deadline(&self) -> Option<Instant> {
        self.agent_session_matcher.next_deadline()
    }

    pub(crate) fn decide_due_agent_session_reports(
        &mut self,
        now: Instant,
    ) -> Vec<PaneStateUpdate> {
        let mut updates = Vec::new();
        for (received, report) in self.agent_session_matcher.take_due(now) {
            let candidates: Vec<PaneId> = self
                .agent_session_matcher
                .candidates(received, &report)
                .into_iter()
                .filter(|(pane_id, terminal_id)| {
                    self.pane_still_runs_agent(*pane_id, terminal_id, report.agent)
                })
                .map(|(pane_id, _)| pane_id)
                .collect();
            match candidates.as_slice() {
                [pane_id] => {
                    let pane_id = *pane_id;
                    updates.extend(self.apply_unattributed_agent_session(pane_id, report));
                }
                [] => {
                    tracing::info!(agent = %report.agent_label, "no pane matched agent session report")
                }
                _ => {
                    tracing::info!(agent = %report.agent_label, "dropping ambiguous agent session report")
                }
            }
        }
        updates
    }

    pub(crate) fn observe_agent_turn_starts(&mut self, updates: &[PaneStateUpdate], at: Instant) {
        for update in updates {
            let Some(agent) = update.known_agent else {
                continue;
            };
            if update.state != AgentState::Working || update.previous_state == AgentState::Working {
                continue;
            }
            let Some(terminal_id) = self
                .workspaces
                .get(update.ws_idx)
                .and_then(|ws| ws.pane_state(update.pane_id))
                .map(|pane| pane.attached_terminal_id.clone())
            else {
                continue;
            };
            self.agent_session_matcher.record_turn_start(TurnStart {
                pane_id: update.pane_id,
                terminal_id,
                agent,
                at,
            });
        }
    }

    fn pane_still_runs_agent(
        &self,
        pane_id: PaneId,
        terminal_id: &TerminalId,
        agent: Agent,
    ) -> bool {
        self.workspaces
            .iter()
            .find_map(|ws| ws.pane_state(pane_id))
            .is_some_and(|pane| &pane.attached_terminal_id == terminal_id)
            && self
                .terminals
                .get(terminal_id)
                .is_some_and(|terminal| terminal.detected_agent == Some(agent))
    }

    fn pane_owning_agent_session(&self, report: &UnattributedSessionReport) -> Option<PaneId> {
        self.workspaces.iter().find_map(|ws| {
            ws.tabs.iter().find_map(|tab| {
                tab.panes.iter().find_map(|(pane_id, pane)| {
                    self.terminals
                        .get(&pane.attached_terminal_id)
                        .is_some_and(|terminal| {
                            terminal.detected_agent == Some(report.agent)
                                && terminal
                                    .owns_agent_session(&report.agent_label, &report.session_ref)
                        })
                        .then_some(*pane_id)
                })
            })
        })
    }

    fn apply_unattributed_agent_session(
        &mut self,
        pane_id: PaneId,
        report: UnattributedSessionReport,
    ) -> Option<PaneStateUpdate> {
        tracing::info!(pane = pane_id.raw(), agent = %report.agent_label, "attributed agent session report");
        self.update_terminal_state(pane_id, |terminal| {
            terminal.set_agent_session_ref_for_session_start(
                report.source,
                report.agent_label,
                Some(report.session_ref),
                report.seq,
                report.session_start_source,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(n: u32) -> PaneId {
        PaneId::from_raw(n)
    }

    fn report(eligible: &[u32]) -> UnattributedSessionReport {
        UnattributedSessionReport {
            agent: Agent::Codex,
            eligible: eligible.iter().copied().map(pane).collect(),
            source: "herdr:codex".into(),
            agent_label: "codex".into(),
            seq: None,
            session_ref: AgentSessionRef::id("session").unwrap(),
            session_start_source: None,
        }
    }

    fn start(matcher: &mut AgentSessionMatcher, n: u32, at: Instant) {
        matcher.record_turn_start(TurnStart {
            pane_id: pane(n),
            terminal_id: TerminalId::alloc(),
            agent: Agent::Codex,
            at,
        });
    }

    fn candidate_panes(
        matcher: &AgentSessionMatcher,
        received: Instant,
        report: &UnattributedSessionReport,
    ) -> Vec<PaneId> {
        matcher
            .candidates(received, report)
            .into_iter()
            .map(|(pane_id, _)| pane_id)
            .collect()
    }

    #[test]
    fn turn_start_on_either_side_of_the_report_is_a_candidate() {
        let t = Instant::now() + Duration::from_secs(10);
        let mut matcher = AgentSessionMatcher::default();
        start(&mut matcher, 1, t - Duration::from_millis(300));
        assert_eq!(candidate_panes(&matcher, t, &report(&[1, 2])), [pane(1)]);

        let mut matcher = AgentSessionMatcher::default();
        start(&mut matcher, 2, t + Duration::from_millis(300));
        assert_eq!(candidate_panes(&matcher, t, &report(&[1, 2])), [pane(2)]);
    }

    #[test]
    fn a_later_turn_inside_the_window_makes_the_report_ambiguous() {
        let t = Instant::now() + Duration::from_secs(10);
        let mut matcher = AgentSessionMatcher::default();
        start(&mut matcher, 1, t + Duration::from_millis(100));
        start(&mut matcher, 2, t + Duration::from_millis(200));
        assert_eq!(candidate_panes(&matcher, t, &report(&[1, 2])).len(), 2);
    }

    #[test]
    fn ineligible_or_stale_turns_are_not_candidates() {
        let t = Instant::now() + Duration::from_secs(10);
        let mut matcher = AgentSessionMatcher::default();
        start(&mut matcher, 1, t);
        start(&mut matcher, 2, t - MATCH_WINDOW - Duration::from_millis(1));
        assert!(candidate_panes(&matcher, t, &report(&[2, 3])).is_empty());
    }

    #[test]
    fn repeated_turns_in_one_pane_are_one_candidate() {
        let t = Instant::now() + Duration::from_secs(10);
        let mut matcher = AgentSessionMatcher::default();
        start(&mut matcher, 1, t);
        start(&mut matcher, 1, t + Duration::from_millis(500));
        assert_eq!(candidate_panes(&matcher, t, &report(&[1])), [pane(1)]);
    }

    mod state {
        use super::*;
        use crate::events::AppEvent;
        use crate::workspace::Workspace;

        fn app_with_codex_panes(count: usize) -> (AppState, Vec<PaneId>) {
            let mut state = AppState::test_new();
            for n in 0..count {
                state
                    .workspaces
                    .push(Workspace::test_new(&format!("ws{n}")));
            }
            state.ensure_test_terminals();
            let panes: Vec<PaneId> = state
                .workspaces
                .iter()
                .map(|ws| ws.tabs[0].root_pane)
                .collect();
            for pane_id in &panes {
                set_state(&mut state, *pane_id, AgentState::Idle, Instant::now());
            }
            (state, panes)
        }

        fn set_state(state: &mut AppState, pane_id: PaneId, agent_state: AgentState, at: Instant) {
            state.handle_app_event(AppEvent::StateChanged {
                pane_id,
                agent: Some(Agent::Codex),
                state: agent_state,
                visible_blocker: false,
                visible_working: agent_state == AgentState::Working,
                process_exited: false,
                observed_at: at,
            });
        }

        fn session_of(state: &AppState, pane_id: PaneId) -> Option<String> {
            let pane = state
                .workspaces
                .iter()
                .find_map(|ws| ws.pane_state(pane_id))?;
            state.terminals[&pane.attached_terminal_id]
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.clone())
        }

        fn report_for(panes: &[PaneId], session: &str) -> UnattributedSessionReport {
            UnattributedSessionReport {
                eligible: panes.to_vec(),
                session_ref: AgentSessionRef::id(session).unwrap(),
                session_start_source: Some("startup".into()),
                ..report(&[])
            }
        }

        fn decide(state: &mut AppState, received: Instant) {
            state.decide_due_agent_session_reports(received + MATCH_WINDOW + DECISION_GRACE);
        }

        #[test]
        fn binds_the_pane_whose_turn_started_with_the_report() {
            let (mut state, panes) = app_with_codex_panes(2);
            let t = Instant::now();
            set_state(&mut state, panes[1], AgentState::Working, t);
            state.report_unattributed_agent_session(report_for(&panes, "s-1"), t);
            decide(&mut state, t);
            assert_eq!(session_of(&state, panes[0]), None);
            assert_eq!(session_of(&state, panes[1]).as_deref(), Some("s-1"));
        }

        #[test]
        fn pane_whose_agent_left_cannot_claim_a_report() {
            let (mut state, panes) = app_with_codex_panes(2);
            let t = Instant::now();
            set_state(&mut state, panes[0], AgentState::Working, t);
            state.report_unattributed_agent_session(report_for(&panes, "s-1"), t);
            state.handle_app_event(AppEvent::StateChanged {
                pane_id: panes[0],
                agent: None,
                state: AgentState::Unknown,
                visible_blocker: false,
                visible_working: false,
                process_exited: true,
                observed_at: t,
            });
            decide(&mut state, t);
            assert_eq!(session_of(&state, panes[0]), None);
        }

        #[test]
        fn existing_session_owner_wins_over_timing() {
            let (mut state, panes) = app_with_codex_panes(2);
            let pane = state.workspaces[0].pane_state(panes[0]).unwrap();
            state
                .terminals
                .get_mut(&pane.attached_terminal_id.clone())
                .unwrap()
                .set_managed_agent_launch_session(crate::agent_resume::PersistedAgentSession {
                    source: "herdr:codex".into(),
                    agent: "codex".into(),
                    session_ref: AgentSessionRef::id("resumed").unwrap(),
                });
            let t = Instant::now();
            set_state(&mut state, panes[1], AgentState::Working, t);
            state.report_unattributed_agent_session(report_for(&panes, "resumed"), t);
            decide(&mut state, t);
            assert_eq!(session_of(&state, panes[0]).as_deref(), Some("resumed"));
            assert_eq!(session_of(&state, panes[1]), None);
        }
    }

    #[test]
    fn reports_are_decided_only_after_the_window_closes() {
        let t = Instant::now();
        let mut matcher = AgentSessionMatcher::default();
        matcher.pending.push((t, report(&[1])));
        let deadline = matcher.next_deadline().unwrap();
        assert!(deadline >= t + MATCH_WINDOW);
        assert!(matcher
            .take_due(deadline - Duration::from_millis(1))
            .is_empty());
        assert_eq!(matcher.take_due(deadline).len(), 1);
        assert!(matcher.next_deadline().is_none());
    }
}
