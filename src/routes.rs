use std::sync::Arc;

use argon2::{
    Argon2,
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router, Json,
};
use axum_extra::extract::cookie::{CookieJar as Cookies, Cookie};
use sqlx::SqlitePool;
use tera::Tera;

use crate::models::{ApiKeyForm, AnnotateForm, ArticleSegment, LoginForm, RegisterForm, Translation, User, SubtitleEntry};
use crate::parser::parse_subtitles;
use crate::i18n::I18n;
use crate::session;
use crate::translator::Segment;
use crate::translator;

pub struct AppState {
    pub pool: SqlitePool,
    pub tera: Tera,
    pub default_api_key: String,
    /// 会话签名密钥
    pub session_secret: Vec<u8>,
    /// 是否给 Cookie 加 Secure 属性（https 部署时开启）
    pub cookie_secure: bool,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/setlang/{lang}", get(setlang_handler))
        .route("/", get(home))
        .route("/login", get(login_page).post(login_handler))
        .route("/register", get(register_page).post(register_handler))
        .route("/logout", get(logout_handler))
        .route("/subtitle", get(subtitle_page).post(subtitle_handler))
        .route("/article", get(article_page).post(article_handler))
        .route("/result/{id}", get(result_page))
        .route("/edit/{id}", post(edit_handler))
        .route("/continue-translate/{id}", post(continue_translate_handler))
        .route("/annotate/{id}", post(annotate_handler))
        .route("/download/{id}/{format}", get(download_handler))
        .route("/history", get(history_page))
        .route("/settings", get(settings_page).post(settings_handler))
        .route("/api/status/{id}", get(status_handler))
}

type Jar = Cookies;

/// 没有显式 lang Cookie 时，按浏览器 `Accept-Language` 选语言，
/// 把结果合成到请求的 Cookie 头里，后续 handler 无需感知。
pub async fn detect_language(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let existing = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let has_lang = existing
        .split(';')
        .any(|c| c.trim_start().starts_with("lang="));

    if !has_lang {
        let accept = req
            .headers()
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok());
        let lang = crate::i18n::detect_lang(accept);
        let merged = if existing.is_empty() {
            format!("lang={}", lang)
        } else {
            format!("{}; lang={}", existing, lang)
        };
        if let Ok(value) = HeaderValue::from_str(&merged) {
            req.headers_mut().insert(header::COOKIE, value);
        }
    }

    next.run(req).await
}

fn get_user_id(jar: &Jar, state: &AppState) -> Option<i64> {
    let raw = jar.get(session::COOKIE_NAME)?.value();
    session::verify(&state.session_secret, raw)
}

async fn require_user(jar: &Jar, state: &AppState) -> Option<User> {
    let user_id = get_user_id(jar, state)?;
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
}

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

fn render_with(state: &Arc<AppState>, jar: Jar, template: &str, mut ctx: tera::Context) -> Response {
    let lang = current_lang(&jar);
    let i18n = I18n::new(&lang);
    t_ctx(&i18n, &mut ctx);
    ctx.insert("lang", &lang);
    let body = match state.tera.render(template, &ctx) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            Html(format!("<pre>Template error: {}</pre>", e)).into_response()
        }
    };
    (jar, body).into_response()
}

