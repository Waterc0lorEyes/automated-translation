use anyhow::{Result, anyhow};
use futures::stream::{self, StreamExt};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::sync::OnceLock;
use std::time::Duration;

use crate::models::{ArticleSegment, SubtitleEntry};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// 默认官方端点；可用 `DEEPSEEK_API_URL` 覆盖（走代理/兼容网关/本地 mock 时用）
const DEFAULT_API_URL: &str = "https://api.deepseek.com/chat/completions";
const MODEL: &str = "deepseek-chat";
/// 单批最多送多少条。用 id 协议后输出很小，100 条不会超 max_tokens。
const BATCH_SIZE: usize = 100;
/// 拆句时单次请求的最大字符数（长文会分块送）
const SPLIT_CHUNK_CHARS: usize = 4000;
/// 校验时单次请求的最大句子数
const VERIFY_CHUNK_ITEMS: usize = 120;
/// 同时在飞的请求数上限（可用 TRANSLATE_CONCURRENCY 覆盖）
const DEFAULT_CONCURRENCY: usize = 3;
const MAX_CONCURRENCY: usize = 8;

fn concurrency() -> usize {
    parse_concurrency(std::env::var("TRANSLATE_CONCURRENCY").ok().as_deref())
}

fn parse_concurrency(raw: Option<&str>) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .map(|n| n.clamp(1, MAX_CONCURRENCY))
        .unwrap_or(DEFAULT_CONCURRENCY)
}

fn api_url() -> String {
    std::env::var("DEEPSEEK_API_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_API_URL.to_string())
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("构建 HTTP 客户端失败")
    })
}

/// 字幕条目和文章片段共有的"原文 + 译文"能力，
/// 让分批翻译/重试/回填的逻辑只写一份。
pub trait Segment: serde::Serialize {
    fn text(&self) -> &str;
    fn translation(&self) -> Option<&str>;
    fn set_translation(&mut self, value: String);
    fn note(&self) -> Option<&str>;
    fn set_note(&mut self, value: String);

    /// 落库时的 JSON 形态（字幕是裸数组，文章是带 kind 的对象）。
    fn serialize_all(items: &[Self]) -> String
    where
        Self: Sized,
    {
        serde_json::to_string(items).unwrap_or_default()
    }

    /// 从库里的 JSON 还原。
    fn parse_doc(raw: &str) -> Option<Vec<Self>>
    where
        Self: Sized;
}

impl Segment for SubtitleEntry {
    fn text(&self) -> &str {
        &self.text
    }
    fn translation(&self) -> Option<&str> {
        self.translation.as_deref()
    }
    fn set_translation(&mut self, value: String) {
        // 空字符串代表"清空译文"，回退为原文
        self.translation = if value.is_empty() { None } else { Some(value) };
    }

    fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
    fn set_note(&mut self, value: String) {
        self.note = if value.is_empty() { None } else { Some(value) };
    }
    fn parse_doc(raw: &str) -> Option<Vec<Self>> {
        serde_json::from_str(raw).ok()
    }
}

impl Segment for ArticleSegment {
    fn text(&self) -> &str {
        &self.text
    }
    fn translation(&self) -> Option<&str> {
        self.translation.as_deref()
    }
    fn set_translation(&mut self, value: String) {
        self.translation = if value.is_empty() { None } else { Some(value) };
    }
    fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
    fn set_note(&mut self, value: String) {
        self.note = if value.is_empty() { None } else { Some(value) };
    }
    fn parse_doc(raw: &str) -> Option<Vec<Self>> {
        let v: Value = serde_json::from_str(raw).ok()?;
        if v.get("kind")?.as_str()? != "article" {
            return None;
        }
        serde_json::from_value(v.get("segments")?.clone()).ok()
    }
    fn serialize_all(items: &[Self]) -> String {
        // 带 kind 的信封，和字幕的 JSON 数组区分开
        serde_json::to_string(&json!({ "kind": "article", "segments": items }))
            .unwrap_or_default()
    }
}

/// 解析库里存的 translated_text。只有两种合法形态：
/// 字幕数组、带 `kind: "article"` 的文章片段对象。
pub enum ParsedContent {
    Subtitles(Vec<SubtitleEntry>),
    Article(Vec<ArticleSegment>),
}

pub fn parse_content(raw: &str) -> Option<ParsedContent> {
    let t = raw.trim_start();

    if t.starts_with('{') {
        let v: Value = serde_json::from_str(t).ok()?;
        if v.get("kind")?.as_str()? != "article" {
            return None;
        }
        let list = serde_json::from_value::<Vec<ArticleSegment>>(v.get("segments")?.clone()).ok()?;
        return Some(ParsedContent::Article(list));
    }

    if t.starts_with('[') {
        let list = serde_json::from_str::<Vec<SubtitleEntry>>(t).ok()?;
        return Some(ParsedContent::Subtitles(list));
    }

    None
}

// ============================================================
// 文章：AI 拆句 → AI 校验
// ============================================================

/// 让模型把文章拆成句子。长文按字符数分块，逐块拆分后拼接。
pub async fn split_article(api_key: &str, text: &str, source_lang: &str) -> Result<Vec<String>> {
    let chunks = chunk_text(text, SPLIT_CHUNK_CHARS);
    tracing::info!("split_article: {} 字符, {} 块", text.chars().count(), chunks.len());

    let prompt = split_prompt(source_lang);
    let limit = concurrency();

    // buffered 保序：并发请求但按原块顺序拼接，句子顺序不能乱
    let mut stream = stream::iter(chunks)
        .map(|chunk: String| {
            let user_content = json!({ "text": chunk }).to_string();
            let body = api_body(&prompt, &user_content, 8192, true);
            async move { call_deepseek_json(api_key, &body).await }
        })
        .buffered(limit);

    let mut out = Vec::new();
    while let Some(result) = stream.next().await {
        let result = result?;
        let content = extract_content(&result)?;
        let sentences = extract_strings(&content);
        if sentences.is_empty() {
            return Err(anyhow!("拆句返回为空"));
        }
        out.extend(sentences);
    }

    // 去掉完全重复的相邻句子（模型偶尔会把段落首句重复一次）
    out.dedup();
    Ok(out)
}

/// 让模型复核拆句结果：合并碎句、切开过长句、删除空/重复、补齐遗漏。
/// 校验失败时退回初版，保证主流程不中断。
pub async fn verify_sentences(api_key: &str, sentences: &[String], source_lang: &str) -> Vec<String> {
    let mut out = Vec::new();

    for chunk in sentences.chunks(VERIFY_CHUNK_ITEMS) {
        let user_content = json!({ "sentences": chunk }).to_string();
        let body = api_body(&verify_prompt(source_lang), &user_content, 8192, true);

        match call_deepseek_json(api_key, &body).await {
            Ok(result) => match extract_content(&result) {
                Ok(content) => {
                    let revised = extract_strings(&content);
                    if revised.is_empty() {
                        tracing::warn!("校验返回为空，沿用初版拆句");
                        out.extend(chunk.iter().cloned());
                    } else {
                        tracing::info!("校验: {} -> {} 条", chunk.len(), revised.len());
                        out.extend(revised);
                    }
                }
                Err(e) => {
                    tracing::warn!("校验内容异常，沿用初版: {}", e);
                    out.extend(chunk.iter().cloned());
                }
            },
            Err(e) => {
                tracing::warn!("校验调用失败，沿用初版: {}", e);
                out.extend(chunk.iter().cloned());
            }
        }
    }

    // 校验后仍为空（整篇只有空白）时退回初版
    if out.is_empty() {
        out = sentences.to_vec();
    }
    out
}

