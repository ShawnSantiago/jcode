use async_trait::async_trait;
use jcode_plan::{PlanGraphSummary, PlanItem, summarize_plan_graph};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RalplanReviewVerdict {
    Approve,
    Iterate,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RalplanDraft {
    pub plan_items: Vec<PlanItem>,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RalplanReview {
    pub verdict: RalplanReviewVerdict,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_changes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RalplanRunOptions {
    pub task: String,
    pub max_iterations: u32,
}

impl RalplanRunOptions {
    pub fn new(task: impl Into<String>) -> Self {
        Self {
            task: task.into(),
            max_iterations: 3,
        }
    }

    fn normalized(self) -> Result<Self, String> {
        let task = self.task.trim().to_string();
        if task.is_empty() {
            return Err("ralplan task must not be empty".to_string());
        }
        Ok(Self {
            task,
            max_iterations: self.max_iterations.max(1),
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RalplanTerminalStatus {
    Approved,
    Rejected,
    MaxIterationsReached,
    InvalidPlan,
    ExecutorFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RalplanRunResult {
    pub status: RalplanTerminalStatus,
    pub iterations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_draft: Option<RalplanDraft>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub architect_reviews: Vec<RalplanReview>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub critic_reviews: Vec<RalplanReview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_summary: Option<PlanGraphSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct RalplanDraftContext<'a> {
    pub task: &'a str,
    pub iteration: u32,
    pub previous_draft: Option<&'a RalplanDraft>,
    pub architect_reviews: &'a [RalplanReview],
    pub critic_reviews: &'a [RalplanReview],
}

pub struct RalplanReviewContext<'a> {
    pub task: &'a str,
    pub iteration: u32,
    pub draft: &'a RalplanDraft,
    pub prior_architect_reviews: &'a [RalplanReview],
    pub prior_critic_reviews: &'a [RalplanReview],
}

#[async_trait]
pub trait RalplanConsensusExecutor {
    async fn draft(&mut self, ctx: RalplanDraftContext<'_>) -> anyhow::Result<RalplanDraft>;

    async fn architect_review(
        &mut self,
        ctx: RalplanReviewContext<'_>,
    ) -> anyhow::Result<RalplanReview>;

    async fn critic_review(
        &mut self,
        ctx: RalplanReviewContext<'_>,
    ) -> anyhow::Result<RalplanReview>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RalplanPlanValidationError {
    EmptyPlan,
    DuplicateIds(Vec<String>),
    Cycles(Vec<String>),
    UnresolvedDependencies(Vec<String>),
    NoRunnableItems,
}

impl std::fmt::Display for RalplanPlanValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPlan => write!(f, "plan must contain at least one item"),
            Self::DuplicateIds(ids) => {
                write!(f, "plan contains duplicate item ids: {}", ids.join(", "))
            }
            Self::Cycles(ids) => write!(
                f,
                "plan contains dependency cycles involving: {}",
                ids.join(", ")
            ),
            Self::UnresolvedDependencies(ids) => {
                write!(
                    f,
                    "plan contains unresolved dependencies: {}",
                    ids.join(", ")
                )
            }
            Self::NoRunnableItems => write!(f, "plan must contain at least one runnable item"),
        }
    }
}

impl std::error::Error for RalplanPlanValidationError {}

pub fn validate_ralplan_plan(
    items: &[PlanItem],
) -> Result<PlanGraphSummary, RalplanPlanValidationError> {
    if items.is_empty() {
        return Err(RalplanPlanValidationError::EmptyPlan);
    }

    let duplicate_ids = duplicate_plan_item_ids(items);
    if !duplicate_ids.is_empty() {
        return Err(RalplanPlanValidationError::DuplicateIds(duplicate_ids));
    }

    let summary = summarize_plan_graph(items);
    if !summary.cycle_ids.is_empty() {
        return Err(RalplanPlanValidationError::Cycles(summary.cycle_ids));
    }
    if !summary.unresolved_dependency_ids.is_empty() {
        return Err(RalplanPlanValidationError::UnresolvedDependencies(
            summary.unresolved_dependency_ids,
        ));
    }
    if summary.ready_ids.is_empty() {
        return Err(RalplanPlanValidationError::NoRunnableItems);
    }

    Ok(summary)
}

pub async fn run_ralplan_consensus<E>(
    executor: &mut E,
    options: RalplanRunOptions,
) -> RalplanRunResult
where
    E: RalplanConsensusExecutor + Send,
{
    let options = match options.normalized() {
        Ok(options) => options,
        Err(error) => return failure_result(RalplanTerminalStatus::InvalidPlan, 0, error),
    };

    let mut final_draft: Option<RalplanDraft> = None;
    let mut architect_reviews = Vec::new();
    let mut critic_reviews = Vec::new();
    let mut validation_summary = None;

    for iteration in 1..=options.max_iterations {
        let draft = match executor
            .draft(RalplanDraftContext {
                task: &options.task,
                iteration,
                previous_draft: final_draft.as_ref(),
                architect_reviews: &architect_reviews,
                critic_reviews: &critic_reviews,
            })
            .await
        {
            Ok(draft) => draft,
            Err(error) => {
                return RalplanRunResult {
                    status: RalplanTerminalStatus::ExecutorFailed,
                    iterations: iteration,
                    final_draft,
                    architect_reviews,
                    critic_reviews,
                    validation_summary,
                    error: Some(error.to_string()),
                };
            }
        };

        let summary = match validate_ralplan_plan(&draft.plan_items) {
            Ok(summary) => summary,
            Err(error) => {
                return RalplanRunResult {
                    status: RalplanTerminalStatus::InvalidPlan,
                    iterations: iteration,
                    final_draft: Some(draft),
                    architect_reviews,
                    critic_reviews,
                    validation_summary: None,
                    error: Some(error.to_string()),
                };
            }
        };
        validation_summary = Some(summary);

        let architect_review = match executor
            .architect_review(RalplanReviewContext {
                task: &options.task,
                iteration,
                draft: &draft,
                prior_architect_reviews: &architect_reviews,
                prior_critic_reviews: &critic_reviews,
            })
            .await
        {
            Ok(review) => review,
            Err(error) => {
                return RalplanRunResult {
                    status: RalplanTerminalStatus::ExecutorFailed,
                    iterations: iteration,
                    final_draft: Some(draft),
                    architect_reviews,
                    critic_reviews,
                    validation_summary,
                    error: Some(error.to_string()),
                };
            }
        };
        architect_reviews.push(architect_review);

        let critic_review = match executor
            .critic_review(RalplanReviewContext {
                task: &options.task,
                iteration,
                draft: &draft,
                prior_architect_reviews: &architect_reviews,
                prior_critic_reviews: &critic_reviews,
            })
            .await
        {
            Ok(review) => review,
            Err(error) => {
                return RalplanRunResult {
                    status: RalplanTerminalStatus::ExecutorFailed,
                    iterations: iteration,
                    final_draft: Some(draft),
                    architect_reviews,
                    critic_reviews,
                    validation_summary,
                    error: Some(error.to_string()),
                };
            }
        };
        let verdict = critic_review.verdict;
        critic_reviews.push(critic_review);
        final_draft = Some(draft);

        match verdict {
            RalplanReviewVerdict::Approve => {
                return RalplanRunResult {
                    status: RalplanTerminalStatus::Approved,
                    iterations: iteration,
                    final_draft,
                    architect_reviews,
                    critic_reviews,
                    validation_summary,
                    error: None,
                };
            }
            RalplanReviewVerdict::Reject => {
                return RalplanRunResult {
                    status: RalplanTerminalStatus::Rejected,
                    iterations: iteration,
                    final_draft,
                    architect_reviews,
                    critic_reviews,
                    validation_summary,
                    error: None,
                };
            }
            RalplanReviewVerdict::Iterate => {}
        }
    }

    RalplanRunResult {
        status: RalplanTerminalStatus::MaxIterationsReached,
        iterations: options.max_iterations,
        final_draft,
        architect_reviews,
        critic_reviews,
        validation_summary,
        error: None,
    }
}

fn duplicate_plan_item_ids(items: &[PlanItem]) -> Vec<String> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for item in items {
        *counts.entry(item.id.as_str()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .filter_map(|(id, count)| (count > 1).then_some(id.to_string()))
        .collect()
}

fn failure_result(
    status: RalplanTerminalStatus,
    iterations: u32,
    error: String,
) -> RalplanRunResult {
    RalplanRunResult {
        status,
        iterations,
        final_draft: None,
        architect_reviews: Vec::new(),
        critic_reviews: Vec::new(),
        validation_summary: None,
        error: Some(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, status: &str, blocked_by: &[&str]) -> PlanItem {
        PlanItem {
            content: format!("task {id}"),
            status: status.to_string(),
            priority: "medium".to_string(),
            id: id.to_string(),
            subsystem: None,
            file_scope: Vec::new(),
            blocked_by: blocked_by.iter().map(|value| value.to_string()).collect(),
            assigned_to: None,
        }
    }

    fn draft(items: Vec<PlanItem>) -> RalplanDraft {
        RalplanDraft {
            plan_items: items,
            summary: "draft summary".to_string(),
            notes: Vec::new(),
        }
    }

    fn review(verdict: RalplanReviewVerdict) -> RalplanReview {
        RalplanReview {
            verdict,
            summary: format!("{verdict:?}"),
            required_changes: Vec::new(),
        }
    }

    #[test]
    fn validate_plan_rejects_empty_plan() {
        assert_eq!(
            validate_ralplan_plan(&[]).unwrap_err(),
            RalplanPlanValidationError::EmptyPlan
        );
    }

    #[test]
    fn validate_plan_rejects_duplicate_ids() {
        assert_eq!(
            validate_ralplan_plan(&[item("a", "queued", &[]), item("a", "queued", &[])])
                .unwrap_err(),
            RalplanPlanValidationError::DuplicateIds(vec!["a".to_string()])
        );
    }

    #[test]
    fn validate_plan_rejects_cycles() {
        assert_eq!(
            validate_ralplan_plan(&[item("a", "queued", &["b"]), item("b", "queued", &["a"])])
                .unwrap_err(),
            RalplanPlanValidationError::Cycles(vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn validate_plan_rejects_unresolved_dependencies() {
        assert_eq!(
            validate_ralplan_plan(&[item("a", "queued", &["missing"])]).unwrap_err(),
            RalplanPlanValidationError::UnresolvedDependencies(vec!["missing".to_string()])
        );
    }

    #[test]
    fn validate_plan_accepts_ready_graph() {
        let summary =
            validate_ralplan_plan(&[item("a", "queued", &[]), item("b", "queued", &["a"])])
                .unwrap();
        assert_eq!(summary.ready_ids, vec!["a".to_string()]);
        assert_eq!(summary.blocked_ids, vec!["b".to_string()]);
    }

    #[test]
    fn validate_plan_rejects_no_runnable_items() {
        assert_eq!(
            validate_ralplan_plan(&[item("a", "completed", &[])]).unwrap_err(),
            RalplanPlanValidationError::NoRunnableItems
        );
    }

    #[derive(Default)]
    struct FakeExecutor {
        calls: Vec<String>,
        critic_verdicts: Vec<RalplanReviewVerdict>,
        draft_error_at: Option<u32>,
        architect_error_at: Option<u32>,
        critic_error_at: Option<u32>,
        invalid_plan_at: Option<u32>,
    }

    impl FakeExecutor {
        fn with_verdicts(critic_verdicts: Vec<RalplanReviewVerdict>) -> Self {
            Self {
                critic_verdicts,
                ..Self::default()
            }
        }
    }

    #[async_trait]
    impl RalplanConsensusExecutor for FakeExecutor {
        async fn draft(&mut self, ctx: RalplanDraftContext<'_>) -> anyhow::Result<RalplanDraft> {
            self.calls.push(format!("draft:{}", ctx.iteration));
            if self.draft_error_at == Some(ctx.iteration) {
                anyhow::bail!("draft failed at {}", ctx.iteration);
            }
            if self.invalid_plan_at == Some(ctx.iteration) {
                return Ok(draft(Vec::new()));
            }
            Ok(draft(vec![item("a", "queued", &[])]))
        }

        async fn architect_review(
            &mut self,
            ctx: RalplanReviewContext<'_>,
        ) -> anyhow::Result<RalplanReview> {
            self.calls.push(format!("architect:{}", ctx.iteration));
            if self.architect_error_at == Some(ctx.iteration) {
                anyhow::bail!("architect failed at {}", ctx.iteration);
            }
            Ok(review(RalplanReviewVerdict::Approve))
        }

        async fn critic_review(
            &mut self,
            ctx: RalplanReviewContext<'_>,
        ) -> anyhow::Result<RalplanReview> {
            self.calls.push(format!("critic:{}", ctx.iteration));
            if self.critic_error_at == Some(ctx.iteration) {
                anyhow::bail!("critic failed at {}", ctx.iteration);
            }
            let verdict = self
                .critic_verdicts
                .get((ctx.iteration - 1) as usize)
                .copied()
                .unwrap_or(RalplanReviewVerdict::Iterate);
            Ok(review(verdict))
        }
    }

    #[tokio::test]
    async fn ralplan_calls_architect_before_critic() {
        let mut executor = FakeExecutor::with_verdicts(vec![RalplanReviewVerdict::Approve]);
        let result = run_ralplan_consensus(&mut executor, RalplanRunOptions::new("task")).await;
        assert_eq!(result.status, RalplanTerminalStatus::Approved);
        assert_eq!(executor.calls, vec!["draft:1", "architect:1", "critic:1"]);
    }

    #[tokio::test]
    async fn ralplan_approves_on_first_iteration() {
        let mut executor = FakeExecutor::with_verdicts(vec![RalplanReviewVerdict::Approve]);
        let result = run_ralplan_consensus(&mut executor, RalplanRunOptions::new("task")).await;
        assert_eq!(result.status, RalplanTerminalStatus::Approved);
        assert_eq!(result.iterations, 1);
        assert_eq!(result.architect_reviews.len(), 1);
        assert_eq!(result.critic_reviews.len(), 1);
        assert!(result.final_draft.is_some());
    }

    #[tokio::test]
    async fn ralplan_iterates_until_approval() {
        let mut executor = FakeExecutor::with_verdicts(vec![
            RalplanReviewVerdict::Iterate,
            RalplanReviewVerdict::Approve,
        ]);
        let result = run_ralplan_consensus(&mut executor, RalplanRunOptions::new("task")).await;
        assert_eq!(result.status, RalplanTerminalStatus::Approved);
        assert_eq!(result.iterations, 2);
        assert_eq!(result.architect_reviews.len(), 2);
        assert_eq!(result.critic_reviews.len(), 2);
        assert_eq!(
            executor.calls,
            vec![
                "draft:1",
                "architect:1",
                "critic:1",
                "draft:2",
                "architect:2",
                "critic:2"
            ]
        );
    }

    #[tokio::test]
    async fn ralplan_stops_at_max_iterations() {
        let mut executor = FakeExecutor::with_verdicts(vec![RalplanReviewVerdict::Iterate; 5]);
        let mut options = RalplanRunOptions::new("task");
        options.max_iterations = 3;
        let result = run_ralplan_consensus(&mut executor, options).await;
        assert_eq!(result.status, RalplanTerminalStatus::MaxIterationsReached);
        assert_eq!(result.iterations, 3);
        assert_eq!(result.architect_reviews.len(), 3);
        assert_eq!(result.critic_reviews.len(), 3);
    }

    #[tokio::test]
    async fn ralplan_rejects_on_critic_reject() {
        let mut executor = FakeExecutor::with_verdicts(vec![RalplanReviewVerdict::Reject]);
        let result = run_ralplan_consensus(&mut executor, RalplanRunOptions::new("task")).await;
        assert_eq!(result.status, RalplanTerminalStatus::Rejected);
        assert_eq!(result.iterations, 1);
    }

    #[tokio::test]
    async fn ralplan_returns_executor_failed_on_draft_error() {
        let mut executor = FakeExecutor {
            draft_error_at: Some(1),
            ..FakeExecutor::default()
        };
        let result = run_ralplan_consensus(&mut executor, RalplanRunOptions::new("task")).await;
        assert_eq!(result.status, RalplanTerminalStatus::ExecutorFailed);
        assert_eq!(result.iterations, 1);
        assert!(result.error.unwrap().contains("draft failed"));
    }

    #[tokio::test]
    async fn ralplan_returns_executor_failed_on_architect_error() {
        let mut executor = FakeExecutor {
            architect_error_at: Some(1),
            ..FakeExecutor::default()
        };
        let result = run_ralplan_consensus(&mut executor, RalplanRunOptions::new("task")).await;
        assert_eq!(result.status, RalplanTerminalStatus::ExecutorFailed);
        assert_eq!(result.iterations, 1);
        assert!(result.error.unwrap().contains("architect failed"));
        assert!(result.final_draft.is_some());
        assert!(result.architect_reviews.is_empty());
    }

    #[tokio::test]
    async fn ralplan_returns_executor_failed_on_critic_error() {
        let mut executor = FakeExecutor {
            critic_error_at: Some(1),
            ..FakeExecutor::default()
        };
        let result = run_ralplan_consensus(&mut executor, RalplanRunOptions::new("task")).await;
        assert_eq!(result.status, RalplanTerminalStatus::ExecutorFailed);
        assert_eq!(result.iterations, 1);
        assert!(result.error.unwrap().contains("critic failed"));
        assert!(result.final_draft.is_some());
        assert_eq!(result.architect_reviews.len(), 1);
        assert!(result.critic_reviews.is_empty());
    }

    #[tokio::test]
    async fn ralplan_returns_invalid_plan_for_bad_draft() {
        let mut executor = FakeExecutor {
            invalid_plan_at: Some(1),
            ..FakeExecutor::default()
        };
        let result = run_ralplan_consensus(&mut executor, RalplanRunOptions::new("task")).await;
        assert_eq!(result.status, RalplanTerminalStatus::InvalidPlan);
        assert_eq!(result.iterations, 1);
        assert!(result.error.unwrap().contains("plan must contain"));
        assert!(result.architect_reviews.is_empty());
        assert!(result.critic_reviews.is_empty());
    }

    #[tokio::test]
    async fn ralplan_normalizes_empty_task_to_invalid_plan() {
        let mut executor = FakeExecutor::default();
        let result = run_ralplan_consensus(&mut executor, RalplanRunOptions::new("   ")).await;
        assert_eq!(result.status, RalplanTerminalStatus::InvalidPlan);
        assert_eq!(result.iterations, 0);
        assert!(result.error.unwrap().contains("must not be empty"));
        assert!(executor.calls.is_empty());
    }

    #[tokio::test]
    async fn ralplan_max_iterations_minimum_is_one() {
        let mut executor = FakeExecutor::with_verdicts(vec![RalplanReviewVerdict::Iterate]);
        let mut options = RalplanRunOptions::new("task");
        options.max_iterations = 0;
        let result = run_ralplan_consensus(&mut executor, options).await;
        assert_eq!(result.status, RalplanTerminalStatus::MaxIterationsReached);
        assert_eq!(result.iterations, 1);
    }

    #[test]
    fn duplicate_ids_are_sorted() {
        assert_eq!(
            duplicate_plan_item_ids(&[
                item("b", "queued", &[]),
                item("a", "queued", &[]),
                item("b", "queued", &[]),
                item("a", "queued", &[]),
            ]),
            vec!["a".to_string(), "b".to_string()]
        );
    }
}