fn t_ctx(i18n: &I18n, ctx: &mut tera::Context) {
    let keys = [
        "nav_subtitle","nav_article","nav_history","nav_settings","nav_logout",
        "title_subtitle","title_article","title_history","title_settings","title_result",
        "title_index","title_login","title_register",
        "hero_subtitle","hero_article","hero_index","hero_index_sub",
        "h1_trans_result","h2_annot","h2_compare","h2_seg",
        "splitting_msg","task_failed","busy_msg","legacy_msg",
        "h2_upload_sub","h2_upload_art","h2_recent","h1_login","h1_register","h1_settings","h1_history",
        "label_source","label_target","label_file","label_file_art","label_paste","label_paste_art","drop_hint",
        "label_prompt","label_username","label_password","label_confirm","label_api_key",
        "label_annot_topic","label_annot_lang","label_annot_lang_en","label_annot_lang_ja",
        "btn_translate","btn_login","btn_register","btn_save","btn_save_edit","btn_cancel",
        "btn_annot_gen","btn_continue","btn_download_vtt","btn_download_srt","btn_download_txt",
        "btn_edit","btn_start_sub","btn_start_art","btn_dl_trans","btn_dl_source","btn_dl_table",
        "th_index","th_time","th_original","th_translation","th_note","th_filename","th_lang",
        "th_actions","th_view","divider_or","file","time",
        "auth_no_account","auth_has_account","auth_link","auth_link_login",
        "auth_error_invalid","auth_error_empty","auth_error_mismatch","auth_error_short",
        "auth_error_taken","auth_error_db","auth_register_success",
        "annot_failed","settings_saved","settings_current","settings_get_key",
        "no_translations","no_content","no_api_key","saving","saved","save_failed",
            ];
    for key in &keys {
        ctx.insert(*key, i18n.t(key));
    }
}

fn ok(jar: Jar, body: impl IntoResponse) -> Response {
    (jar, body).into_response()
}

async fn setlang_handler(jar: Jar, Path(lang): Path<String>) -> Response {
    // 只接受支持的语言代码，避免把任意字符串写进 Cookie
    if !crate::i18n::is_supported(&lang) {
        return ok(jar, Redirect::to("/"));
    }
    let cookie = Cookie::build(("lang", lang))
        .path("/")
        .max_age(time::Duration::days(365))
        .build();
    ok(jar.add(cookie), Redirect::to("/"))
}

async fn home(
    State(state): State<Arc<AppState>>,
    jar: Jar,
) -> Response {
    let user = require_user(&jar, &state).await;
    if user.is_none() {
        return ok(jar, Redirect::to("/login"));
    }
    let user = user.unwrap();

    let translations = sqlx::query_as::<_, Translation>(
        "SELECT * FROM translations WHERE user_id = ? ORDER BY created_at DESC LIMIT 20"
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    let mut ctx = tera::Context::new();
    ctx.insert("username", &user.username);
    ctx.insert("translations", &translations);

    render_with(&state, jar, "index.html", ctx)
}

async fn login_page(
    State(state): State<Arc<AppState>>,
    jar: Jar,
) -> Response {
    if require_user(&jar, &state).await.is_some() {
        return ok(jar, Redirect::to("/"));
    }
    let mut ctx = tera::Context::new();
    ctx.insert("error", "");
    render_with(&state, jar, "login.html", ctx)
}

async fn login_handler(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    Form(form): Form<LoginForm>,
) -> Response {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&form.username)
        .fetch_optional(&state.pool)
        .await;

    let user = match user {
        Ok(Some(u)) => u,
        _ => {
            let mut ctx = tera::Context::new();
            ctx.insert("error", "invalid");
            return render_with(&state, jar, "login.html", ctx);
        }
    };

    let parsed_hash = match PasswordHash::new(&user.password_hash) {
        Ok(h) => h,
        Err(_) => {
            let mut ctx = tera::Context::new();
            ctx.insert("error", "invalid");
            return render_with(&state, jar, "login.html", ctx);
        }
    };

    let valid = Argon2::default()
        .verify_password(form.password.as_bytes(), &parsed_hash)
        .is_ok();

    if !valid {
        let mut ctx = tera::Context::new();
        ctx.insert("error", "invalid");
        return render_with(&state, jar, "login.html", ctx);
    }

    let jar = jar.add(session::build_cookie(
        &state.session_secret,
        user.id,
        state.cookie_secure,
    ));
    ok(jar, Redirect::to("/"))
}

async fn register_page(
    State(state): State<Arc<AppState>>,
    jar: Jar,
) -> Response {
    if require_user(&jar, &state).await.is_some() {
        return ok(jar, Redirect::to("/"));
    }
    let mut ctx = tera::Context::new();
    ctx.insert("error", "");
    render_with(&state, jar, "register.html", ctx)
}