// ============================================================
// 翻译核心：字幕和文章共用
// ============================================================

/// 分批翻译直到没有空条目。每批把 id 送出去、按 id 回填。
async fn translate_segments<T: Segment>(
    api_key: &str, entries: &mut [T],
    source_lang: &str, target_lang: &str, custom_prompt: &str,
    pool: &SqlitePool, translation_id: i64,
) -> Result<()> {
    if entries.is_empty() { return Ok(()); }
    let total = entries.len();
    let texts: Vec<String> = entries.iter().map(|e| norm(e.text())).collect();

    for batch_size in batch_sizes(total) {
        let remaining: Vec<usize> = entries.iter().enumerate()
            .filter(|(_, e)| e.translation().is_none_or(|t| t.is_empty()))
            .map(|(i, _)| i).collect();
        if remaining.is_empty() { break; }
        let remaining_count = remaining.len();

        tracing::info!("translate: {}/{} remaining, batch_size={}", remaining_count, total, batch_size);

        let prompt = translate_prompt(source_lang, target_lang, custom_prompt);
        let limit = concurrency();

        // 请求并发发出，但结果回到主任务里串行写入 entries / 落库，
        // 避免多个任务各持一份快照互相覆盖。
        let chunks: Vec<Vec<usize>> = remaining.chunks(batch_size).map(|c| c.to_vec()).collect();
        let mut stream = stream::iter(chunks)
            .map(|chunk: Vec<usize>| {
                let user_content = json!({
                    "items": chunk.iter().map(|&i| json!({"id": i, "text": texts[i]})).collect::<Vec<_>>()
                }).to_string();
                let body = api_body(&prompt, &user_content, 8192, true);
                async move { (chunk, call_deepseek_json(api_key, &body).await) }
            })
            .buffer_unordered(limit);

        let mut round_matched = 0;
        while let Some((chunk, result)) = stream.next().await {
            match result {
                Ok(result) => {
                    let content = match extract_content(&result) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!("translate content error: {}", e);
                            continue;
                        }
                    };
                    let items = extract_items(&content);
                    let (filled, ignored) = apply_translations(entries, &chunk, &items);
                    if ignored > 0 {
                        tracing::warn!("translate batch: 忽略 {} 条无效/越界结果", ignored);
                    }
                    tracing::info!("translate batch: {}/{} filled", filled.len(), chunk.len());
                    round_matched += filled.len();
                    merge_filled::<T>(pool, translation_id, &filled).await;
                }
                Err(e) => tracing::error!("translate API error: {}", e),
            }
        }

        let still_empty = entries.iter()
            .filter(|e| e.translation().is_none_or(|t| t.is_empty()))
            .count();
        tracing::info!("translate round done: +{} filled, {} still empty", round_matched, still_empty);
        if still_empty == 0 { break; }
    }
    Ok(())
}

/// 补译：只处理还空着的条目，按批发送。
async fn fill_segments<T: Segment>(
    api_key: &str, entries: &mut [T],
    source_lang: &str, target_lang: &str, custom_prompt: &str,
    pool: &SqlitePool, translation_id: i64,
) -> Result<()> {
    let texts: Vec<String> = entries.iter().map(|e| norm(e.text())).collect();

    let unmatched: Vec<usize> = entries.iter().enumerate()
        .filter(|(_, e)| e.translation().is_none_or(|t| t.is_empty()))
        .map(|(i, _)| i).collect();

    if unmatched.is_empty() { return Ok(()); }

    tracing::info!("continue_translate: {} 条待补", unmatched.len());

    // 先把已有译文取出来，避免并发闭包里借用 entries（后面要可变写）
    let existing: Vec<String> = entries.iter()
        .map(|e| e.translation().unwrap_or("").to_string())
        .collect();
    let prompt = continue_prompt(source_lang, target_lang, custom_prompt);
    let limit = concurrency();

    let chunks: Vec<Vec<usize>> = unmatched.chunks(BATCH_SIZE).map(|c| c.to_vec()).collect();
    let mut stream = stream::iter(chunks)
        .map(|chunk: Vec<usize>| {
            let user_content = json!({
                "items": chunk.iter().map(|&i| json!({
                    "id": i,
                    "text": texts[i],
                    "translation": existing[i],
                })).collect::<Vec<_>>()
            }).to_string();
            let body = api_body(&prompt, &user_content, 8192, true);
            async move { (chunk, call_deepseek_json(api_key, &body).await) }
        })
        .buffer_unordered(limit);

    while let Some((chunk, result)) = stream.next().await {
        match result {
            Ok(result) => {
                let content = match extract_content(&result) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("补译 content error: {}", e);
                        continue;
                    }
                };
                let items = extract_items(&content);
                let (filled, ignored) = apply_translations(entries, &chunk, &items);
                tracing::info!("补译 batch: {}/{} filled, 忽略 {}", filled.len(), chunk.len(), ignored);
                merge_filled::<T>(pool, translation_id, &filled).await;
            }
            Err(e) => tracing::error!("补译 API error: {}", e),
        }

    }

    Ok(())
}

/// 供 `tokio::spawn` 用的所有权版本：内部仍有借用，但整体不跨任务借数据，
/// 避免 spawn 时对 `&mut` 参数做高阶生命周期推断。
pub async fn translate_subtitles_owned(
    api_key: String, mut entries: Vec<SubtitleEntry>, source_lang: String, target_lang: String,
    custom_prompt: String, pool: SqlitePool, translation_id: i64,
) -> (Vec<SubtitleEntry>, Result<()>) {
    let result = translate_segments(&api_key, &mut entries, &source_lang, &target_lang, &custom_prompt, &pool, translation_id).await;
    (entries, result)
}

pub async fn translate_article_owned(
    api_key: String, mut segments: Vec<ArticleSegment>, source_lang: String, target_lang: String,
    custom_prompt: String, pool: SqlitePool, translation_id: i64,
) -> (Vec<ArticleSegment>, Result<()>) {
    let result = translate_segments(&api_key, &mut segments, &source_lang, &target_lang, &custom_prompt, &pool, translation_id).await;
    (segments, result)
}

