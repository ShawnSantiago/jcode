//! Listwise LLM reranking of memory retrieval candidates (recall-5, Mode-2).
//!
//! Hybrid retrieval (`MemoryManager::find_similar_hybrid`) reliably pulls the
//! relevant memories into a top-N candidate pool, but ranks them poorly: on the
//! recall benchmark the pool holds ~99% of relevant memories yet only ~53% reach
//! the top 5. A reranker that reorders the existing pool closes most of that gap.
//!
//! A local cross-encoder (MS-MARCO) was tried and *hurt* recall (out-of-domain
//! for memory statements, and it chokes on noisy multi-message context). A
//! listwise LLM reranker, fed the *focused* query (latest user intent, with
//! system-reminder/tool noise stripped) and all candidates in one call, lifts
//! benchmark recall@5 0.53 -> 0.75 and precision@5 0.23 -> 0.35.
//!
//! This module is the single source of truth for that reranking, shared by the
//! offline benchmark (`memory_recall_bench`) and the live memory agent so the
//! shipped behavior matches what was measured. It is pure with respect to the
//! memory agent (depends only on `Sidecar` + `MemoryEntry`).

use std::collections::HashSet;

use crate::memory_types::MemoryEntry;
use crate::sidecar::Sidecar;

/// System prompt instructing the model to rank candidates by usefulness.
pub const LLM_RERANK_SYSTEM: &str = "You re-rank stored MEMORIES by how useful each would be to surface to an AI coding agent for the CURRENT request. \
Order them best-first: a memory ranks high if a competent engineer would say knowing it specifically helps respond here (a relevant fact, preference, correction, or procedure). \
Off-topic, generic, or keyword-only matches rank low. \
Reply with ONLY a JSON array of candidate numbers, best first, e.g. [3,1,7]. Include only clearly useful candidates; omit ones that are not relevant. No prose.";

/// Cap the query length fed to the reranker. The query should already be the
/// focused (noise-stripped) view; this is a defensive bound. We keep the TAIL,
/// which carries the most recent intent.
const MAX_QUERY_CHARS: usize = 4000;

/// Per-candidate content cap so a single huge memory cannot dominate the prompt.
const MAX_CANDIDATE_CHARS: usize = 600;

fn truncate_tail(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    s.chars().skip(count - max).collect()
}

fn truncate_head(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Build the listwise rerank prompt from a focused query and `(id, content)`
/// candidate pairs. Candidates are presented as a 1-based numbered list.
pub fn build_rerank_prompt(focused_query: &str, candidates: &[(String, String)]) -> String {
    let q = truncate_tail(focused_query, MAX_QUERY_CHARS);
    let mut p = String::with_capacity(256 + candidates.len() * 64);
    p.push_str("CURRENT REQUEST:\n");
    p.push_str(&q);
    p.push_str("\n\nCANDIDATE MEMORIES:\n");
    for (i, (_id, content)) in candidates.iter().enumerate() {
        let one_line = truncate_head(content, MAX_CANDIDATE_CHARS).replace('\n', " ");
        p.push_str(&format!("{}. {}\n", i + 1, one_line));
    }
    p.push_str("\nReturn candidate numbers ranked best-first as a JSON array.");
    p
}

/// Parse a ranked JSON array of 1-based candidate numbers into 0-based indices,
/// preserving order and dropping out-of-range / duplicate entries. Tolerates
/// surrounding prose by extracting the first `[`..`]` span.
///
/// Returns `None` when NO JSON array is found (unparseable / garbage response),
/// which the caller must treat as a *failure* (fall back to hybrid order), vs
/// `Some(vec![])` for a genuine empty array `[]` (model judged nothing relevant,
/// which the caller honors). These two cases have opposite correct behavior.
pub fn extract_ranking(resp: &str, n: usize) -> Option<Vec<usize>> {
    let (s, e) = (resp.find('[')?, resp.rfind(']')?);
    if e < s {
        return None;
    }
    let nums: Vec<i64> = serde_json::from_str(&resp[s..=e]).ok()?;
    let mut seen = HashSet::new();
    Some(
        nums.into_iter()
            .filter_map(|x| {
                let idx = usize::try_from(x).ok()?;
                if idx >= 1 && idx <= n && seen.insert(idx) {
                    Some(idx - 1)
                } else {
                    None
                }
            })
            .collect(),
    )
}

/// Backwards-compatible wrapper: parse a ranking, treating "no array found" the
/// same as "empty array" (both yield an empty Vec). Used by the offline
/// benchmark where the failure/empty distinction is not needed. Production uses
/// [`extract_ranking`] to distinguish the two.
pub fn parse_rerank_response(resp: &str, n: usize) -> Vec<usize> {
    extract_ranking(resp, n).unwrap_or_default()
}

/// Rerank `candidates` with a listwise LLM call.
///
/// Returns ALL candidates reordered best-first (callers truncate to their own
/// top-k). Candidates the model ranks are placed first in model order; any
/// candidate the model omits is appended afterwards in the original hybrid order
/// (so omitted-but-retrieved memories are never lost, just deprioritized).
///
/// How aggressively the reranker filters candidates.
///
/// The listwise LLM both *ranks* candidates and *omits* the ones it judges
/// irrelevant. These modes decide what to do with the omitted ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RerankMode {
    /// Precision-focused (default): inject ONLY the memories the model judged
    /// relevant, in model order. If the model keeps 2 of 50, return 2; if it
    /// keeps none, return none. Maximizes precision (nothing irrelevant is
    /// surfaced) at the cost of some recall.
    #[default]
    Precision,
    /// Recall-focused: model-ranked memories first, then the omitted candidates
    /// appended in hybrid order. The caller can then take a fixed top-k. Trades
    /// precision for recall (surfaces more, including model-rejected ones).
    Recall,
}