async fn register_handler(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    Form(form): Form<RegisterForm>,
) -> Response {
    let mut ctx = tera::Context::new();

    if form.username.trim().is_empty() || form.password.is_empty() {
        ctx.insert("error", "empty_fields");
        return render_with(&state, jar, "register.html", ctx);
    }
    if form.password != form.password_confirm {
        ctx.insert("error", "password_mismatch");
        return render_with(&state, jar, "register.html", ctx);
    }
    if form.password.len() < 4 {
        ctx.insert("error", "password_short");
        return render_with(&state, jar, "register.html", ctx);
    }

    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM users WHERE username = ?"
    )
    .bind(&form.username)
    .fetch_one(&state.pool)
    .await;

    match existing {
        Ok(count) if count > 0 => {
            ctx.insert("error", "username_taken");
            return render_with(&state, jar, "register.html", ctx);
        }
        Err(_) => {
            ctx.insert("error", "db_error");
            return render_with(&state, jar, "register.html", ctx);
        }
        _ => {}
    }

    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = match argon2.hash_password(form.password.as_bytes(), &salt) {
        Ok(h) => h.to_string(),
        Err(_) => {
            ctx.insert("error", "server_error");
            return render_with(&state, jar, "register.html", ctx);
        }
    };

    let _ = sqlx::query("INSERT INTO users (username, password_hash) VALUES (?, ?)")
        .bind(&form.username)
        .bind(&password_hash)
        .execute(&state.pool)
        .await;

    ok(jar, Redirect::to("/login?registered=1"))
}

async fn logout_handler(jar: Jar) -> Response {
    let jar = jar.remove(session::clear_cookie());
    ok(jar, Redirect::to("/login"))
}

async fn subtitle_page(
    State(state): State<Arc<AppState>>,
    jar: Jar,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u,
        None => return ok(jar, Redirect::to("/login")),
    };
    let mut ctx = tera::Context::new();
    ctx.insert("username", &user.username);
    ctx.insert("error", "");
    render_with(&state, jar, "subtitle.html", ctx)
}

fn get_api_key(user: &User, state: &AppState) -> String {
    let key = user.api_key.as_deref().unwrap_or("");
    if !key.is_empty() { return key.to_string(); }
    if !state.default_api_key.is_empty() { return state.default_api_key.clone(); }
    String::new()
}

