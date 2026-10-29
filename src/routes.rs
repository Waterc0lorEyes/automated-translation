//! JSON API + SPA 静态资源服务。
//!
//! 前端是 Vue SPA，所有页面逻辑在 `frontend/`，构建产物落在 `static/`。
//! 这里只负责三件事：
//!   1. `/api/*` 的 JSON 接口
//!   2. `/download/{id}/{format}` 的文件下载（浏览器直接跳转，不走 fetch）
//!   3. 其余路径回退到 `static/index.html`，交给 Vue Router
//!
//! 认证仍然是签名 Cookie（见 session.rs），所以 fetch 一律 `credentials: same-origin`。
//! 所有写操作额外做了 Origin 校验：SameSite=Lax 已经挡住跨站 POST，
//! 这里再加一层，防止未来改成 SameSite=None 时裸奔。

use std::sync::Arc;

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{Multipart, Path, Request, State},
    http::{header, HeaderValue, Method, StatusCode, Uri},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use axum_extra::extract::cookie::{Cookie, CookieJar as Jar, SameSite};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tower_http::services::ServeDir;

use crate::i18n::I18n;
use crate::models::{AnnotateForm, ArticleSegment, LoginForm, RegisterForm, SubtitleEntry, Translation, User};
use crate::parser::parse_subtitles;
use crate::session;
use crate::translator::{self, ParsedContent, Segment};

pub struct AppState {
    pub pool: SqlitePool,
    pub default_api_key: String,
    /// 会话签名密钥
    pub session_secret: Vec<u8>,
    /// 是否给 Cookie 加 Secure 属性（https 部署时开启）
    pub cookie_secure: bool,
}

/* ------------------------------------------------------------------ */
/* 路由                                                                */
/* ------------------------------------------------------------------ */

pub fn router() -> Router<Arc<AppState>> {
    let api = Router::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/me", get(me))
        .route("/translations", get(list_translations).post(create_translation))
        .route("/translations/{id}", get(get_translation))
        .route("/translations/{id}/status", get(get_status))
        .route("/translations/{id}/edits", post(put_edits))
        .route("/translations/{id}/annotate", post(annotate))
        .route("/translations/{id}/continue", post(continue_translation))
        .route("/settings", get(settings_get).put(settings_put))
        .route("/lang", post(set_lang))
        .fallback(api_not_found);

    Router::new()
        .nest("/api", api)
        .route("/download/{id}/{format}", get(download))
        // 静态文件优先，找不到再回退到 index.html（Vue Router 接管）
        .fallback_service(ServeDir::new("static").fallback(get(spa_index)))
}

/// 取 authority 里的主机名，忽略端口；兼容 IPv6 的 `[::1]:8080`。
fn hostname(authority: &str) -> &str {
    let value = authority.trim();
    if let Some(rest) = value.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return &value[..end + 2];
        }
    }
    value.split(':').next().unwrap_or(value)
}

/// 写操作的 Origin 校验：Origin 和 Host 的主机名必须一致。
pub async fn origin_guard(req: Request, next: Next) -> Response {
    let mutating = matches!(
        *req.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );

    if mutating {
        let origin = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        if let Some(origin) = origin {
            let origin_authority = origin.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("");
            // 只比较主机名：反代常把 Host 的端口去掉（proxy_set_header Host $host），
            // 严格要求端口一致会误杀正常请求。跨站请求的 Host 本来就不是我们的域名。
            if !host.is_empty() && !hostname(origin_authority).eq_ignore_ascii_case(hostname(&host)) {
                tracing::warn!("拒绝跨站写请求: origin={} host={}", origin, host);
                return api_error(StatusCode::FORBIDDEN, "bad_origin");
            }
        }
    }

    next.run(req).await
}

/* ------------------------------------------------------------------ */
/* 小工具                                                              */
/* ------------------------------------------------------------------ */

fn api_error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

/// `require_user` 的错误用这个小结构，避免 `Result<User, Response>` 触发
/// clippy::result_large_err（Response 很大）。
struct ApiError {
    status: StatusCode,
    code: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.code }))).into_response()
    }
}

fn internal(e: impl std::fmt::Display) -> Response {
    tracing::error!("api error: {}", e);
    api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
}