/// Rerank `candidates` with a listwise LLM call (precision-focused default).
///
/// Equivalent to [`rerank_candidates_with_mode`] with [`RerankMode::Precision`]:
/// returns ONLY the memories the model judged relevant, in model order (no
/// irrelevant padding). The caller still applies its own upper-bound cap (e.g.
/// `MAX_MEMORIES_PER_TURN`), so the injected set is `min(relevant_count, cap)`,
/// and empty when the model judges nothing relevant.
pub async fn rerank_candidates(
    sidecar: &Sidecar,
    focused_query: &str,
    candidates: Vec<(MemoryEntry, f32)>,
) -> Vec<MemoryEntry> {
    rerank_candidates_with_mode(sidecar, focused_query, candidates, RerankMode::Precision).await
}

/// Rerank `candidates` with a listwise LLM call, choosing precision vs recall.
///
/// In [`RerankMode::Precision`] returns only the model-kept relevant memories in
/// model order. In [`RerankMode::Recall`] returns model-kept first then the
/// omitted candidates in hybrid order (so a fixed top-k still fills).
///
/// Failure handling (never regress below the hybrid baseline):
/// - LLM transport error -> hybrid order.
/// - response with no parseable JSON array (garbage) -> hybrid order.
/// - response with a genuine empty array `[]` (model judged nothing relevant)
///   -> Precision: empty; Recall: hybrid order.
pub async fn rerank_candidates_with_mode(
    sidecar: &Sidecar,
    focused_query: &str,
    candidates: Vec<(MemoryEntry, f32)>,
    mode: RerankMode,
) -> Vec<MemoryEntry> {
    if candidates.is_empty() {
        return Vec::new();
    }
    if candidates.len() == 1 {
        // A single candidate: trust hybrid (one LLM call to vet one item is not
        // worth it; the downstream surfacing already gates on hybrid relevance).
        return candidates.into_iter().map(|(e, _)| e).collect();
    }

    let pairs: Vec<(String, String)> = candidates
        .iter()
        .map(|(e, _)| (e.id.clone(), e.content.clone()))
        .collect();
    let prompt = build_rerank_prompt(focused_query, &pairs);
    let n = candidates.len();

    let order = match sidecar.complete(LLM_RERANK_SYSTEM, &prompt).await {
        // Case 1/3 failure: network error OR a response with no parseable array.
        // Fall back to hybrid order so a transient blip never drops all memory.
        Err(e) => {
            crate::logging::info(&format!(
                "Memory rerank failed ({e}); falling back to hybrid order"
            ));
            return candidates.into_iter().map(|(e, _)| e).collect();
        }
        Ok(resp) => match extract_ranking(&resp, n) {
            Some(order) => order,
            None => {
                // Case 3: model replied but with no usable JSON array (garbage).
                // Treat as failure, not as "nothing relevant".
                crate::logging::info(
                    "Memory rerank: unparseable response; falling back to hybrid order",
                );
                return candidates.into_iter().map(|(e, _)| e).collect();
            }
        },
    };

    if order.is_empty() {
        // Case 2: model returned a genuine empty array -> it judged NOTHING
        // relevant. Precision mode honors that (inject nothing); Recall mode
        // still surfaces the hybrid set.
        return match mode {
            RerankMode::Precision => Vec::new(),
            RerankMode::Recall => candidates.into_iter().map(|(e, _)| e).collect(),
        };
    }

    compose_reranked(candidates, &order, mode)
}