async fn subtitle_handler(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    mut multipart: axum::extract::Multipart,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u,
        None => return ok(jar, Redirect::to("/login")),
    };

    let api_key = get_api_key(&user, &state);
    if api_key.is_empty() {
        return ok(jar, Redirect::to("/settings?error=no_api_key"));
    }

    let mut filename = String::from("subtitle.vtt");
    let mut file_content = String::new();
    let mut custom_prompt = String::new();
    let mut source_lang = String::from("en");
    let mut target_lang = String::from("zh");

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                filename = field.file_name().unwrap_or("subtitle.vtt").to_string();
                file_content = field.text().await.unwrap_or_default();
            }
            "text" => {
                let text = field.text().await.unwrap_or_default();
                if !text.is_empty() { file_content = text; }
            }
            "prompt" => { custom_prompt = field.text().await.unwrap_or_default(); }
            "source_lang" => { source_lang = field.text().await.unwrap_or_default(); }
            "target_lang" => { target_lang = field.text().await.unwrap_or_default(); }
            _ => {}
        }
    }

    if file_content.trim().is_empty() {
        let mut ctx = tera::Context::new();
        ctx.insert("username", &user.username);
        ctx.insert("error", "no_content");
        return render_with(&state, jar, "subtitle.html", ctx);
    }

    let entries = parse_subtitles(&file_content);
    let entry_count = entries.len();
    if entry_count == 0 {
        let mut ctx = tera::Context::new();
        ctx.insert("username", &user.username);
        ctx.insert("error", "未能从文件中解析出字幕条目，请检查格式");
        return render_with(&state, jar, "subtitle.html", ctx);
    }
    tracing::info!("subtitle: {} entries, {}→{}", entry_count, source_lang, target_lang);

    let entries_json = serde_json::to_string(&entries).unwrap_or_default();
    let save_result = sqlx::query(
        "INSERT INTO translations (user_id, filename, source_text, translated_text, source_lang, target_lang, status) \
         VALUES (?, ?, ?, ?, ?, ?, 'running')"
    )
    .bind(user.id).bind(&filename).bind(&file_content).bind(&entries_json)
    .bind(&source_lang).bind(&target_lang)
    .execute(&state.pool).await;

    let id = match save_result {
        Ok(r) => r.last_insert_rowid(),
        Err(_) => {
            let mut ctx = tera::Context::new();
            ctx.insert("username", &user.username);
            ctx.insert("error", "Failed to save translation");
            return render_with(&state, jar, "subtitle.html", ctx);
        }
    };

    // background translate
    let pool = state.pool.clone();
    let api_key_clone = api_key.clone();
    let sl = source_lang.clone();
    let tl = target_lang.clone();
    tokio::spawn(async move {
        let (entries, result) = translator::translate_subtitles_owned(
            api_key_clone, entries, sl, tl, custom_prompt, pool.clone(), id,
        ).await;
        match result {
            Ok(()) => {
                let filled = entries.iter().filter(|e| e.translation.as_deref().is_some_and(|t| !t.is_empty())).count();
                translator::set_status(&pool, id, "done").await;
                tracing::info!("bg translate done: {}/{} id={}", filled, entry_count, id);
            }
            Err(e) => {
                translator::set_status(&pool, id, "failed").await;
                tracing::error!("bg translate failed: {} id={}", e, id);
            }
        }
    });

    ok(jar, Redirect::to(&format!("/result/{}", id)))
}

async fn article_page(
    State(state): State<Arc<AppState>>,
    jar: Jar,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u,
        None => return ok(jar, Redirect::to("/login")),
    };
    let mut ctx = tera::Context::new();
    ctx.insert("username", &user.username);
    ctx.insert("error", "");
    render_with(&state, jar, "article.html", ctx)
}