pub async fn continue_translate(
    api_key: &str, entries: &mut [SubtitleEntry],
    source_lang: &str, target_lang: &str, custom_prompt: &str,
    pool: &SqlitePool, translation_id: i64,
) -> Result<()> {
    fill_segments(api_key, entries, source_lang, target_lang, custom_prompt, pool, translation_id).await
}

pub async fn continue_translate_article(
    api_key: &str, segments: &mut [ArticleSegment],
    source_lang: &str, target_lang: &str, custom_prompt: &str,
    pool: &SqlitePool, translation_id: i64,
) -> Result<()> {
    fill_segments(api_key, segments, source_lang, target_lang, custom_prompt, pool, translation_id).await
}

/// 供 `tokio::spawn` 用的所有权版本。
pub async fn generate_annotations_owned<T: Segment>(
    api_key: String, mut entries: Vec<T>, annotation_prompt: String,
    pool: SqlitePool, translation_id: i64,
) -> (Vec<T>, Result<()>) {
    let result = generate_annotations(&api_key, &mut entries, &annotation_prompt, &pool, translation_id).await;
    (entries, result)
}

pub async fn generate_annotations<T: Segment>(
    api_key: &str, entries: &mut [T], annotation_prompt: &str,
    pool: &SqlitePool, translation_id: i64,
) -> Result<()> {
    if entries.is_empty() { return Ok(()); }
    let total = entries.len();
    let texts: Vec<String> = entries.iter().map(|e| norm(e.text())).collect();

    tracing::info!("generate_annotations: {} entries, prompt='{}'", total, annotation_prompt);

    let ranges: Vec<(usize, usize)> = (0..total)
        .step_by(BATCH_SIZE)
        .map(|start| (start, (start + BATCH_SIZE).min(total)))
        .collect();

    let prompt = annotate_prompt(annotation_prompt);
    let limit = concurrency();

    let mut stream = stream::iter(ranges)
        .map(|(start, end): (usize, usize)| {
            let user_content = json!({
                "items": (start..end).map(|i| json!({"id": i, "text": texts[i]})).collect::<Vec<_>>()
            }).to_string();
            let body = api_body(&prompt, &user_content, 8192, true);
            async move { ((start, end), call_deepseek_json(api_key, &body).await) }
        })
        .buffer_unordered(limit);

    while let Some(((start, end), result)) = stream.next().await {
        match result {
            Ok(result) => {
                let content = match extract_content(&result) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("annot content error: {}", e);
                        continue;
                    }
                };
                let items = extract_items(&content);
                let range: Vec<usize> = (start..end).collect();
                let (notes, ignored) = apply_notes(entries, &range, &items);
                tracing::info!("annot batch {}-{}: {} notes, 忽略 {}", start, end, notes.len(), ignored);
                merge_notes::<T>(pool, translation_id, &notes).await;
            }
            Err(e) => tracing::error!("annot API error: {}", e),
        }

    }
    Ok(())
}

/// 全局写锁：所有落库都是"读最新 → 改 → 写回"，必须串行，否则并发任务会互相覆盖。
fn save_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// 读出库里最新内容，交给 `f` 修改后写回。
async fn with_doc<T: Segment>(pool: &SqlitePool, id: i64, f: impl FnOnce(&mut Vec<T>)) {
    let _guard = save_lock().lock().await;

    let raw: Option<String> = sqlx::query_scalar("SELECT translated_text FROM translations WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    let Some(raw) = raw else { return };
    let Some(mut items) = T::parse_doc(&raw) else { return };

    f(&mut items);

    let payload = T::serialize_all(&items);
    let _ = sqlx::query("UPDATE translations SET translated_text = ? WHERE id = ?")
        .bind(&payload).bind(id).execute(pool).await;
}

/// 只补空位，不覆盖已有译文（翻译任务用）。
async fn merge_filled<T: Segment>(pool: &SqlitePool, id: i64, filled: &[(usize, String)]) {
    if filled.is_empty() { return; }
    with_doc::<T>(pool, id, |items| {
        for (i, text) in filled {
            if let Some(item) = items.get_mut(*i) {
                if item.translation().is_none_or(|t| t.is_empty()) {
                    item.set_translation(text.clone());
                }
            }
        }
    })
    .await;
}

/// 覆盖/清空指定条目（用户编辑用）。
pub async fn merge_edits<T: Segment>(pool: &SqlitePool, id: i64, edits: &[(usize, Option<String>)]) {
    if edits.is_empty() { return; }
    with_doc::<T>(pool, id, |items| {
        for (i, value) in edits {
            if let Some(item) = items.get_mut(*i) {
                item.set_translation(value.clone().unwrap_or_default());
            }
        }
    })
    .await;
}

/// 追加注释（已存在的不重复加）。
async fn merge_notes<T: Segment>(pool: &SqlitePool, id: i64, notes: &[(usize, String)]) {
    if notes.is_empty() { return; }
    with_doc::<T>(pool, id, |items| {
        for (i, note) in notes {
            if let Some(item) = items.get_mut(*i) {
                let existing = item.note().unwrap_or("");
                if existing.is_empty() {
                    item.set_note(note.clone());
                } else if !existing.contains(note.as_str()) {
                    item.set_note(format!("{}；{}", existing, note));
                }
            }
        }
    })
    .await;
}

/// 整体替换文档内容（拆句完成后写入 segment 列表）。
pub async fn replace_doc<T: Segment>(pool: &SqlitePool, id: i64, items: &[T]) {
    let _guard = save_lock().lock().await;
    let payload = T::serialize_all(items);
    let _ = sqlx::query("UPDATE translations SET translated_text = ? WHERE id = ?")
        .bind(&payload).bind(id).execute(pool).await;
}

/// 抢占任务：只有不在运行中的记录才能被认领，避免同一记录上并发跑多个
/// 消耗 API 的任务。单条 UPDATE 保证原子性。
pub async fn try_claim(pool: &SqlitePool, id: i64) -> bool {
    sqlx::query(
        "UPDATE translations SET status = 'running' \
         WHERE id = ? AND status NOT IN ('running', 'splitting')",
    )
    .bind(id)
    .execute(pool)
    .await
    .map(|r| r.rows_affected() == 1)
    .unwrap_or(false)
}

/// 更新任务状态（queued/splitting/running/done/failed）。
pub async fn set_status(pool: &SqlitePool, id: i64, status: &str) {
    let _ = sqlx::query("UPDATE translations SET status = ? WHERE id = ?")
        .bind(status).bind(id).execute(pool).await;
}

fn api_body(system_prompt: &str, user_content: &str, max_tokens: u32, json_mode: bool) -> Value {
    let mut body = json!({
        "model": MODEL,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_content }
        ],
        "temperature": 0.3,
        "max_tokens": max_tokens
    });
    if json_mode {
        // JSON Output：约束模型只吐合法 JSON 对象
        body["response_format"] = json!({ "type": "json_object" });
    }
    body
}

