use std::collections::HashMap;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RunState {
    Created,
    Queued,
    Scheduled,
    Running,
    Waiting,
    Paused,
    Blocked,
    Retrying,
    Success,
    Failed,
    Timeout,
    Cancelled,
    Skipped,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct StateChangedPayload {
    pub from: RunState,
    pub to: RunState,
}

#[derive(Debug, Default)]
pub struct StateEngine {
    runs: HashMap<Uuid, RunState>,
}

impl StateEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current(&self, run_id: Uuid) -> Option<RunState> {
        self.runs.get(&run_id).copied()
    }

    pub fn transition(&mut self, run_id: Uuid, to: RunState) -> Result<StateChangedPayload> {
        let from = self.current(run_id).unwrap_or(RunState::Created);
        if !is_transition_allowed(from, to) {
            bail!("illegal run state transition: {from:?} -> {to:?}");
        }

        self.runs.insert(run_id, to);
        Ok(StateChangedPayload { from, to })
    }

    pub fn apply_state_changed(
        &mut self,
        run_id: Uuid,
        payload: StateChangedPayload,
    ) -> Result<()> {
        let current = self.current(run_id).unwrap_or(RunState::Created);
        if current != payload.from {
            bail!(
                "state mismatch for run {run_id}: expected {:?}, got {:?}",
                payload.from,
                current
            );
        }

        if !is_transition_allowed(payload.from, payload.to) {
            bail!(
                "illegal run state transition: {:?} -> {:?}",
                payload.from,
                payload.to
            );
        }

        self.runs.insert(run_id, payload.to);
        Ok(())
    }
}

pub fn is_transition_allowed(from: RunState, to: RunState) -> bool {
    use RunState::*;

    match from {
        Created => matches!(to, Queued | Scheduled | Running | Cancelled),
        Queued => matches!(to, Running | Cancelled | Skipped),
        Scheduled => matches!(to, Queued | Running | Cancelled),
        Running => matches!(
            to,
            Waiting | Paused | Blocked | Retrying | Success | Failed | Timeout | Cancelled
        ),
        Waiting => matches!(to, Running | Timeout | Cancelled),
        Paused => matches!(to, Running | Cancelled),
        Blocked => matches!(to, Running | Failed | Cancelled),
        Retrying => matches!(to, Running | Failed | Cancelled),
        Success | Failed | Timeout | Cancelled | Skipped => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_valid_transitions() {
        let mut engine = StateEngine::new();
        let run_id = Uuid::new_v4();

        assert_eq!(
            engine.transition(run_id, RunState::Running).unwrap(),
            StateChangedPayload {
                from: RunState::Created,
                to: RunState::Running,
            }
        );
        assert_eq!(
            engine.transition(run_id, RunState::Success).unwrap(),
            StateChangedPayload {
                from: RunState::Running,
                to: RunState::Success,
            }
        );
        assert_eq!(engine.current(run_id), Some(RunState::Success));
    }

    #[test]
    fn rejects_illegal_transitions() {
        let mut engine = StateEngine::new();
        let run_id = Uuid::new_v4();

        engine.transition(run_id, RunState::Running).unwrap();
        engine.transition(run_id, RunState::Success).unwrap();

        assert!(engine.transition(run_id, RunState::Running).is_err());
    }
}
