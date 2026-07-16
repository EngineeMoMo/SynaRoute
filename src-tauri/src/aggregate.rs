//! 大脑聚合引擎（FR-013 ~ FR-017）。
//!
//! 流程（arch-decisions §7-9）：
//! 1. 成员并行解答（并发上限 concurrency_limit，总超时 total_timeout_ms）
//! 2. 不可用成员跳过（FR-015）
//! 3. 聚合：
//!    - compressed(A)：调独立汇总模型压缩各答案（默认复用决策者），再交决策者
//!    - full(B)：全量答案直接交决策者
//! 4. 决策者产出最终答案
//! MVP 仅纯文本。

use crate::error::{AppError, AppResult};
use crate::model::{AggregateMode, BrainConfig, CategoryType};
use crate::store::Store;
use crate::upstream;
use futures_util::future::join_all;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

/// 单个成员的解答结果
struct MemberAnswer {
    label: String,
    answer: String,
}

/// 执行一次大脑聚合。prompt 为用户原始问题文本，返回最终答案。
pub async fn run(store: &Arc<Store>, category: CategoryType, prompt: &str) -> AppResult<String> {
    let brain = store.get_brain(category);
    if !brain.enabled {
        return Err(AppError::Invalid("大脑聚合未启用".into()));
    }
    let decider_ref = brain
        .decider_ref
        .clone()
        .ok_or_else(|| AppError::Invalid("未配置最终决策者".into()))?;

    // 1. 并行成员解答（受总超时约束）
    let answers = gather_members(store, &brain, prompt).await;

    if answers.is_empty() {
        // 所有成员不可用 → 回退：直接由决策者独立回答（arch-decisions §8 回退普通路由的简化）
        return call_ref(store, &decider_ref, prompt).await;
    }

    // 2. 聚合
    let decider_input = match brain.aggregate_mode {
        AggregateMode::Full => build_full_context(prompt, &answers),
        AggregateMode::Compressed => {
            // 汇总模型默认复用决策者
            let summarizer_ref = brain.summarizer_ref.clone().unwrap_or_else(|| decider_ref.clone());
            let summary = compress(store, &summarizer_ref, prompt, &answers).await?;
            build_compressed_context(prompt, &summary)
        }
    };

    // 3. 决策者产出最终答案
    call_ref(store, &decider_ref, &decider_input).await
}

/// 并行调用各成员，带并发上限与总超时。跳过不可用成员。
async fn gather_members(
    store: &Arc<Store>,
    brain: &BrainConfig,
    prompt: &str,
) -> Vec<MemberAnswer> {
    let total_timeout = Duration::from_millis(brain.total_timeout_ms);
    let sem = Arc::new(tokio::sync::Semaphore::new(brain.concurrency_limit.max(1) as usize));

    let tasks = brain.members.iter().map(|m| {
        let store = store.clone();
        let sem = sem.clone();
        let key_id = m.key_id.clone();
        let model = m.model_name.clone();
        let prompt = prompt.to_string();
        async move {
            let _permit = sem.acquire().await.ok()?;
            let key = store.get_key(&key_id)?;
            // 跳过熔断/不可用成员（FR-015）
            if !crate::health::is_candidate(&key.health) {
                return None;
            }
            let secret = store.secrets.read().get(&key_id).ok().flatten()?;
            let max_tokens = key.params.max_tokens.unwrap_or(4096);
            match upstream::text_completion(&key, &secret, &model, &prompt, max_tokens).await {
                Ok(ans) if !ans.trim().is_empty() => Some(MemberAnswer {
                    label: format!("{} / {}", key.name, model),
                    answer: ans,
                }),
                _ => None,
            }
        }
    });

    match timeout(total_timeout, join_all(tasks)).await {
        Ok(results) => results.into_iter().flatten().collect(),
        Err(_) => vec![], // 总超时：用已完成的（此简化实现下返回空，触发回退）
    }
}

/// 压缩汇总：调汇总模型把多份答案压成要点（方式A，减少 token）
async fn compress(
    store: &Arc<Store>,
    summarizer_ref: &str,
    question: &str,
    answers: &[MemberAnswer],
) -> AppResult<String> {
    let mut joined = String::new();
    for (i, a) in answers.iter().enumerate() {
        joined.push_str(&format!("\n【答案{} · {}】\n{}\n", i + 1, a.label, a.answer));
    }
    let sum_prompt = format!(
        "以下是多个模型对同一问题的回答。请提炼各答案的关键要点、共识与分歧，压缩成简洁的要点清单，供最终决策参考。\n\n问题：{question}\n{joined}"
    );
    call_ref(store, summarizer_ref, &sum_prompt).await
}

fn build_full_context(question: &str, answers: &[MemberAnswer]) -> String {
    let mut s = format!(
        "你是最终决策者。以下是多个模型对同一问题的完整回答，请综合它们，产出一个最准确、最完整的最终答案。\n\n问题：{question}\n"
    );
    for (i, a) in answers.iter().enumerate() {
        s.push_str(&format!("\n【成员{} · {}】\n{}\n", i + 1, a.label, a.answer));
    }
    s.push_str("\n请给出你综合后的最终答案：");
    s
}

fn build_compressed_context(question: &str, summary: &str) -> String {
    format!(
        "你是最终决策者。以下是多个模型回答的要点汇总，请据此产出最准确、最完整的最终答案。\n\n问题：{question}\n\n要点汇总：\n{summary}\n\n请给出你的最终答案："
    )
}

/// 按 "keyId::modelName" 引用调用一次文本补全
async fn call_ref(store: &Arc<Store>, reference: &str, prompt: &str) -> AppResult<String> {
    let (key_id, model) = reference
        .split_once("::")
        .ok_or_else(|| AppError::Invalid(format!("无效引用: {reference}")))?;
    let key = store
        .get_key(key_id)
        .ok_or_else(|| AppError::NotFound(key_id.into()))?;
    let secret = store
        .secrets
        .read()
        .get(key_id)?
        .ok_or_else(|| AppError::Invalid("决策者密钥缺失".into()))?;
    let max_tokens = key.params.max_tokens.unwrap_or(4096);
    upstream::text_completion(&key, &secret, model, prompt, max_tokens).await
}