async fn call_deepseek(api_key: &str, body: &Value) -> Result<Value> {
    let start = std::time::Instant::now();
    let model = body["model"].as_str().unwrap_or("?");
    let tokens = body["max_tokens"].as_u64().unwrap_or(0);

    let response = http_client()
        .post(api_url())
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(body)
        .send()
        .await?;

    let elapsed = start.elapsed();
    let status = response.status();
    let raw = response.text().await?;

    if !status.is_success() {
        tracing::error!(
            "DeepSeek {} FAIL after {:.1}s: {} — body:{}",
            status, elapsed.as_secs_f32(), model, snippet(&raw, 200)
        );
        return Err(anyhow!("DeepSeek {} : {}", status, snippet(&raw, 300)));
    }

    let result: Value = serde_json::from_str(&raw).map_err(|e| {
        tracing::error!("JSON parse error: {} body:{}", e, snippet(&raw, 200));
        anyhow!("JSON parse: {} body: {}", e, snippet(&raw, 200))
    })?;

    let usage = &result["usage"];
    tracing::info!(
        "DeepSeek {} OK {:.1}s max_tok={} prompt={} compl={} total={}",
        model, elapsed.as_secs_f32(), tokens,
        usage["prompt_tokens"].as_u64().unwrap_or(0),
        usage["completion_tokens"].as_u64().unwrap_or(0),
        usage["total_tokens"].as_u64().unwrap_or(0),
    );

    Ok(result)
}

/// 带 JSON Output 的调用；若服务端不认 `response_format`，自动去掉后重试一次。
async fn call_deepseek_json(api_key: &str, body: &Value) -> Result<Value> {
    match call_deepseek(api_key, body).await {
        Ok(v) => Ok(v),
        Err(e) => {
            let msg = e.to_string();
            let unsupported = msg.contains("response_format")
                || (msg.contains("400") && (msg.contains("json_object") || msg.contains("json mode")));
            if !unsupported {
                return Err(e);
            }
            tracing::warn!(
                "服务端不支持 response_format，回退为普通 JSON 输出重试：{}",
                snippet(&msg, 120)
            );
            let mut fallback = body.clone();
            if let Some(obj) = fallback.as_object_mut() {
                obj.remove("response_format");
            }
            call_deepseek(api_key, &fallback).await
        }
    }
}

fn extract_content(response: &Value) -> Result<String> {
    response["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("模型返回了空 content"))
}

fn translate_prompt(source_lang: &str, target_lang: &str, custom_prompt: &str) -> String {
    if !custom_prompt.trim().is_empty() {
        // 自定义提示词也要带上 id 协议，否则结果无法回填
        return format!(
            "{}\n\nTranslate each item's \"text\" from {} to {}. \
             The input is json: {{\"items\":[{{\"id\":1,\"text\":\"...\"}}]}}.\n\
             Return json only, in exactly this shape: \
             {{\"pairs\":[{{\"id\":1,\"translation\":\"...\"}}]}}\n\
             Copy every \"id\" back unchanged, never invent ids, and only include items you translated.",
            custom_prompt, lang_name(source_lang), lang_name(target_lang)
        );
    }
    format!(
        "You are a professional translator. Translate each item's \"text\" from {} to {}.\n\
         The input is json: {{\"items\":[{{\"id\":1,\"text\":\"...\"}}]}}.\n\
         Return json only, in exactly this shape:\n\
         {{\"pairs\":[{{\"id\":1,\"translation\":\"...\"}}]}}\n\
         Rules:\n\
         - copy every \"id\" back unchanged; never invent or omit the id field\n\
         - only include items you actually translated\n\
         - no commentary, no markdown fences",
        lang_name(source_lang), lang_name(target_lang)
    )
}

fn continue_prompt(source_lang: &str, target_lang: &str, custom_prompt: &str) -> String {
    if !custom_prompt.trim().is_empty() {
        return format!(
            "{}\n\nFill in the empty \"translation\" fields, translating from {} to {}.\n\
             The input is json: {{\"items\":[{{\"id\":1,\"text\":\"...\",\"translation\":\"\"}}]}}.\n\
             Return json only: {{\"pairs\":[{{\"id\":1,\"translation\":\"...\"}}]}}\n\
             Copy every \"id\" back unchanged and only return ids you translated.",
            custom_prompt, lang_name(source_lang), lang_name(target_lang)
        );
    }
    format!(
        "Fill in the missing translations from {} to {}.\n\
         The input is json: {{\"items\":[{{\"id\":1,\"text\":\"...\",\"translation\":\"\"}}]}}.\n\
         Some \"translation\" values are empty strings — translate only those, \
         and follow the style of the ones already filled.\n\
         Return json only, in exactly this shape:\n\
         {{\"pairs\":[{{\"id\":1,\"translation\":\"...\"}}]}}\n\
         Copy every \"id\" back unchanged; no commentary, no markdown fences.",
        lang_name(source_lang), lang_name(target_lang)
    )
}

fn annotate_prompt(annotation_prompt: &str) -> String {
    format!(
        "You annotate text for language learners.\n\
         For items related to: {}, add a brief note explaining the reference, slang or culture.\n\
         The input is json: {{\"items\":[{{\"id\":1,\"text\":\"...\"}}]}}.\n\
         Return json only, in exactly this shape:\n\
         {{\"notes\":[{{\"id\":1,\"note\":\"...\"}}]}}\n\
         Only include items that actually need a note; copy each \"id\" back unchanged.",
        annotation_prompt
    )
}

fn split_prompt(source_lang: &str) -> String {
    format!(
        "Split the following {} article into individual sentences for translation.\n\
         The input is json: {{\"text\":\"...\"}}.\n\
         Return json only, in exactly this shape:\n\
         {{\"sentences\":[\"...\",\"...\"]}}\n\
         Rules:\n\
         - keep the original wording and language exactly; do not translate, summarize or rewrite\n\
         - one complete sentence per element, punctuation stays inside the element\n\
         - if a sentence is very long, you may split it at a natural clause boundary\n\
         - never merge two separate sentences into one element, never drop content\n\
         - keep the original order",
        lang_name(source_lang)
    )
}

fn verify_prompt(source_lang: &str) -> String {
    format!(
        "You are reviewing a sentence split of a {} article before translation.\n\
         The input is json: {{\"sentences\":[\"...\",\"...\"]}}.\n\
         Fix the split and return json only, in exactly this shape:\n\
         {{\"sentences\":[\"...\",\"...\"]}}\n\
         Rules:\n\
         - merge fragments that are not complete sentences\n\
         - split elements that actually contain more than one sentence\n\
         - delete empty or duplicated elements\n\
         - add back sentences that are obviously missing\n\
         - keep the original wording; do not translate or rewrite\n\
         - keep the original order",
        lang_name(source_lang)
    )
}

#[derive(Debug, serde::Deserialize)]
struct RawItem {
    /// 可能是数字 1，也可能是字符串 "1"
    id: Value,
    #[serde(default)]
    translation: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

impl RawItem {
    fn id(&self) -> Option<usize> {
        if let Some(n) = self.id.as_u64() {
            return usize::try_from(n).ok();
        }
        self.id.as_str()?.trim().parse().ok()
    }

    fn clean(&self, field: Option<&String>) -> Option<String> {
        field
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }
}

/// 返回本批新增的 (下标, 译文)，供增量落库。
fn apply_translations<T: Segment>(entries: &mut [T], chunk: &[usize], items: &[RawItem]) -> (Vec<(usize, String)>, usize) {
    let mut filled = Vec::new();
    let mut ignored = 0;
    for item in items {
        let (Some(id), Some(text)) = (item.id(), item.clean(item.translation.as_ref())) else {
            ignored += 1;
            continue;
        };
        // 只接受本批次发出的 id，防止模型编造越界下标
        if chunk.binary_search(&id).is_err() {
            ignored += 1;
            continue;
        }
        if entries[id].translation().is_none_or(|t| t.is_empty()) {
            entries[id].set_translation(text.clone());
            filled.push((id, text));
        } else {
            ignored += 1; // 已有译文不覆盖
        }
    }
    (filled, ignored)
}

/// 返回本批新增的 (下标, 注释)，供增量落库。
fn apply_notes<T: Segment>(entries: &mut [T], range: &[usize], items: &[RawItem]) -> (Vec<(usize, String)>, usize) {
    let mut notes = Vec::new();
    let mut ignored = 0;
    for item in items {
        let (Some(id), Some(note)) = (item.id(), item.clean(item.note.as_ref())) else {
            ignored += 1;
            continue;
        };
        if range.binary_search(&id).is_err() {
            ignored += 1;
            continue;
        }
        let existing = entries[id].note().unwrap_or("");
        if existing.is_empty() {
            entries[id].set_note(note.clone());
        } else if !existing.contains(&note) {
            entries[id].set_note(format!("{}；{}", existing, note));
        }
        notes.push((id, note));
    }
    (notes, ignored)
}

/// 从模型输出里取出条目：优先整体解析（对象信封或裸数组），
/// 失败时逐个扫描顶层对象——这样 JSON 被截断也能捞回前面完整的部分。
fn extract_items(content: &str) -> Vec<RawItem> {
    let body = strip_fences(content);

    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if let Some(arr) = find_array(&value) {
            return arr.iter()
                .filter_map(|v| serde_json::from_value::<RawItem>(v.clone()).ok())
                .collect();
        }
    }

    let scan_target = match body.find('[') {
        Some(start) => {
            let tail = &body[start + 1..];
            match tail.rfind(']') {
                Some(end) => &tail[..end],
                None => tail,
            }
        }
        None => body,
    };

    scan_objects(scan_target)
}

/// 取字符串数组（拆句/校验的响应）。
fn extract_strings(content: &str) -> Vec<String> {
    let body = strip_fences(content);

    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if let Some(arr) = find_array(&value) {
            return arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
        if let Some(s) = value.as_str() {
            let t = s.trim();
            if !t.is_empty() {
                return vec![t.to_string()];
            }
        }
    }

    // 截断兜底：只扫数组里的字符串字面量
    match body.find('[') {
        Some(i) => scan_strings(&body[i + 1..]),
        None => Vec::new(),
    }
}

/// 找出所有配对完整的 `{...}` 片段并逐个解析（字符串内的括号不参与计数）。
fn scan_objects(s: &str) -> Vec<RawItem> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut in_str = false;
    let mut escaped = false;

    for (i, b) in s.bytes().enumerate() {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let parsed = serde_json::from_str::<Value>(&s[start..=i])
                        .ok()
                        .and_then(|v| serde_json::from_value::<RawItem>(v).ok());
                    if let Some(item) = parsed {
                        out.push(item);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// 找出所有配对完整的 `"..."` 字面量（用于被截断的字符串数组）。
fn scan_strings(s: &str) -> Vec<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        let mut escaped = false;
        while i < bytes.len() {
            let b = bytes[i];
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                break;
            }
            i += 1;
        }
        if i < bytes.len() {
            if let Ok(Value::String(v)) = serde_json::from_str::<Value>(&s[start..=i]) {
                let t = v.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
            }
            i += 1;
        }
    }
    out
}