async fn get_user(jar: &Jar, state: &AppState) -> Option<User> {
    let raw = jar.get(session::COOKIE_NAME)?.value();
    let user_id = session::verify(&state.session_secret, raw)?;
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
}

async fn require_user(jar: &Jar, state: &AppState) -> Result<User, ApiError> {
    get_user(jar, state).await.ok_or(ApiError {
        status: StatusCode::UNAUTHORIZED,
        code: "unauthorized",
    })
}

fn get_api_key(user: &User, state: &AppState) -> String {
    let key = user.api_key.as_deref().unwrap_or("");
    if !key.is_empty() {
        return key.to_string();
    }
    state.default_api_key.clone()
}

fn user_json(user: &User, state: &AppState) -> Value {
    let key = get_api_key(user, state);
    json!({
        "id": user.id,
        "username": user.username,
        "has_key": !key.is_empty(),
        "masked_key": mask_key(&key),
    })
}

fn fmt_time(t: chrono::NaiveDateTime) -> String {
    t.format("%Y-%m-%d %H:%M").to_string()
}

/// 把一条记录拆成 JSON。`with_entries=false` 用于列表，避免一次传几百 KB。
fn record_json(t: &Translation, with_entries: bool) -> Value {
    let parsed = translator::parse_content(&t.translated_text);
    let (kind, entries, total, done) = match &parsed {
        Some(ParsedContent::Subtitles(items)) => (
            "subtitle",
            serde_json::to_value(items).unwrap_or(Value::Null),
            items.len(),
            progress_done(items),
        ),
        Some(ParsedContent::Article(items)) => (
            "article",
            serde_json::to_value(items).unwrap_or(Value::Null),
            items.len(),
            progress_done(items),
        ),
        None => ("legacy", Value::Null, 0, 0),
    };

    let mut body = json!({
        "id": t.id,
        "filename": t.filename,
        "kind": kind,
        "source_lang": t.source_lang,
        "target_lang": t.target_lang,
        "status": t.status,
        "created_at": fmt_time(t.created_at),
        "total": total,
        "done": done,
    });

    if with_entries {
        body["entries"] = entries;
    }
    body
}

fn progress_done<T: Segment>(items: &[T]) -> usize {
    items
        .iter()
        .filter(|e| e.translation().is_some_and(|t| !t.is_empty()))
        .count()
}

async fn fetch_record(state: &AppState, user_id: i64, id: i64) -> Option<Translation> {
    sqlx::query_as::<_, Translation>("SELECT * FROM translations WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
}

async fn finish_job(pool: &SqlitePool, id: i64, result: anyhow::Result<()>) {
    match result {
        Ok(()) => translator::set_status(pool, id, "done").await,
        Err(e) => {
            tracing::error!("bg job failed: {} id={}", e, id);
            translator::set_status(pool, id, "failed").await;
        }
    }
}

/* ------------------------------------------------------------------ */
/* 认证                                                                */
/* ------------------------------------------------------------------ */

async fn register(State(state): State<Arc<AppState>>, jar: Jar, Json(form): Json<RegisterForm>) -> Response {
    if form.username.trim().is_empty() || form.password.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "empty_fields");
    }
    if form.password != form.password_confirm {
        return api_error(StatusCode::BAD_REQUEST, "password_mismatch");
    }
    if form.password.len() < 4 {
        return api_error(StatusCode::BAD_REQUEST, "password_short");
    }

    let existing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE username = ?")
        .bind(&form.username)
        .fetch_one(&state.pool)
        .await;

    match existing {
        Ok(count) if count > 0 => return api_error(StatusCode::CONFLICT, "username_taken"),
        Err(e) => return internal(e),
        _ => {}
    }

    let salt = SaltString::generate(&mut OsRng);
    let password_hash = match Argon2::default().hash_password(form.password.as_bytes(), &salt) {
        Ok(h) => h.to_string(),
        Err(e) => return internal(e),
    };

    let inserted = sqlx::query("INSERT INTO users (username, password_hash) VALUES (?, ?)")
        .bind(form.username.trim())
        .bind(&password_hash)
        .execute(&state.pool)
        .await;

    let id = match inserted {
        Ok(r) => r.last_insert_rowid(),
        Err(e) if e.to_string().contains("UNIQUE") => {
            return api_error(StatusCode::CONFLICT, "username_taken")
        }
        Err(e) => return internal(e),
    };

    let user = User {
        id,
        username: form.username.trim().to_string(),
        password_hash,
        api_key: None,
        created_at: chrono::Utc::now().naive_utc(),
    };
    let jar = jar.add(session::build_cookie(&state.session_secret, id, state.cookie_secure));

    (jar, Json(json!({ "user": user_json(&user, &state) }))).into_response()
}