async fn article_handler(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    mut multipart: axum::extract::Multipart,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u,
        None => return ok(jar, Redirect::to("/login")),
    };

    let api_key = get_api_key(&user, &state);
    if api_key.is_empty() {
        return ok(jar, Redirect::to("/settings?error=no_api_key"));
    }

    let mut filename = String::from("article.txt");
    let mut file_content = String::new();
    let mut custom_prompt = String::new();
    let mut source_lang = String::from("en");
    let mut target_lang = String::from("zh");

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                filename = field.file_name().unwrap_or("article.txt").to_string();
                file_content = field.text().await.unwrap_or_default();
            }
            "text" => {
                let text = field.text().await.unwrap_or_default();
                if !text.is_empty() { file_content = text; }
            }
            "prompt" => { custom_prompt = field.text().await.unwrap_or_default(); }
            "source_lang" => { source_lang = field.text().await.unwrap_or_default(); }
            "target_lang" => { target_lang = field.text().await.unwrap_or_default(); }
            _ => {}
        }
    }

    if file_content.trim().is_empty() {
        let mut ctx = tera::Context::new();
        ctx.insert("username", &user.username);
        ctx.insert("error", "no_content");
        return render_with(&state, jar, "article.html", ctx);
    }

    tracing::info!(
        "article: {}→{} {} chars, 开始拆句",
        source_lang, target_lang, file_content.chars().count()
    );

    // 先落一条"拆分中"的占位记录，请求立即返回，拆句在后台做
    let placeholder = ArticleSegment::serialize_all(&[]);
    let save_result = sqlx::query(
        "INSERT INTO translations (user_id, filename, source_text, translated_text, source_lang, target_lang, status) \
         VALUES (?, ?, ?, ?, ?, ?, 'splitting')"
    )
    .bind(user.id)
    .bind(&filename)
    .bind(&file_content)
    .bind(&placeholder)
    .bind(&source_lang)
    .bind(&target_lang)
    .execute(&state.pool)
    .await;

    let id = match save_result {
        Ok(r) => r.last_insert_rowid(),
        Err(e) => {
            tracing::error!("保存文章记录失败: {}", e);
            return article_error(&state, jar, &user.username, "保存失败");
        }
    };

    let pool = state.pool.clone();
    let key = api_key.clone();
    let sl = source_lang.clone();
    let tl = target_lang.clone();
    let prompt = custom_prompt.clone();
    let raw_text = file_content.clone();
    tokio::spawn(async move {
        // 1) 拆句 + 2) 校验
        let segments = match build_segments(&key, &raw_text, &sl).await {
            Ok(segs) => segs,
            Err(e) => {
                tracing::error!("文章拆句失败 id={}: {}", id, e);
                translator::set_status(&pool, id, "failed").await;
                return;
            }
        };
        tracing::info!("article: 拆成 {} 句 id={}", segments.len(), id);

        // 3) 落库片段，进入翻译阶段
        translator::replace_doc::<ArticleSegment>(&pool, id, &segments).await;
        translator::set_status(&pool, id, "running").await;

        let total = segments.len();
        let (segs, result) = translator::translate_article_owned(
            key, segments, sl, tl, prompt, pool.clone(), id,
        ).await;
        match result {
            Ok(()) => {
                let done = segs.iter().filter(|s| s.translation.is_some()).count();
                translator::set_status(&pool, id, "done").await;
                tracing::info!("bg article translate done: {}/{} id={}", done, total, id);
            }
            Err(e) => {
                translator::set_status(&pool, id, "failed").await;
                tracing::error!("bg article translate failed: {} id={}", e, id);
            }
        }
    });

    ok(jar, Redirect::to(&format!("/result/{}", id)))
}

/// 拆句 + 校验 + 组装片段（后台任务里跑）。
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

fn article_error(state: &Arc<AppState>, jar: Jar, username: &str, msg: &str) -> Response {
    let mut ctx = tera::Context::new();
    ctx.insert("username", username);
    ctx.insert("error", msg);
    render_with(state, jar, "article.html", ctx)
}

async fn result_page(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    Path(id): Path<i64>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u,
        None => return ok(jar, Redirect::to("/login")),
    };

    let translation = sqlx::query_as::<_, Translation>(
        "SELECT * FROM translations WHERE id = ? AND user_id = ?"
    )
    .bind(id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await;

    let translation = match translation {
        Ok(Some(t)) => t,
        _ => return ok(jar, Redirect::to("/")),
    };

    let mut ctx = tera::Context::new();
    ctx.insert("username", &user.username);
    ctx.insert("translation", &translation);
    ctx.insert("note_error", &params.contains_key("note_error"));
    ctx.insert("status", &translation.status);
    ctx.insert("busy", &params.contains_key("busy"));

    // 按 JSON 形态判定类型，不看文件扩展名
    match translator::parse_content(&translation.translated_text) {
        None => {
            // 旧版本数据（纯文本文章/旧数组），给出提示而不是 500
            ctx.insert("legacy", &true);
            ctx.insert("is_subtitle", &false);
            ctx.insert("is_article", &false);
        }
        Some(translator::ParsedContent::Subtitles(entries)) => {
            let (total, done) = progress_counts(&entries);
            ctx.insert("is_subtitle", &true);
            ctx.insert("is_article", &false);
            ctx.insert("total_entries", &total);
            ctx.insert("done_entries", &done);
            ctx.insert("unmatched", &(total - done));
            ctx.insert("unmatched_text", &format!("{} entries remaining", total - done));
            ctx.insert("entries", &entries);
        }
        Some(translator::ParsedContent::Article(segments)) => {
            let (total, done) = progress_counts(&segments);
            ctx.insert("is_subtitle", &false);
            ctx.insert("is_article", &true);
            ctx.insert("total_entries", &total);
            ctx.insert("done_entries", &done);
            ctx.insert("unmatched", &(total - done));
            ctx.insert("unmatched_text", &format!("{} sentences remaining", total - done));
            ctx.insert("entries", &segments);
        }
    }

    render_with(&state, jar, "result.html", ctx)
}