fn find_array(v: &Value) -> Option<&Vec<Value>> {
    if let Some(arr) = v.as_array() {
        return Some(arr);
    }
    let obj = v.as_object()?;
    for key in [
        "pairs", "items", "results", "notes", "data", "translations", "annotations",
        "sentences", "result", "segments",
    ] {
        if let Some(arr) = obj.get(key).and_then(|x| x.as_array()) {
            return Some(arr);
        }
    }
    // 兜底：任意数组字段
    obj.values().find_map(|x| x.as_array())
}

fn strip_fences(s: &str) -> &str {
    let t = s.trim();
    let Some(start) = t.find("```") else {
        return t;
    };
    let after = &t[start + 3..];
    let after = after.strip_prefix("json").unwrap_or(after);
    let after = after.strip_prefix('\n').unwrap_or(after);
    match after.rfind("```") {
        Some(end) => after[..end].trim(),
        None => after.trim(),
    }
}

/// 按字符数切成若干块，尽量在段落边界断开。
fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;

    for para in text.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        let para_len = para.chars().count();

        if para_len > max_chars {
            if !cur.is_empty() {
                chunks.push(std::mem::take(&mut cur));
                cur_len = 0;
            }
            let mut buf = String::new();
            let mut buf_len = 0usize;
            for ch in para.chars() {
                buf.push(ch);
                buf_len += 1;
                if buf_len >= max_chars {
                    chunks.push(std::mem::take(&mut buf));
                    buf_len = 0;
                }
            }
            if !buf.is_empty() {
                chunks.push(buf);
            }
            continue;
        }

        if cur_len > 0 && cur_len + para_len + 2 > max_chars {
            chunks.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        if cur_len > 0 {
            cur.push_str("\n\n");
            cur_len += 2;
        }
        cur.push_str(para);
        cur_len += para_len;
    }

    if !cur.is_empty() {
        chunks.push(cur);
    }
    if chunks.is_empty() {
        chunks.push(text.to_string());
    }
    chunks
}

fn batch_sizes(total: usize) -> Vec<usize> {
    let mut sizes = vec![total.min(BATCH_SIZE)];
    let mut s = sizes[0];
    while s > 10 {
        s /= 2;
        sizes.push(s);
    }
    sizes.dedup();
    sizes
}

// ============================================================
// 导出
// ============================================================

pub fn format_subtitles(entries: &[SubtitleEntry], format: &str) -> String {
    match format.to_lowercase().as_str() {
        "vtt" => format_vtt(entries),
        "srt" => format_srt(entries),
        "txt" => format_plain(entries),
        _ => format_vtt(entries),
    }
}

/// 文章的逐句译文，一句一行。
pub fn format_segments(segments: &[ArticleSegment]) -> String {
    let mut out = String::new();
    for s in segments {
        out.push_str(s.translation.as_deref().unwrap_or(&s.text));
        out.push('\n');
    }
    out
}