async fn login(State(state): State<Arc<AppState>>, jar: Jar, Json(form): Json<LoginForm>) -> Response {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(form.username.trim())
        .fetch_optional(&state.pool)
        .await;

    let user = match user {
        Ok(Some(u)) => u,
        Ok(None) => return api_error(StatusCode::UNAUTHORIZED, "invalid_credentials"),
        Err(e) => return internal(e),
    };

    let ok = PasswordHash::new(&user.password_hash)
        .map(|hash| Argon2::default().verify_password(form.password.as_bytes(), &hash).is_ok())
        .unwrap_or(false);

    if !ok {
        return api_error(StatusCode::UNAUTHORIZED, "invalid_credentials");
    }

    let jar = jar.add(session::build_cookie(&state.session_secret, user.id, state.cookie_secure));
    (jar, Json(json!({ "user": user_json(&user, &state) }))).into_response()
}

async fn logout(jar: Jar) -> Response {
    (jar.remove(session::clear_cookie()), StatusCode::NO_CONTENT).into_response()
}

async fn me(State(state): State<Arc<AppState>>, jar: Jar) -> Response {
    match require_user(&jar, &state).await {
        Ok(user) => Json(json!({ "user": user_json(&user, &state) })).into_response(),
        Err(err) => err.into_response(),
    }
}

/* ------------------------------------------------------------------ */
/* 记录                                                                */
/* ------------------------------------------------------------------ */

async fn list_translations(State(state): State<Arc<AppState>>, jar: Jar) -> Response {
    let user = match require_user(&jar, &state).await {
        Ok(u) => u,
        Err(err) => return err.into_response(),
    };

    let rows = sqlx::query_as::<_, Translation>(
        "SELECT * FROM translations WHERE user_id = ? ORDER BY created_at DESC, id DESC LIMIT 100",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await;

    match rows {
        Ok(rows) => {
            let items: Vec<Value> = rows.iter().map(|t| record_json(t, false)).collect();
            Json(json!({ "items": items })).into_response()
        }
        Err(e) => internal(e),
    }
}

async fn get_translation(State(state): State<Arc<AppState>>, jar: Jar, Path(id): Path<i64>) -> Response {
    let user = match require_user(&jar, &state).await {
        Ok(u) => u,
        Err(err) => return err.into_response(),
    };

    match fetch_record(&state, user.id, id).await {
        Some(record) => Json(record_json(&record, true)).into_response(),
        None => api_error(StatusCode::NOT_FOUND, "not_found"),
    }
}

async fn get_status(State(state): State<Arc<AppState>>, jar: Jar, Path(id): Path<i64>) -> Response {
    let user = match require_user(&jar, &state).await {
        Ok(u) => u,
        Err(err) => return err.into_response(),
    };

    let record = match fetch_record(&state, user.id, id).await {
        Some(record) => record,
        None => return api_error(StatusCode::NOT_FOUND, "not_found"),
    };

    let body = record_json(&record, true);
    Json(json!({
        "total": body["total"],
        "done": body["done"],
        "status": body["status"],
        "entries": body["entries"],
    }))
    .into_response()
}

/// 上传并启动翻译。multipart 字段与旧表单保持一致：
/// kind=subtitle|article，file 或 text，source_lang，target_lang，prompt。
async fn create_translation(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    mut multipart: Multipart,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Ok(u) => u,
        Err(err) => return err.into_response(),
    };

    let api_key = get_api_key(&user, &state);
    if api_key.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "no_api_key");
    }

    let mut kind = String::from("subtitle");
    let mut filename = String::new();
    let mut content = String::new();
    let mut prompt = String::new();
    let mut source_lang = String::from("en");
    let mut target_lang = String::from("zh");

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "kind" => kind = field.text().await.unwrap_or_default(),
            "file" => {
                filename = field.file_name().unwrap_or("").to_string();
                content = field.text().await.unwrap_or_default();
            }
            "text" => {
                let text = field.text().await.unwrap_or_default();
                if !text.is_empty() {
                    content = text;
                }
            }
            "prompt" => prompt = field.text().await.unwrap_or_default(),
            "source_lang" => source_lang = field.text().await.unwrap_or_default(),
            "target_lang" => target_lang = field.text().await.unwrap_or_default(),
            _ => {}
        }
    }

    if content.trim().is_empty() {
        return api_error(StatusCode::UNPROCESSABLE_ENTITY, "no_content");
    }

    let is_article = kind == "article";
    if filename.trim().is_empty() {
        filename = if is_article { "article.txt" } else { "subtitle.vtt" }.to_string();
    }

    if is_article {
        create_article(&state, &user, api_key, filename, content, source_lang, target_lang, prompt).await
    } else {
        create_subtitle(&state, &user, api_key, filename, content, source_lang, target_lang, prompt).await
    }
}