/// 统计"总条数 / 已翻译条数"，字幕和文章片段通用。
fn progress_counts<T: translator::Segment>(items: &[T]) -> (usize, usize) {
    let done = items
        .iter()
        .filter(|e| e.translation().is_some_and(|t| !t.is_empty()))
        .count();
    (items.len(), done)
}

async fn continue_translate_handler(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    Path(id): Path<i64>,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u, None => return ok(jar, Redirect::to("/login")),
    };
    let api_key = get_api_key(&user, &state);
    if api_key.is_empty() { return ok(jar, Redirect::to("/settings?error=no_api_key")); }

    let translation = sqlx::query_as::<_, Translation>(
        "SELECT * FROM translations WHERE id = ? AND user_id = ?"
    ).bind(id).bind(user.id).fetch_optional(&state.pool).await;

    let translation = match translation { Ok(Some(t)) => t, _ => return ok(jar, Redirect::to("/")), };

    if !translator::try_claim(&state.pool, id).await {
        return ok(jar, Redirect::to(&format!("/result/{}?busy=1", id)));
    }

    let sl = &translation.source_lang;
    let tl = &translation.target_lang;
    let result = match translator::parse_content(&translation.translated_text) {
        Some(translator::ParsedContent::Subtitles(mut entries)) => {
            translator::continue_translate(&api_key, &mut entries, sl, tl, "", &state.pool, id).await
        }
        Some(translator::ParsedContent::Article(mut segments)) => {
            translator::continue_translate_article(&api_key, &mut segments, sl, tl, "", &state.pool, id).await
        }
        None => Ok(()),
    };
    match result {
        Ok(()) => translator::set_status(&state.pool, id, "done").await,
        Err(e) => {
            tracing::error!("continue translate failed: {} id={}", e, id);
            translator::set_status(&state.pool, id, "failed").await;
        }
    }
    ok(jar, Redirect::to(&format!("/result/{}", id)))
}

async fn edit_handler(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    Path(id): Path<i64>,
    Json(data): Json<serde_json::Value>,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u, None => return ok(jar, Redirect::to("/login")),
    };

    let translation = sqlx::query_as::<_, Translation>(
        "SELECT * FROM translations WHERE id = ? AND user_id = ?"
    ).bind(id).bind(user.id).fetch_optional(&state.pool).await;

    let translation = match translation { Ok(Some(t)) => t, _ => return ok(jar, Redirect::to("/")), };

    let empty = serde_json::Map::new();
    let fields = data.as_object().unwrap_or(&empty);

    // 前端发的是 { "0": "新译文", "1": "" }，下标即数组位置；空串表示清空
    let edits: Vec<(usize, Option<String>)> = fields
        .iter()
        .filter_map(|(key, value)| {
            let idx = key.parse::<usize>().ok()?;
            let text = value.as_str()?.trim().to_string();
            Some((idx, if text.is_empty() { None } else { Some(text) }))
        })
        .collect();

    match translator::parse_content(&translation.translated_text) {
        Some(translator::ParsedContent::Subtitles(_)) => {
            translator::merge_edits::<SubtitleEntry>(&state.pool, id, &edits).await
        }
        Some(translator::ParsedContent::Article(_)) => {
            translator::merge_edits::<ArticleSegment>(&state.pool, id, &edits).await
        }
        None => {}
    }

    ok(jar, Redirect::to(&format!("/result/{}", id)))
}