/// 文章的逐句原文，一句一行。
pub fn format_segment_sources(segments: &[ArticleSegment]) -> String {
    let mut out = String::new();
    for s in segments {
        out.push_str(&s.text);
        out.push('\n');
    }
    out
}

/// 原文 / 译文 / 注释 对照表（CSV）。
/// 带 UTF-8 BOM、CRLF 换行和标准转义，Excel 直接双击打开不会乱码。
pub fn format_segment_table(segments: &[ArticleSegment], headers: [&str; 4]) -> String {
    let mut out = String::from("\u{feff}");
    out.push_str(&headers.iter().map(|h| csv_field(h)).collect::<Vec<_>>().join(","));
    out.push_str("\r\n");

    for s in segments {
        let row = [
            s.index.to_string(),
            s.text.clone(),
            s.translation.clone().unwrap_or_default(),
            s.note.clone().unwrap_or_default(),
        ];
        out.push_str(&row.iter().map(|c| csv_field(c)).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out
}

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// 纯文本导出：一条字幕一行译文
fn format_plain(entries: &[SubtitleEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        let text = e.translation.as_deref().unwrap_or(&e.text);
        out.push_str(text);
        out.push('\n');
    }
    out
}

fn format_vtt(entries: &[SubtitleEntry]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for e in entries {
        let text = e.translation.as_deref().unwrap_or(&e.text);
        // 保留原始定位信息（align/line 等），否则导出后字幕位置会变
        let settings = match e.settings.as_deref() {
            Some(s) if !s.is_empty() => format!(" {}", s),
            _ => String::new(),
        };
        out.push_str(&format!("{} --> {}{}\n{}\n\n", e.start, e.end, settings, text));
    }
    out
}

fn format_srt(entries: &[SubtitleEntry]) -> String {
    let mut out = String::new();
    for (i, e) in entries.iter().enumerate() {
        let start = e.start.replace('.', ",");
        let end = e.end.replace('.', ",");
        let text = e.translation.as_deref().unwrap_or(&e.text);
        out.push_str(&format!("{}\n{} --> {}\n{}\n\n", i + 1, start, end, text));
    }
    out
}

fn lang_name(code: &str) -> &str {
    match code {
        "en" => "English", "zh" => "Chinese", "ja" => "Japanese",
        "ko" => "Korean", "fr" => "French", "de" => "German", "es" => "Spanish",
        _ => code,
    }
}

/// 把空白（含换行）压成单个空格，用于跨行文本。
fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 按字节上限截断日志片段，且保证落在字符边界上。
/// 直接写 `&s[..s.len().min(n)]` 在中文/emoji 上会 panic。
fn snippet(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str, settings: Option<&str>) -> SubtitleEntry {
        SubtitleEntry {
            index: 1,
            start: "00:00:01.000".into(),
            end: "00:00:02.000".into(),
            text: text.into(),
            translation: None,
            note: None,
            settings: settings.map(|s| s.to_string()),
        }
    }

    fn entries(n: usize) -> Vec<SubtitleEntry> {
        (0..n).map(|i| entry(&format!("line {i}"), None)).collect()
    }

    fn segments(n: usize) -> Vec<ArticleSegment> {
        (0..n)
            .map(|i| ArticleSegment { index: i + 1, text: format!("sentence {i}"), translation: None, note: None })
            .collect()
    }

    #[test]
    fn json_mode_sets_response_format() {
        let body = api_body("p", "u", 100, true);
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["model"], MODEL);
    }

    #[test]
    fn plain_mode_has_no_response_format() {
        let body = api_body("p", "u", 100, false);
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn prompts_mention_json_and_ids() {
        for p in [
            translate_prompt("en", "zh", ""),
            translate_prompt("en", "zh", "be formal"),
            continue_prompt("en", "zh", ""),
            annotate_prompt("slang"),
        ] {
            assert!(p.contains("json"), "提示词必须含 json 字样: {p}");
            assert!(p.contains("id"), "提示词必须说明 id 协议: {p}");
        }
    }

    #[test]
    fn split_and_verify_prompts_ask_for_sentences() {
        for p in [split_prompt("en"), verify_prompt("en")] {
            assert!(p.contains("json"));
            assert!(p.contains("sentences"));
        }
    }

    #[test]
    fn parses_object_envelope() {
        let content = r#"{"pairs":[{"id":0,"translation":"甲"},{"id":2,"translation":"乙"}]}"#;
        let items = extract_items(content);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id(), Some(0));
        assert_eq!(items[1].translation.as_deref(), Some("乙"));
    }

    #[test]
    fn parses_bare_array() {
        let items = extract_items(r#"[{"id":1,"translation":"甲"}]"#);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn parses_fenced_output() {
        let content = "```json\n{\"pairs\":[{\"id\":5,\"translation\":\"甲\"}]}\n```";
        assert_eq!(extract_items(content).len(), 1);
    }

    #[test]
    fn salvages_truncated_array() {
        let content = r#"{"pairs":[{"id":0,"translation":"甲"},{"id":1,"translation":"乙"},{"id":2,"transl"#;
        let items = extract_items(content);
        assert_eq!(items.len(), 2, "应捞回前两个完整对象");
        assert_eq!(items[1].id(), Some(1));
    }

    #[test]
    fn accepts_string_ids() {
        let items = extract_items(r#"{"pairs":[{"id":"7","translation":"甲"}]}"#);
        assert_eq!(items[0].id(), Some(7));
    }

    #[test]
    fn parses_notes_key() {
        let items = extract_items(r#"{"notes":[{"id":3,"note":"俚语"}]}"#);
        assert_eq!(items[0].note.as_deref(), Some("俚语"));
    }

    #[test]
    fn ignores_junk_around_json() {
        let content = "Sure! Here is the json you asked for:\n{\"pairs\":[{\"id\":0,\"translation\":\"甲\"}]}\nHope it helps.";
        assert_eq!(extract_items(content).len(), 1);
    }

    #[test]
    fn extracts_sentences_from_envelope() {
        let content = r#"{"sentences":["First one.","Second one."]}"#;
        assert_eq!(extract_strings(content), vec!["First one.", "Second one."]);
    }

    #[test]
    fn extracts_sentences_from_fenced_and_bare() {
        assert_eq!(extract_strings("```json\n{\"sentences\":[\"甲。\",\"乙。\"]}\n```"), vec!["甲。", "乙。"]);
        assert_eq!(extract_strings(r#"["甲。","乙。"]"#), vec!["甲。", "乙。"]);
    }

    #[test]
    fn extracts_sentences_from_truncated_output() {
        let content = r#"{"sentences":["完整的一句。","另一句。","被截断的"#;
        let got = extract_strings(content);
        assert_eq!(got, vec!["完整的一句。", "另一句。"]);
    }

    #[test]
    fn extract_strings_handles_escapes_and_unicode() {
        let content = r#"{"sentences":["He said \"hi\".","第二句。"]}"#;
        assert_eq!(extract_strings(content), vec!["He said \"hi\".", "第二句。"]);
    }

    #[test]
    fn chunk_text_splits_long_paragraphs() {
        let text = "a".repeat(9500);
        let chunks = chunk_text(&text, 4000);
        assert!(chunks.len() >= 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4000));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn chunk_text_packs_paragraphs() {
        let text = "p1 para\n\np2 para\n\np3 para";
        let chunks = chunk_text(text, 4000);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("p1 para") && chunks[0].contains("p3 para"));
    }

    #[test]
    fn article_segments_roundtrip_via_envelope() {
        let segs = segments(2);
        let json = ArticleSegment::serialize_all(&segs);
        assert!(json.contains("\"kind\":\"article\""));

        match parse_content(&json) {
            Some(ParsedContent::Article(got)) => {
                assert_eq!(got.len(), 2);
                assert_eq!(got[1].text, "sentence 1");
            }
            _ => panic!("应识别为文章片段"),
        }
    }

    #[test]
    fn parse_content_distinguishes_kinds() {
        let sub = serde_json::to_string(&entries(2)).unwrap();
        assert!(matches!(parse_content(&sub), Some(ParsedContent::Subtitles(v)) if v.len() == 2));

        let art = ArticleSegment::serialize_all(&segments(3));
        assert!(matches!(parse_content(&art), Some(ParsedContent::Article(v)) if v.len() == 3));

        assert!(parse_content("plain text").is_none());
        // 老的无 kind 片段数组不再支持
        let legacy = serde_json::to_string(&segments(2)).unwrap();
        assert!(parse_content(&legacy).is_none());
    }

    #[test]
    fn format_segments_is_plain_text() {
        let mut segs = segments(2);
        segs[1].translation = Some("第二句译文".into());
        let out = format_segments(&segs);
        assert_eq!(out, "sentence 0\n第二句译文\n");
    }

    #[test]
    fn source_export_lists_originals() {
        let segs = segments(2);
        assert_eq!(format_segment_sources(&segs), "sentence 0\nsentence 1\n");
    }

    #[test]
    fn table_export_has_bom_header_and_crlf() {
        let mut segs = segments(2);
        segs[0].translation = Some("第一句".into());
        segs[0].note = Some("俚语".into());
        let out = format_segment_table(&segs, ["#", "原文", "译文", "注释"]);

        assert!(out.starts_with('\u{feff}'), "需要 BOM 以便 Excel 识别 UTF-8");
        assert!(out.contains("#,原文,译文,注释\r\n"));
        assert!(out.contains("1,sentence 0,第一句,俚语\r\n"));
        assert!(out.contains("2,sentence 1,,\r\n"));
    }

    #[test]
    fn table_export_escapes_commas_quotes_and_newlines() {
        let mut segs = segments(1);
        segs[0].text = "He said \"hi, there\"\nand left".into();
        let out = format_segment_table(&segs, ["a", "b", "c", "d"]);
        // 含逗号/引号/换行的字段必须整体加引号，内部引号翻倍
        assert!(out.contains("\"He said \"\"hi, there\"\"\nand left\""), "实际输出: {out}");
    }

    #[test]
    fn article_notes_survive_roundtrip() {
        let mut segs = segments(1);
        segs[0].note = Some("典故".into());
        let doc = ArticleSegment::serialize_all(&segs);
        match parse_content(&doc) {
            Some(ParsedContent::Article(got)) => assert_eq!(got[0].note.as_deref(), Some("典故")),
            _ => panic!("应解析为文章"),
        }
    }

    #[test]
    fn apply_notes_works_for_article_segments() {
        let mut segs = segments(2);
        let items = vec![
            RawItem { id: json!(1), translation: None, note: Some("注1".into()) },
        ];
        let (notes, ignored) = apply_notes(&mut segs, &[0, 1], &items);
        assert_eq!((notes.len(), ignored), (1, 0));
        assert_eq!(segs[1].note.as_deref(), Some("注1"));
        assert!(segs[0].note.is_none());
    }

    #[test]
    fn apply_translations_works_for_article_segments() {
        let mut segs = segments(3);
        let items = vec![
            RawItem { id: json!(0), translation: Some("甲".into()), note: None },
            RawItem { id: json!(2), translation: Some("乙".into()), note: None },
        ];
        let (filled, ignored) = apply_translations(&mut segs, &[0, 1, 2], &items);
        assert_eq!((filled.len(), ignored), (2, 0));
        assert_eq!(segs[2].translation.as_deref(), Some("乙"));
    }

    #[test]
    fn apply_translations_fills_duplicate_texts() {
        // 同一个 id 协议下，重复台词各自独立回填（旧实现只会填一条）
        let mut es = entries(3);
        es[0].text = "Yeah.".into();
        es[1].text = "Yeah.".into();
        es[2].text = "Yeah.".into();
        let items = vec![
            RawItem { id: json!(0), translation: Some("是啊".into()), note: None },
            RawItem { id: json!(1), translation: Some("是啊".into()), note: None },
            RawItem { id: json!(2), translation: Some("是啊".into()), note: None },
        ];
        let (filled, ignored) = apply_translations(&mut es, &[0, 1, 2], &items);
        assert_eq!((filled.len(), ignored), (3, 0));
        assert!(es.iter().all(|e| e.translation.as_deref() == Some("是啊")));
    }

    #[test]
    fn apply_translations_rejects_out_of_chunk_ids() {
        let mut es = entries(4);
        let items = vec![
            RawItem { id: json!(0), translation: Some("甲".into()), note: None },
            RawItem { id: json!(3), translation: Some("越界".into()), note: None },
            RawItem { id: json!(99), translation: Some("不存在".into()), note: None },
        ];
        let (filled, ignored) = apply_translations(&mut es, &[0, 1], &items);
        assert_eq!(filled.len(), 1);
        assert_eq!(ignored, 2);
        assert!(es[3].translation.is_none());
    }

    #[test]
    fn apply_translations_skips_empty_and_keeps_existing() {
        let mut es = entries(2);
        es[1].translation = Some("已有".into());
        let items = vec![
            RawItem { id: json!(0), translation: Some("   ".into()), note: None },
            RawItem { id: json!(1), translation: Some("覆盖".into()), note: None },
        ];
        let (filled, _) = apply_translations(&mut es, &[0, 1], &items);
        assert_eq!(filled.len(), 0);
        assert!(es[0].translation.is_none());
        assert_eq!(es[1].translation.as_deref(), Some("已有"));
    }

    #[test]
    fn apply_notes_appends_without_duplicating() {
        let mut es = entries(2);
        let items = vec![
            RawItem { id: json!(0), translation: None, note: Some("甲".into()) },
            RawItem { id: json!(1), translation: None, note: Some("乙".into()) },
        ];
        apply_notes(&mut es, &[0, 1], &items);
        apply_notes(&mut es, &[0, 1], &items);
        assert_eq!(es[0].note.as_deref(), Some("甲"));
        assert_eq!(es[1].note.as_deref(), Some("乙"));
    }

    #[test]
    fn snippet_never_splits_utf8() {
        let cjk = "翻".repeat(300);
        for n in 1..400 {
            let s = snippet(&cjk, n);
            assert!(cjk.starts_with(s));
            assert!(s.len() <= n);
        }
    }

    #[test]
    fn snippet_short_input_is_untouched() {
        assert_eq!(snippet("abc", 200), "abc");
    }

    #[test]
    fn norm_collapses_newlines_and_spaces() {
        assert_eq!(norm("a  b\nc\nd "), "a b c d");
        assert_eq!(norm("\n\n"), "");
    }

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE translations (id INTEGER PRIMARY KEY, translated_text TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    async fn read_doc<T: Segment>(pool: &SqlitePool, id: i64) -> Vec<T> {
        let raw: String = sqlx::query_scalar("SELECT translated_text FROM translations WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        T::parse_doc(&raw).expect("文档应可解析")
    }

    #[tokio::test]
    async fn merge_filled_does_not_clobber_concurrent_notes() {
        // 复现 P0：翻译任务和注释任务各自持快照时，后写者会覆盖前者
        let pool = test_pool().await;
        let id = 1i64;
        let doc = serde_json::to_string(&entries(3)).unwrap();
        sqlx::query("INSERT INTO translations (id, translated_text) VALUES (?, ?)")
            .bind(id).bind(&doc).execute(&pool).await.unwrap();

        // 翻译任务先写入第 0 条
        merge_filled::<SubtitleEntry>(&pool, id, &[(0, "译文0".into())]).await;
        // 注释任务基于更早的快照，只追加注释
        merge_notes::<SubtitleEntry>(&pool, id, &[(1, "注1".into())]).await;
        // 翻译任务继续写第 1 条（旧实现会用无注释的快照把注1覆盖掉）
        merge_filled::<SubtitleEntry>(&pool, id, &[(1, "译文1".into())]).await;

        let got: Vec<SubtitleEntry> = read_doc(&pool, id).await;
        assert_eq!(got[0].translation.as_deref(), Some("译文0"));
        assert_eq!(got[1].translation.as_deref(), Some("译文1"));
        assert_eq!(got[1].note.as_deref(), Some("注1"), "注释不能被翻译覆盖");
    }

    #[tokio::test]
    async fn merge_filled_keeps_user_edit() {
        let pool = test_pool().await;
        let id = 1i64;
        sqlx::query("INSERT INTO translations (id, translated_text) VALUES (?, ?)")
            .bind(id).bind(serde_json::to_string(&entries(1)).unwrap())
            .execute(&pool).await.unwrap();

        merge_edits::<SubtitleEntry>(&pool, id, &[(0, Some("用户手改".into()))]).await;
        // 后台任务晚到的一批译文不应覆盖用户编辑
        merge_filled::<SubtitleEntry>(&pool, id, &[(0, "模型译文".into())]).await;

        let got: Vec<SubtitleEntry> = read_doc(&pool, id).await;
        assert_eq!(got[0].translation.as_deref(), Some("用户手改"));
    }

    #[tokio::test]
    async fn merge_edits_can_clear_translation() {
        let pool = test_pool().await;
        let id = 1i64;
        let mut init = entries(1);
        init[0].translation = Some("原文里有".into());
        sqlx::query("INSERT INTO translations (id, translated_text) VALUES (?, ?)")
            .bind(id).bind(serde_json::to_string(&init).unwrap())
            .execute(&pool).await.unwrap();

        merge_edits::<SubtitleEntry>(&pool, id, &[(0, None)]).await;
        let got: Vec<SubtitleEntry> = read_doc(&pool, id).await;
        assert!(got[0].translation.is_none(), "空值应清空译文");
    }

    #[tokio::test]
    async fn merge_works_for_article_envelope() {
        let pool = test_pool().await;
        let id = 2i64;
        sqlx::query("INSERT INTO translations (id, translated_text) VALUES (?, ?)")
            .bind(id).bind(ArticleSegment::serialize_all(&segments(2)))
            .execute(&pool).await.unwrap();

        merge_filled::<ArticleSegment>(&pool, id, &[(1, "第二句".into())]).await;

        let got: Vec<ArticleSegment> = read_doc(&pool, id).await;
        assert_eq!(got[1].translation.as_deref(), Some("第二句"));
        // 落库仍是带 kind 的信封
        let raw: String = sqlx::query_scalar("SELECT translated_text FROM translations WHERE id = ?")
            .bind(id).fetch_one(&pool).await.unwrap();
        assert!(raw.contains("\"kind\":\"article\""));
    }

    #[test]
    fn concurrency_env_is_parsed_and_clamped() {
        assert_eq!(parse_concurrency(None), DEFAULT_CONCURRENCY);
        assert_eq!(parse_concurrency(Some("")), DEFAULT_CONCURRENCY);
        assert_eq!(parse_concurrency(Some("abc")), DEFAULT_CONCURRENCY);
        assert_eq!(parse_concurrency(Some("5")), 5);
        assert_eq!(parse_concurrency(Some(" 2 ")), 2);
        assert_eq!(parse_concurrency(Some("0")), 1);
        assert_eq!(parse_concurrency(Some("999")), MAX_CONCURRENCY);
    }

    #[test]
    fn batching_shrinks_to_ten() {
        assert_eq!(batch_sizes(246), vec![100, 50, 25, 12, 6]);
        assert_eq!(batch_sizes(6), vec![6]);
        assert_eq!(batch_sizes(100), vec![100, 50, 25, 12, 6]);
    }

    #[test]
    fn vtt_export_keeps_settings() {
        let e = entry("hello", Some("align:center line:81%"));
        let out = format_subtitles(&[e], "vtt");
        assert!(out.starts_with("WEBVTT\n\n"));
        assert!(out.contains("00:00:01.000 --> 00:00:02.000 align:center line:81%\nhello"));
    }

    #[test]
    fn vtt_export_without_settings_has_no_trailing_space() {
        let out = format_subtitles(&[entry("hello", None)], "vtt");
        assert!(out.contains("00:00:01.000 --> 00:00:02.000\nhello"));
    }

    #[test]
    fn txt_export_is_plain_not_vtt() {
        let mut e = entry("hello", Some("align:center line:81%"));
        e.translation = Some("你好".to_string());
        let out = format_subtitles(&[e], "txt");
        assert_eq!(out, "你好\n");
        assert!(!out.contains("WEBVTT"));
        assert!(!out.contains("-->"));
    }

    #[test]
    fn srt_export_uses_commas_and_renumbers() {
        let out = format_subtitles(&[entry("hello", None)], "srt");
        assert!(out.contains("1\n00:00:01,000 --> 00:00:02,000\nhello"));
    }
}