#[allow(clippy::too_many_arguments)]
async fn create_subtitle(
    state: &Arc<AppState>,
    user: &User,
    api_key: String,
    filename: String,
    content: String,
    source_lang: String,
    target_lang: String,
    prompt: String,
) -> Response {
    let entries = parse_subtitles(&content);
    if entries.is_empty() {
        return api_error(StatusCode::UNPROCESSABLE_ENTITY, "parse_failed");
    }
    let entry_count = entries.len();

    let entries_json = serde_json::to_string(&entries).unwrap_or_default();
    let inserted = sqlx::query(
        "INSERT INTO translations (user_id, filename, source_text, translated_text, source_lang, target_lang, status) \
         VALUES (?, ?, ?, ?, ?, ?, 'running')",
    )
    .bind(user.id)
    .bind(&filename)
    .bind(&content)
    .bind(&entries_json)
    .bind(&source_lang)
    .bind(&target_lang)
    .execute(&state.pool)
    .await;

    let id = match inserted {
        Ok(r) => r.last_insert_rowid(),
        Err(e) => return internal(e),
    };

    tracing::info!("subtitle: {} entries, {}→{} id={}", entry_count, source_lang, target_lang, id);

    let pool = state.pool.clone();
    let key = api_key.clone();
    let sl = source_lang.clone();
    let tl = target_lang.clone();
    tokio::spawn(async move {
        let (entries, result) = translator::translate_subtitles_owned(key, entries, sl, tl, prompt, pool.clone(), id).await;
        match result {
            Ok(()) => {
                let filled = progress_done(&entries);
                translator::set_status(&pool, id, "done").await;
                tracing::info!("bg translate done: {}/{} id={}", filled, entry_count, id);
            }
            Err(e) => {
                translator::set_status(&pool, id, "failed").await;
                tracing::error!("bg translate failed: {} id={}", e, id);
            }
        }
    });

    (StatusCode::ACCEPTED, Json(json!({ "id": id }))).into_response()
}

#[allow(clippy::too_many_arguments)]
async fn create_article(
    state: &Arc<AppState>,
    user: &User,
    api_key: String,
    filename: String,
    content: String,
    source_lang: String,
    target_lang: String,
    prompt: String,
) -> Response {
    // 先落一条 "splitting" 占位记录：拆句要跑 AI，请求必须立刻返回
    let placeholder = serde_json::to_string(&json!({ "kind": "article", "segments": [] })).unwrap_or_default();
    let inserted = sqlx::query(
        "INSERT INTO translations (user_id, filename, source_text, translated_text, source_lang, target_lang, status) \
         VALUES (?, ?, ?, ?, ?, ?, 'splitting')",
    )
    .bind(user.id)
    .bind(&filename)
    .bind(&content)
    .bind(&placeholder)
    .bind(&source_lang)
    .bind(&target_lang)
    .execute(&state.pool)
    .await;

    let id = match inserted {
        Ok(r) => r.last_insert_rowid(),
        Err(e) => return internal(e),
    };

    tracing::info!("article: {}→{} {} chars, 开始拆句 id={}", source_lang, target_lang, content.chars().count(), id);

    let pool = state.pool.clone();
    let key = api_key.clone();
    let sl = source_lang.clone();
    let tl = target_lang.clone();

    tokio::spawn(async move {
        let segments = match build_segments(&key, &content, &sl).await {
            Ok(segments) => segments,
            Err(e) => {
                tracing::error!("文章拆句失败 id={}: {}", id, e);
                translator::set_status(&pool, id, "failed").await;
                return;
            }
        };
        tracing::info!("article: 拆成 {} 句 id={}", segments.len(), id);

        translator::replace_doc::<ArticleSegment>(&pool, id, &segments).await;
        translator::set_status(&pool, id, "running").await;

        let total = segments.len();
        let (segments, result) = translator::translate_article_owned(key, segments, sl, tl, prompt, pool.clone(), id).await;
        match result {
            Ok(()) => {
                let done = progress_done(&segments);
                translator::set_status(&pool, id, "done").await;
                tracing::info!("bg article translate done: {}/{} id={}", done, total, id);
            }
            Err(e) => {
                translator::set_status(&pool, id, "failed").await;
                tracing::error!("bg article translate failed: {} id={}", e, id);
            }
        }
    });

    (StatusCode::ACCEPTED, Json(json!({ "id": id }))).into_response()
}