async fn annotate_handler(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    Path(id): Path<i64>,
    Form(form): Form<AnnotateForm>,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u,
        None => return ok(jar, Redirect::to("/login")),
    };

    let api_key = get_api_key(&user, &state);
    if api_key.is_empty() {
        return ok(jar, Redirect::to("/settings?error=no_api_key"));
    }

    let translation = sqlx::query_as::<_, Translation>(
        "SELECT * FROM translations WHERE id = ? AND user_id = ?"
    )
    .bind(id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await;

    let translation = match translation {
        Ok(Some(t)) => t,
        _ => return ok(jar, Redirect::to("/")),
    };

    // 字幕和文章都支持注释
    let content = match translator::parse_content(&translation.translated_text) {
        Some(c) => c,
        None => return ok(jar, Redirect::to(&format!("/result/{}", id))),
    };

    let topic = form.topic.unwrap_or_default();
    let note_lang = form.note_lang.unwrap_or_else(|| String::from("zh"));

    if topic.is_empty() {
        return ok(jar, Redirect::to(&format!("/result/{}", id)));
    }

    if !translator::try_claim(&state.pool, id).await {
        return ok(jar, Redirect::to(&format!("/result/{}?busy=1", id)));
    }

    let lang_name = match note_lang.as_str() {
        "zh" => "Chinese",
        "ja" => "Japanese",
        _ => "English",
    };
    let prompt = format!("{} (respond in {})", topic, lang_name);

    let pool = state.pool.clone();
    let api_key_clone = api_key.clone();

    match content {
        translator::ParsedContent::Subtitles(entries) => {
            tokio::spawn(async move {
                let (_, result) = translator::generate_annotations_owned::<SubtitleEntry>(
                    api_key_clone, entries, prompt, pool.clone(), id,
                ).await;
                finish_job(&pool, id, result).await;
            });
        }
        translator::ParsedContent::Article(segments) => {
            tokio::spawn(async move {
                let (_, result) = translator::generate_annotations_owned::<ArticleSegment>(
                    api_key_clone, segments, prompt, pool.clone(), id,
                ).await;
                finish_job(&pool, id, result).await;
            });
        }
    }

    ok(jar, Redirect::to(&format!("/result/{}", id)))
}

/// 后台任务结束时落状态。
async fn finish_job(pool: &sqlx::SqlitePool, id: i64, result: anyhow::Result<()>) {
    match result {
        Ok(()) => translator::set_status(pool, id, "done").await,
        Err(e) => {
            tracing::error!("bg job failed: {} id={}", e, id);
            translator::set_status(pool, id, "failed").await;
        }
    }
}

