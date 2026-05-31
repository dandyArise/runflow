use std::collections::VecDeque;
use std::str::FromStr;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use cron::Schedule;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyPolicy {
    Allow,
    Forbid,
    Queue,
    Replace,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ConcurrencyDecision {
    StartNow,
    Reject,
    Queue,
    Replace { cancelled_run_id: Uuid },
}

#[derive(Debug)]
pub struct Scheduler {
    schedule: Schedule,
}

#[derive(Debug)]
pub struct ConcurrencyController {
    policy: ConcurrencyPolicy,
    active_run_id: Option<Uuid>,
    queued_run_ids: VecDeque<Uuid>,
}

impl Scheduler {
    pub fn parse(expression: &str) -> Result<Self> {
        let normalized = normalize_cron_expression(expression);
        let schedule = Schedule::from_str(&normalized)
            .with_context(|| format!("invalid cron expression: {expression}"))?;
        Ok(Self { schedule })
    }

    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.schedule.after(&after).next()
    }

    pub fn upcoming_after(&self, after: DateTime<Utc>, count: usize) -> Vec<DateTime<Utc>> {
        self.schedule.after(&after).take(count).collect()
    }
}

impl ConcurrencyController {
    pub fn new(policy: ConcurrencyPolicy) -> Self {
        Self {
            policy,
            active_run_id: None,
            queued_run_ids: VecDeque::new(),
        }
    }

    pub fn request_run(&mut self, run_id: Uuid) -> ConcurrencyDecision {
        match (self.policy, self.active_run_id) {
            (ConcurrencyPolicy::Allow, _) => {
                self.active_run_id = Some(run_id);
                ConcurrencyDecision::StartNow
            }
            (_, None) => {
                self.active_run_id = Some(run_id);
                ConcurrencyDecision::StartNow
            }
            (ConcurrencyPolicy::Forbid, Some(_)) => ConcurrencyDecision::Reject,
            (ConcurrencyPolicy::Queue, Some(_)) => {
                self.queued_run_ids.push_back(run_id);
                ConcurrencyDecision::Queue
            }
            (ConcurrencyPolicy::Replace, Some(active)) => {
                self.active_run_id = Some(run_id);
                ConcurrencyDecision::Replace {
                    cancelled_run_id: active,
                }
            }
        }
    }

    pub fn finish_active(&mut self) -> Option<Uuid> {
        self.active_run_id = self.queued_run_ids.pop_front();
        self.active_run_id
    }

    pub fn queued_len(&self) -> usize {
        self.queued_run_ids.len()
    }
}

fn normalize_cron_expression(expression: &str) -> String {
    let fields = expression.split_whitespace().count();
    if fields == 5 {
        format!("0 {expression}")
    } else {
        expression.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_five_field_cron_and_returns_next_run() {
        let scheduler = Scheduler::parse("0 1 * * *").unwrap();
        let next = scheduler
            .next_after("2026-05-29T00:00:00Z".parse::<DateTime<Utc>>().unwrap())
            .unwrap();

        assert_eq!(
            next,
            "2026-05-29T01:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn parses_seven_field_cron_and_returns_multiple_runs() {
        let scheduler = Scheduler::parse("0 */5 * * * * *").unwrap();
        let next =
            scheduler.upcoming_after("2026-05-31T14:00:00Z".parse::<DateTime<Utc>>().unwrap(), 3);

        assert_eq!(
            next,
            vec![
                "2026-05-31T14:05:00Z".parse::<DateTime<Utc>>().unwrap(),
                "2026-05-31T14:10:00Z".parse::<DateTime<Utc>>().unwrap(),
                "2026-05-31T14:15:00Z".parse::<DateTime<Utc>>().unwrap(),
            ]
        );
    }

    #[test]
    fn applies_queue_policy() {
        let mut controller = ConcurrencyController::new(ConcurrencyPolicy::Queue);
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();

        assert_eq!(controller.request_run(first), ConcurrencyDecision::StartNow);
        assert_eq!(controller.request_run(second), ConcurrencyDecision::Queue);
        assert_eq!(controller.queued_len(), 1);
        assert_eq!(controller.finish_active(), Some(second));
    }

    #[test]
    fn applies_replace_policy() {
        let mut controller = ConcurrencyController::new(ConcurrencyPolicy::Replace);
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();

        assert_eq!(controller.request_run(first), ConcurrencyDecision::StartNow);
        assert_eq!(
            controller.request_run(second),
            ConcurrencyDecision::Replace {
                cancelled_run_id: first
            }
        );
    }
}