async fn build_segments(api_key: &str, text: &str, source_lang: &str) -> anyhow::Result<Vec<ArticleSegment>> {
    let sentences = translator::split_article(api_key, text, source_lang).await?;
    if sentences.is_empty() {
        anyhow::bail!("拆分结果为空");
    }
    let sentences = translator::verify_sentences(api_key, &sentences, source_lang).await;
    if sentences.is_empty() {
        anyhow::bail!("校验后句子为空");
    }
    Ok(sentences
        .iter()
        .enumerate()
        .map(|(i, s)| ArticleSegment {
            index: i + 1,
            text: s.clone(),
            translation: None,
            note: None,
        })
        .collect())
}

async fn put_edits(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    Path(id): Path<i64>,
    Json(data): Json<Value>,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Ok(u) => u,
        Err(err) => return err.into_response(),
    };

    let record = match fetch_record(&state, user.id, id).await {
        Some(record) => record,
        None => return api_error(StatusCode::NOT_FOUND, "not_found"),
    };

    let empty = serde_json::Map::new();
    let fields = data.as_object().unwrap_or(&empty);

    // key 是数组下标（0-based），空串表示清空
    let edits: Vec<(usize, Option<String>)> = fields
        .iter()
        .filter_map(|(key, value)| {
            let index = key.parse::<usize>().ok()?;
            let text = value.as_str()?.trim().to_string();
            Some((index, if text.is_empty() { None } else { Some(text) }))
        })
        .collect();

    match translator::parse_content(&record.translated_text) {
        Some(ParsedContent::Subtitles(_)) => {
            translator::merge_edits::<SubtitleEntry>(&state.pool, id, &edits).await;
        }
        Some(ParsedContent::Article(_)) => {
            translator::merge_edits::<ArticleSegment>(&state.pool, id, &edits).await;
        }
        None => return api_error(StatusCode::CONFLICT, "legacy_format"),
    }

    StatusCode::NO_CONTENT.into_response()
}

async fn annotate(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    Path(id): Path<i64>,
    Json(form): Json<AnnotateForm>,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Ok(u) => u,
        Err(err) => return err.into_response(),
    };

    let api_key = get_api_key(&user, &state);
    if api_key.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "no_api_key");
    }

    let topic = form.topic.unwrap_or_default().trim().to_string();
    if topic.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "empty_topic");
    }
    let note_lang = form.note_lang.unwrap_or_else(|| "zh".to_string());

    let record = match fetch_record(&state, user.id, id).await {
        Some(record) => record,
        None => return api_error(StatusCode::NOT_FOUND, "not_found"),
    };

    let content = match translator::parse_content(&record.translated_text) {
        Some(content) => content,
        None => return api_error(StatusCode::CONFLICT, "legacy_format"),
    };

    if !translator::try_claim(&state.pool, id).await {
        return api_error(StatusCode::CONFLICT, "busy");
    }

    let lang_name = match note_lang.as_str() {
        "zh" => "Chinese",
        "ja" => "Japanese",
        _ => "English",
    };
    let prompt = format!("{} (respond in {})", topic, lang_name);

    let pool = state.pool.clone();
    let key = api_key.clone();
    match content {
        ParsedContent::Subtitles(entries) => {
            tokio::spawn(async move {
                let (_, result) = translator::generate_annotations_owned::<SubtitleEntry>(key, entries, prompt, pool.clone(), id).await;
                finish_job(&pool, id, result).await;
            });
        }
        ParsedContent::Article(segments) => {
            tokio::spawn(async move {
                let (_, result) = translator::generate_annotations_owned::<ArticleSegment>(key, segments, prompt, pool.clone(), id).await;
                finish_job(&pool, id, result).await;
            });
        }
    }

    (StatusCode::ACCEPTED, Json(json!({ "id": id }))).into_response()
}