async fn download_handler(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    Path((id, format)): Path<(i64, String)>,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u,
        None => return ok(jar, Redirect::to("/login")),
    };

    let translation = sqlx::query_as::<_, Translation>(
        "SELECT * FROM translations WHERE id = ? AND user_id = ?"
    )
    .bind(id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await;

    let translation = match translation {
        Ok(Some(t)) => t,
        _ => return ok(jar, Redirect::to("/")),
    };

    // 格式白名单按内容类型区分：format 会进入文件名，不能任意拼
    let format = format.to_ascii_lowercase();
    let base = strip_known_ext(&translation.filename);
    let base = if base.trim().is_empty() { "translation" } else { base };

    let (content, ext, mime, prefix) = match translator::parse_content(&translation.translated_text) {
        Some(translator::ParsedContent::Subtitles(entries)) => {
            if !matches!(format.as_str(), "vtt" | "srt" | "txt") {
                return ok(jar, (StatusCode::BAD_REQUEST, "subtitle supports vtt/srt/txt").into_response());
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
        Some(translator::ParsedContent::Article(segments)) => match format.as_str() {
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
            _ => {
                return ok(
                    jar,
                    (StatusCode::BAD_REQUEST, "article supports txt/src/csv").into_response(),
                )
            }
        },
        None => return ok(jar, (StatusCode::GONE, "record format is no longer supported").into_response()),
    };

    let download_name = format!("{}_{}.{}", prefix, base, ext);

    let mut response = Response::new(Body::from(content));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(mime),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_DISPOSITION, content_disposition(&download_name));
    ok(jar, response)
}

/// 去掉已知的字幕/文本扩展名（大小写不敏感，且只去一次）。
fn strip_known_ext(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    for ext in [".vtt", ".srt", ".txt"] {
        if lower.ends_with(ext) {
            return &name[..name.len() - ext.len()];
        }
    }
    name
}

/// 生成 Content-Disposition。
///
/// filename 来自用户上传的文件名，可能含中文、引号甚至控制字符：
/// - ASCII 回退名只保留安全字符，保证 header 一定合法（避免 `HeaderValue::from_str` 失败 panic）
/// - 原名按 RFC 5987 用 filename* 传递，中文名才能正确落盘
fn content_disposition(filename: &str) -> HeaderValue {
    let ascii = ascii_fallback_name(filename);
    let value = format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        ascii,
        pct_encode(filename)
    );
    HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

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
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

async fn history_page(
    State(state): State<Arc<AppState>>,
    jar: Jar,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u,
        None => return ok(jar, Redirect::to("/login")),
    };

    let translations = sqlx::query_as::<_, Translation>(
        "SELECT * FROM translations WHERE user_id = ? ORDER BY created_at DESC LIMIT 50"
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    let mut ctx = tera::Context::new();
    ctx.insert("username", &user.username);
    ctx.insert("translations", &translations);

    render_with(&state, jar, "history.html", ctx)
}

async fn settings_page(
    State(state): State<Arc<AppState>>,
    jar: Jar,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u,
        None => return ok(jar, Redirect::to("/login")),
    };

    let mut ctx = tera::Context::new();
    ctx.insert("username", &user.username);
    let api_key = get_api_key(&user, &state);
    ctx.insert("api_key", &api_key);
    ctx.insert("api_key_masked", &mask_key(&api_key));
    ctx.insert("updated", &false);

    render_with(&state, jar, "settings.html", ctx)
}

async fn settings_handler(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    Form(form): Form<ApiKeyForm>,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u,
        None => return ok(jar, Redirect::to("/login")),
    };

    let _ = sqlx::query("UPDATE users SET api_key = ? WHERE id = ?")
        .bind(&form.api_key)
        .bind(user.id)
        .execute(&state.pool)
        .await;

    ok(jar, Redirect::to("/settings?updated=1"))
}

async fn status_handler(
    State(state): State<Arc<AppState>>,
    jar: Jar,
    Path(id): Path<i64>,
) -> Response {
    let user = match require_user(&jar, &state).await {
        Some(u) => u,
        None => return (axum::http::StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "unauthorized"}))).into_response(),
    };

    let translation = sqlx::query_as::<_, Translation>(
        "SELECT * FROM translations WHERE id = ? AND user_id = ?"
    )
    .bind(id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await;

    match translation {
        Ok(Some(t)) => match translator::parse_content(&t.translated_text) {
            Some(translator::ParsedContent::Subtitles(entries)) => {
                let (total, done) = progress_counts(&entries);
                Json(serde_json::json!({ "total": total, "done": done, "status": t.status, "entries": entries })).into_response()
            }
            Some(translator::ParsedContent::Article(segments)) => {
                let (total, done) = progress_counts(&segments);
                Json(serde_json::json!({ "total": total, "done": done, "status": t.status, "entries": segments })).into_response()
            }
            None => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "unparsable", "status": t.status}))).into_response(),
        },
        _ => (axum::http::StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "not found"}))).into_response(),
    }
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
        // 以前 CRLF 会让 HeaderValue::from_str 失败，unwrap 直接 panic
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
        let s = content_disposition("translated_英文字幕.txt").to_str().unwrap().to_string();
        assert!(s.contains("filename*=UTF-8''"), "{s}");
        // 中文应被百分号编码，而不是原样塞进 header
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
        // 修复前这里会 panic（切在第 4 个字节中间）
        assert_eq!(mask_key("密钥密钥密钥密钥密钥"), "密钥密钥****密钥密钥");
    }
}