/// Pure composition step: given the candidates and the model's ranking
/// (0-based indices, best-first), produce the final entry list per `mode`.
/// Precision keeps only ranked entries; Recall appends omitted ones in hybrid
/// order. Factored out so it is unit-testable without a `Sidecar`.
fn compose_reranked(
    candidates: Vec<(MemoryEntry, f32)>,
    order: &[usize],
    mode: RerankMode,
) -> Vec<MemoryEntry> {
    let n = candidates.len();
    let ranked_set: HashSet<usize> = order.iter().copied().collect();
    let mut entries: Vec<Option<MemoryEntry>> =
        candidates.into_iter().map(|(e, _)| Some(e)).collect();

    let mut out: Vec<MemoryEntry> = Vec::with_capacity(n);
    // Model-ranked (relevant) candidates first, in model order.
    for &idx in order {
        if let Some(entry) = entries.get_mut(idx).and_then(Option::take) {
            out.push(entry);
        }
    }
    // Recall mode also appends the omitted candidates in original hybrid order.
    if mode == RerankMode::Recall {
        for (idx, slot) in entries.iter_mut().enumerate() {
            if !ranked_set.contains(&idx)
                && let Some(entry) = slot.take()
            {
                out.push(entry);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rerank_response_basic() {
        assert_eq!(parse_rerank_response("[3,1,2]", 3), vec![2, 0, 1]);
    }

    #[test]
    fn parse_rerank_response_dedups_and_bounds() {
        // 9 is out of range (n=3), duplicate 1 dropped, 0 invalid (1-based).
        assert_eq!(parse_rerank_response("[1, 9, 1, 2, 0]", 3), vec![0, 1]);
    }

    #[test]
    fn parse_rerank_response_tolerates_prose() {
        assert_eq!(
            parse_rerank_response("Here is the ranking: [2,1] (best first)", 2),
            vec![1, 0]
        );
    }

    #[test]
    fn parse_rerank_response_empty_on_garbage() {
        assert!(parse_rerank_response("no array here", 5).is_empty());
        assert!(parse_rerank_response("][", 5).is_empty());
    }

    #[test]
    fn extract_ranking_distinguishes_empty_array_from_no_array() {
        // Genuine empty array -> Some(empty): model judged nothing relevant.
        assert_eq!(extract_ranking("[]", 5), Some(vec![]));
        assert_eq!(extract_ranking("nothing relevant: []", 5), Some(vec![]));
        // No array at all -> None: unparseable, caller must treat as failure.
        assert_eq!(extract_ranking("I could not find anything", 5), None);
        assert_eq!(extract_ranking("][", 5), None);
        // Valid ranking -> Some(indices).
        assert_eq!(extract_ranking("[2,1]", 2), Some(vec![1, 0]));
    }

    fn mem(id: &str) -> MemoryEntry {
        let mut e = MemoryEntry::new(crate::memory_types::MemoryCategory::Fact, id);
        e.id = id.to_string();
        e
    }

    fn cands(ids: &[&str]) -> Vec<(MemoryEntry, f32)> {
        ids.iter().rev().enumerate().map(|(i, id)| (mem(id), i as f32)).collect::<Vec<_>>()
            .into_iter().rev().collect()
    }

    #[test]
    fn compose_precision_keeps_only_ranked() {
        // Pool a,b,c,d; model keeps only c then a (order [2,0]).
        let pool = cands(&["a", "b", "c", "d"]);
        let out = compose_reranked(pool, &[2, 0], RerankMode::Precision);
        let ids: Vec<&str> = out.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["c", "a"], "precision returns ONLY model-kept, in model order");
    }

    #[test]
    fn compose_recall_appends_omitted_in_hybrid_order() {
        let pool = cands(&["a", "b", "c", "d"]);
        let out = compose_reranked(pool, &[2, 0], RerankMode::Recall);
        let ids: Vec<&str> = out.iter().map(|e| e.id.as_str()).collect();
        // ranked (c,a) first, then omitted (b,d) in original order.
        assert_eq!(ids, vec!["c", "a", "b", "d"]);
    }

    #[test]
    fn build_prompt_numbers_candidates_one_based() {
        let cands = vec![
            ("a".to_string(), "first memory".to_string()),
            ("b".to_string(), "second memory".to_string()),
        ];
        let p = build_rerank_prompt("fix the scroll bug", &cands);
        assert!(p.contains("CURRENT REQUEST:\nfix the scroll bug"));
        assert!(p.contains("1. first memory"));
        assert!(p.contains("2. second memory"));
    }
}