async fn continue_translation(State(state): State<Arc<AppState>>, jar: Jar, Path(id): Path<i64>) -> Response {
    let user = match require_user(&jar, &state).await {
        Ok(u) => u,
        Err(err) => return err.into_response(),
    };

    let api_key = get_api_key(&user, &state);
    if api_key.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "no_api_key");
    }

    let record = match fetch_record(&state, user.id, id).await {
        Some(record) => record,
        None => return api_error(StatusCode::NOT_FOUND, "not_found"),
    };

    if !translator::try_claim(&state.pool, id).await {
        return api_error(StatusCode::CONFLICT, "busy");
    }

    let pool = state.pool.clone();
    let sl = record.source_lang.clone();
    let tl = record.target_lang.clone();

    match translator::parse_content(&record.translated_text) {
        Some(ParsedContent::Subtitles(mut entries)) => {
            tokio::spawn(async move {
                let result = translator::continue_translate(&api_key, &mut entries, &sl, &tl, "", &pool, id).await;
                match result {
                    Ok(()) => translator::set_status(&pool, id, "done").await,
                    Err(e) => {
                        tracing::error!("continue failed: {} id={}", e, id);
                        translator::set_status(&pool, id, "failed").await;
                    }
                }
            });
        }
        Some(ParsedContent::Article(mut segments)) => {
            tokio::spawn(async move {
                let result = translator::continue_translate_article(&api_key, &mut segments, &sl, &tl, "", &pool, id).await;
                match result {
                    Ok(()) => translator::set_status(&pool, id, "done").await,
                    Err(e) => {
                        tracing::error!("continue failed: {} id={}", e, id);
                        translator::set_status(&pool, id, "failed").await;
                    }
                }
            });
        }
        None => {
            translator::set_status(&state.pool, id, "done").await;
            return api_error(StatusCode::CONFLICT, "legacy_format");
        }
    }

    (StatusCode::ACCEPTED, Json(json!({ "id": id }))).into_response()
}

/* ------------------------------------------------------------------ */
/* 设置 / 语言                                                         */
/* ------------------------------------------------------------------ */

async fn settings_get(State(state): State<Arc<AppState>>, jar: Jar) -> Response {
    let user = match require_user(&jar, &state).await {
        Ok(u) => u,
        Err(err) => return err.into_response(),
    };
    let key = get_api_key(&user, &state);
    Json(json!({
        "has_key": !key.is_empty(),
        "masked_key": mask_key(&key),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct ApiKeyBody {
    api_key: String,
}

async fn settings_put(State(state): State<Arc<AppState>>, jar: Jar, Json(body): Json<ApiKeyBody>) -> Response {
    let user = match require_user(&jar, &state).await {
        Ok(u) => u,
        Err(err) => return err.into_response(),
    };

    let key = body.api_key.trim();
    // 空值表示不允许清空（否则会静默回落到全局 Key，容易误解）
    if key.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "empty_key");
    }

    if let Err(e) = sqlx::query("UPDATE users SET api_key = ? WHERE id = ?")
        .bind(key)
        .bind(user.id)
        .execute(&state.pool)
        .await
    {
        return internal(e);
    }

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
struct LangBody {
    lang: String,
}

async fn set_lang(jar: Jar, Json(body): Json<LangBody>) -> Response {
    if !crate::i18n::is_supported(&body.lang) {
        return api_error(StatusCode::BAD_REQUEST, "unsupported_lang");
    }
    // 后端的 CSV 表头跟着 lang cookie 走，所以这一条必须落到 Cookie 上
    let cookie = Cookie::build(("lang", body.lang))
        .path("/")
        .same_site(SameSite::Lax)
        .max_age(time::Duration::days(365))
        .build();
    (jar.add(cookie), StatusCode::NO_CONTENT).into_response()
}

/* ------------------------------------------------------------------ */
/* 下载                                                                */
/* ------------------------------------------------------------------ */

fn current_lang(jar: &Jar) -> String {
    let raw = jar
        .get("lang")
        .map(|c| c.value().to_string())
        .unwrap_or_else(|| crate::i18n::DEFAULT_LANG.to_string());
    if crate::i18n::is_supported(&raw) {
        raw
    } else {
        crate::i18n::DEFAULT_LANG.to_string()
    }
}

async fn download(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    Path((id, format)): Path<(i64, String)>,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Ok(u) => u,
        Err(_) => return api_error(StatusCode::UNAUTHORIZED, "unauthorized"),
    };

    let record = match fetch_record(&state, user.id, id).await {
        Some(record) => record,
        None => return api_error(StatusCode::NOT_FOUND, "not_found"),
    };

    let format = format.to_ascii_lowercase();
    let base = strip_known_ext(&record.filename);
    let base = if base.trim().is_empty() { "translation" } else { base };

    let (content, ext, mime, prefix) = match translator::parse_content(&record.translated_text) {
        Some(ParsedContent::Subtitles(entries)) => {
            if !matches!(format.as_str(), "vtt" | "srt" | "txt") {
                return api_error(StatusCode::BAD_REQUEST, "unsupported_format");
            }
            let mime = if format == "vtt" {
                "text/vtt; charset=utf-8"
            } else {
                "text/plain; charset=utf-8"
            };
            (
                translator::format_subtitles(&entries, &format),
                format.clone(),
                mime,
                "translated",
            )
        }
        Some(ParsedContent::Article(segments)) => match format.as_str() {
            "txt" => (
                translator::format_segments(&segments),
                "txt".to_string(),
                "text/plain; charset=utf-8",
                "translated",
            ),
            "src" => (
                translator::format_segment_sources(&segments),
                "txt".to_string(),
                "text/plain; charset=utf-8",
                "source",
            ),
            "csv" => {
                let i18n = I18n::new(&current_lang(&jar));
                let headers = [
                    i18n.t("th_index"),
                    i18n.t("th_original"),
                    i18n.t("th_translation"),
                    i18n.t("th_note"),
                ];
                (
                    translator::format_segment_table(&segments, headers),
                    "csv".to_string(),
                    "text/csv; charset=utf-8",
                    "bilingual",
                )
            }
            _ => return api_error(StatusCode::BAD_REQUEST, "unsupported_format"),
        },
        None => return api_error(StatusCode::GONE, "legacy_format"),
    };

    let download_name = format!("{}_{}.{}", prefix, base, ext);
    let mut response = Response::new(axum::body::Body::from(content));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    response
        .headers_mut()
        .insert(header::CONTENT_DISPOSITION, content_disposition(&download_name));
    response
}

/* ------------------------------------------------------------------ */
/* SPA 回退                                                            */
/* ------------------------------------------------------------------ */

async fn api_not_found() -> Response {
    api_error(StatusCode::NOT_FOUND, "not_found")
}

/// 非 `/api`、非静态文件的 GET 一律交给前端路由。
/// 带扩展名的路径（比如 /assets/x.js）返回 404，避免把 HTML 当成 JS 发出去。
async fn spa_index(uri: Uri) -> Response {
    let path = uri.path();
    let last = path.rsplit('/').next().unwrap_or("");
    if last.contains('.') {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    match tokio::fs::read("static/index.html").await {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            bytes,
        )
            .into_response(),
        Err(e) => {
            tracing::error!("读取 static/index.html 失败: {}（先跑一次前端构建？）", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "前端资源缺失：请在 frontend/ 执行 npm run build",
            )
                .into_response()
        }
    }
}

/* ------------------------------------------------------------------ */
/* 文件名 / Header 工具                                                */
/* ------------------------------------------------------------------ */

fn strip_known_ext(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    for ext in [".vtt", ".srt", ".txt"] {
        if lower.ends_with(ext) && name.len() > ext.len() {
            return &name[..name.len() - ext.len()];
        }
    }
    name
}

fn content_disposition(filename: &str) -> HeaderValue {
    let ascii = ascii_fallback_name(filename);
    let encoded = pct_encode(filename);
    let value = format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        ascii, encoded
    );
    HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

/// 非 ASCII / 控制字符统一替换成 `_`，保证 header 合法。
fn ascii_fallback_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    out = out.trim_matches(|c: char| c == '.' || c == ' ').to_string();
    if out.is_empty() {
        out.push_str("download");
    }
    if out.len() > 64 {
        out.truncate(64); // 全 ASCII，按字节截断安全
    }
    out
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn mask_key(key: &str) -> String {
    // 按字符切，避免用户填入非 ASCII 时切在字节中间 panic
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return "****".to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{}****{}", head, tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_is_always_valid_header() {
        for name in [
            "translated_a.vtt",
            "translated_英文字幕.txt",
            "translated_\"quote\".vtt",
            "translated_x\r\nSet-Cookie: evil=1.vtt",
            "translated_\u{7}.vtt",
        ] {
            let hv = content_disposition(name);
            assert!(hv.to_str().is_ok(), "非法 header: {name:?}");
            let s = hv.to_str().unwrap();
            assert!(s.starts_with("attachment;"));
            assert!(!s.contains('\r') && !s.contains('\n'));
        }
    }

    #[test]
    fn content_disposition_encodes_non_ascii() {
        let s = content_disposition("translated_英文字幕.txt")
            .to_str()
            .unwrap()
            .to_string();
        assert!(s.contains("filename*=UTF-8''"), "{s}");
        assert!(!s.contains('英'), "{s}");
    }

    #[test]
    fn ascii_fallback_keeps_it_safe() {
        assert_eq!(ascii_fallback_name("a b.txt"), "a b.txt");
        assert_eq!(ascii_fallback_name("英文字幕.vtt"), "____.vtt");
        assert_eq!(ascii_fallback_name("..."), "download");
        assert_eq!(ascii_fallback_name(""), "download");
        assert!(ascii_fallback_name(&"x".repeat(200)).len() <= 64);
    }

    #[test]
    fn pct_encode_escapes_reserved() {
        assert_eq!(pct_encode("abc-._~"), "abc-._~");
        assert_eq!(pct_encode("a b"), "a%20b");
        assert_eq!(pct_encode("英"), "%E8%8B%B1");
    }

    #[test]
    fn strip_ext_removes_one_known_suffix() {
        assert_eq!(strip_known_ext("a.vtt"), "a");
        assert_eq!(strip_known_ext("a.SRT"), "a");
        assert_eq!(strip_known_ext("a.txt"), "a");
        assert_eq!(strip_known_ext("a.vtt.vtt"), "a.vtt");
        assert_eq!(strip_known_ext("noext"), "noext");
    }

    #[test]
    fn mask_key_handles_utf8() {
        assert_eq!(mask_key("short"), "****");
        assert_eq!(mask_key("sk-1234567890"), "sk-1****7890");
        assert_eq!(mask_key("密钥密钥密钥密钥密钥"), "密钥密钥****密钥密钥");
    }

    #[test]
    fn hostname_ignores_port_and_handles_ipv6() {
        assert_eq!(hostname("example.com:8080"), "example.com");
        assert_eq!(hostname("example.com"), "example.com");
        assert_eq!(hostname("127.0.0.1:18080"), "127.0.0.1");
        assert_eq!(hostname("[::1]:8080"), "[::1]");
        assert_eq!(hostname("[::1]"), "[::1]");
    }

    #[test]
    fn record_json_marks_legacy_format() {
        let record = Translation {
            id: 1,
            user_id: 1,
            filename: "old.txt".into(),
            source_text: "x".into(),
            translated_text: "just plain text".into(),
            source_lang: "en".into(),
            target_lang: "zh".into(),
            status: "done".into(),
            created_at: chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(10, 30, 0)
                .unwrap(),
        };
        let body = record_json(&record, false);
        assert_eq!(body["kind"], "legacy");
        assert_eq!(body["total"], 0);
        assert_eq!(body["created_at"], "2026-01-01 10:30");
    }
}
