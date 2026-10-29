---
name: automated-translation
description: AI 字幕文章翻译网站，Rust/Axum 技术栈。用于处理字幕翻译(VTT/SRT)、文章翻译、注释生成、多语言界面等功能的开发和修改。工作目录 ~/automated-translation，部署环境 Ubuntu 127.0.0.1。
---

# AI 字幕 & 文章翻译

Rust + Axum，DeepSeek API，SQLite。14 个路由，中英双语。

## 快速启动

```bash
cd ~/automated-translation
DATABASE_URL=sqlite://data.db ./target/debug/automated-translation
# 访问: http://127.0.0.1:3000
```

编译: `cargo build`
日志: `tail -f /tmp/app1.log`

---

## 项目结构

```
src/
├── main.rs          # 入口，Axum 服务启动
├── config.rs        # 环境配置 (DATABASE_URL, HOST, PORT, SESSION_SECRET, COOKIE_SECURE)
├── db.rs            # SQLite 连接池，建表迁移 (users, translations)
├── models.rs        # 数据结构：User, Translation, SubtitleEntry, 各种表单
├── parser.rs        # VTT/SRT 字幕解析器（保留 align/line 定位信息）
├── translator.rs    # DeepSeek API 客户端 — 翻译、注释、补译、导出格式化
├── routes.rs        # 全部 HTTP 处理器 (认证/字幕/文章/结果/编辑/注释/下载/历史/设置)
├── session.rs       # 会话签名（HMAC-SHA256）与 Cookie 属性
├── i18n.rs          # 中英文翻译表，语言切换
templates/           # Tera HTML 模板 (8个)
static/style.css     # 样式
.env                # DATABASE_URL, DEFAULT_API_KEY
```

---

## 数据库结构

SQLite，启动时自动迁移。

### users 表
| 字段 | 类型 | 说明 |
|------|------|------|
| id | INTEGER PK | 自增 |
| username | TEXT UNIQUE | |
| password_hash | TEXT | argon2 |
| api_key | TEXT | 用户独立 Key |
| status | TEXT | queued/splitting/running/done/failed，默认 done |
| created_at | DATETIME | |

### translations 表
| 字段 | 类型 | 说明 |
|------|------|------|
| id | INTEGER PK | |
| user_id | INTEGER FK | |
| filename | TEXT | |
| source_text | TEXT | 原文 |
| translated_text | TEXT | 字幕数组或文章片段信封（见下）|
| source_lang | TEXT | DEFAULT 'en' |
| target_lang | TEXT | DEFAULT 'zh' |
| status | TEXT | queued/splitting/running/done/failed，默认 done |
| created_at | DATETIME | |

---

## 核心类型

```rust
struct User { id, username, password_hash, api_key, created_at }
struct Translation { id, user_id, filename, source_text, translated_text, source_lang, target_lang, status, created_at }
struct ArticleSegment { index, text, translation: Option<String> }  // 文章按句拆分后的片段
struct SubtitleEntry { index, start, end, text, translation: Option<String>, note: Option<String>, settings: Option<String> }
struct RegisterForm { username, password, password_confirm }
struct LoginForm { username, password }
struct ApiKeyForm { api_key }
struct AnnotateForm { topic: Option<String>, note_lang: Option<String> }
```

---

## API 路由

| 方法 | 路径 | 处理器 | 认证 |
|------|------|--------|------|
| GET | `/` | home | 需登录 |
| GET/POST | `/login` | login_page / login_handler | 公开 |
| GET/POST | `/register` | register_page / register_handler | 公开 |
| GET | `/logout` | logout_handler | 无限制 |
| GET/POST | `/subtitle` | subtitle_page / subtitle_handler | 需登录 |
| GET/POST | `/article` | article_page / article_handler | 需登录 |
| GET | `/result/{id}` | result_page | 需登录 |
| POST | `/edit/{id}` | edit_handler (JSON body) | 需登录 |
| POST | `/continue-translate/{id}` | continue_translate_handler | 需登录 |
| POST | `/annotate/{id}` | annotate_handler (AnnotateForm) | 需登录 |
| GET | `/download/{id}/{format}` | download_handler | 需登录 |
| GET | `/history` | history_page | 需登录 |
| GET/POST | `/settings` | settings_page / settings_handler | 需登录 |
| GET | `/setlang/{lang}` | setlang_handler (设语言 Cookie) | 无限制 |

---

## 翻译模块 (translator.rs)

### 函数签名

```rust
// 字幕
translate_subtitles(api_key, entries, source_lang, target_lang, custom_prompt, pool, id) -> Result<()>
continue_translate(api_key, entries, source_lang, target_lang, custom_prompt, pool, id) -> Result<()>
generate_annotations(api_key, entries, annotation_prompt, pool, id) -> Result<()>
format_subtitles(entries, format) -> String

// 文章
split_article(api_key, text, source_lang) -> Result<Vec<String>>
verify_sentences(api_key, sentences, source_lang) -> Vec<String>
translate_article(api_key, segments, source_lang, target_lang, custom_prompt, pool, id) -> Result<()>
continue_translate_article(api_key, segments, ...) -> Result<()>
format_segments(segments) -> String

// 通用
parse_content(raw) -> ParsedContent            // Subtitles / Article / Plain
translate_text(api_key, text, ...) -> String   // 整篇一次，仅拆句失败时降级用
call_deepseek_json(api_key, body) -> Result<Value>
extract_items(content) -> Vec<RawItem>
extract_strings(content) -> Vec<String>
```

字幕和文章共用 `translate_segments()` / `fill_segments()`：靠 `Segment` trait 抽象"原文 + 译文"，
分批、重试、id 回填、落库只有一份实现。新类型只需实现 trait 即可复用。

### 文章翻译流程（2026-10 起）

1. `POST /article` 收到文本，**立即落一条占位记录**（`segments: []`、`status='splitting'`）并返回
2. 后台任务：**AI 拆句** `split_article()`（长文按 4000 字符分块）→ **AI 校验** `verify_sentences()`
3. 拆成 `Vec<ArticleSegment>` 后 `replace_doc()` 写入，状态改 `running`
4. 后台 `translate_article()` 分批翻译；成功置 `done`，异常置 `failed`
5. 结果页在 `splitting` 时显示等待文案并轮询，状态一变就 `location.reload()`；
   `failed` 且无内容时显示失败提示
6. 拆句失败**不降级**，直接置 `failed`

### 落库策略（防并发覆盖）

所有写入都走"读最新 → 改 → 写回"，并由全局 `save_lock()` 串行化：

- `merge_filled()` 只补空位，不覆盖已有译文
- `merge_edits()` 覆盖/清空（空字符串 = 清空，导出时回退原文）
- `merge_notes()` 追加注释，已存在的不重复
- `replace_doc()` 整体替换（仅拆句完成时用一次）

这样"翻译进行中点注释/编辑"不会再用旧快照覆盖新数据（旧实现会整份覆盖，实测丢过 40 条译文）。

### 任务互斥

- 发起翻译/补译/注释前先 `try_claim(pool, id)`：单条原子 UPDATE，把 `status` 置 `running`，
  只有原本不在 `running`/`splitting` 的记录才能成功
- 抢占失败 → 重定向到 `/result/{id}?busy=1`，页面提示"有任务正在运行"，**不会重复消耗 API**
- 任务结束置 `done` 或 `failed`
- 进程启动时把所有残留的 `running`/`splitting` 重置为 `failed`（进程已重启，那些任务已经死了），
  避免记录被永久占锁

`translated_text` 只有两种形态（`parse_content()` 判定，**不看文件名后缀**）：

| 形态 | 结构 | 产生方式 |
|------|------|----------|
| 字幕 | 裸数组 `[{index,start,end,text,translation,note,settings}]` | /subtitle |
| 文章片段 | `{"kind":"article","segments":[{index,text,translation}]}` | /article |

`parse_content()` 返回 `Option`，两种都不匹配时按 500 处理——**不兼容旧数据**：
旧的纯文本文章、旧的裸数组文章片段都会被拒。文章拆句失败时直接返回错误页让用户重试，
不再降级成"整篇一次翻译"。

### 协议：JSON Output + id 回填

1. 请求体带 `response_format: {"type":"json_object"}`（`api_body(..., json_mode=true)`）
2. 送出去的是 `{"items":[{"id":0,"text":"..."}]}`，**id 就是该条在 `entries` 里的下标**
3. 模型只回 `{"pairs":[{"id":0,"translation":"..."}]}`（注释是 `{"notes":[{"id","note"}]}`）
4. `apply_translations()` 按 id 直接写入，**不再回显原文、不再做字符串匹配**

这样做的收益：省输出 token；模型改写原文也不会匹配失败；同一句台词重复出现时各 id 独立回填。

注意：
- 自定义提示词会被拼上 id 协议说明（否则结果无法回填）
- 只接受本批次发出过的 id，越界/未知 id 丢弃并计数
- 空 translation 跳过，已有译文不覆盖
- 模型输出被 `max_tokens` 截断时，`extract_items` 会扫描已完整的 `{...}` 对象，捞回前面成功的部分
- 服务端不认 `response_format`（400）时，自动去掉该字段重试一次

### DeepSeek API
- 端点: 默认 `https://api.deepseek.com/chat/completions`，可用 `DEEPSEEK_API_URL` 覆盖（代理/兼容网关/mock）
- 模型: `deepseek-chat`, 温度 0.3
- 超时: 120s；批量: `[100, 50, 25, 12, 6]`（逐轮减半重试，最多 5 轮）
- 并发: 同一轮的多个 chunk **并发发出**（`buffer_unordered`，默认 3，`TRANSLATE_CONCURRENCY` 可调 1-8），
  但结果回到主任务里串行写入 `entries` 并落库 —— 写库永远单线程，不会互相覆盖
- 拆句用 `buffered`（保序并发），因为句子顺序必须保持原文顺序
- 补译/注释固定 100 条一批

---

## 解析模块

```rust
parse_subtitles(content) -> Vec<SubtitleEntry>  // 自动识别 VTT/SRT
```
VTT: `-->` 行创建条目，多行文本拼接。
SRT: 数字编号，时间戳 `,` 转 `.`。

---

## 多语言系统

支持四种语言：`zh`（简体）/ `zh-TW`（台湾繁体）/ `zh-HK`（香港繁体）/ `en`。

- `I18n::new(code)` 内部映射成 `Lang`，`I18n::t(key)` 返回对应语言文案
- 文案表是 `["简体", "繁體(台)", "繁體(港)", "English"][idx]` 四元组
- **语言来源优先级**：`lang` Cookie（用户主动选择）> 浏览器 `Accept-Language` > 默认 `zh`
- 浏览器检测在 `routes::detect_language` 中间件里做：没有 lang Cookie 时按
  `Accept-Language` 的 q 值挑一个，合成到请求 Cookie 头里，handler 无需感知
  - `zh-HK` / `zh-MO` / `zh-Hant-HK` → 香港繁体（港优先于泛繁体）
  - `zh-TW` / `zh-Hant*` → 台湾繁体；`zh-CN` / `zh-Hans` / `zh` → 简体
  - 不支持的语言（如 `fr`）跳过，最终落回默认简体
- `GET /setlang/{lang}` 只接受 `SUPPORTED` 里的代码，写 Cookie（有效期一年）；
  非法值直接跳回首页不写 Cookie
- 导航栏用的是四选一下拉（未登录页面也可见），`<html lang>` 输出当前语言代码
- 港式用词与台湾不同：帳戶/帳號、用戶/使用者、註釋/註解、檔案名稱/檔名
- `t_ctx(&i18n, &mut ctx)` 注入所有 key

### 新增翻译文本步骤
1. `i18n.rs` 的 `get_translation()` 添加 key: `"key" => &["中文", "English"][idx]`
2. `routes.rs` 的 `t_ctx()` 的 keys 数组加入该 key
3. 模板中用 `{{ key }}`

---

## 认证

- Cookie 会话: `axum_extra::CookieJar`
- `session` Cookie 存 **`{user_id}.{HMAC-SHA256 签名}`**，不是裸 user_id；
  校验走 `session::verify()`，签名不符一律当未登录（手工改 `session=1` 无效）
- 签名密钥来自 `SESSION_SECRET`，未配置时启动随机生成（重启后登录失效）
- Cookie 属性：`HttpOnly` + `SameSite=Lax` + `Path=/` + `Max-Age=7d`；
  https 部署时把 `COOKIE_SECURE=true` 打开才加 `Secure`
- `require_user(jar, state)` → `Option<User>`（注意第二个参数是 AppState，不是 pool）
- argon2 + OsRng 密码哈希
- `get_api_key(user, state)` 返回用户 Key，无则用默认 Key
- 退出登录用 `session::clear_cookie()`（path 必须与下发时一致才能删掉）

---

## 前端

静态资源只有两个文件，无构建步骤：

- `static/style.css`：设计令牌（CSS 变量）集中在 `:root`，深色霓虹主题。
  改配色只动 token；`--accent` / `--accent-2` / `--accent-3` 组成渐变。
  含 View Transitions、`prefers-reduced-motion` 降级、响应式断点 820/560px。
- `static/app.js`：Toast（`window.toast(msg, type)`）、拖拽上传（`[data-drop]` 容器 +
  内部 `input[type=file]`）、表单提交按钮 loading 态。

改主题色只需替换 `:root` 里的 accent 三个变量；换风格（如亮色）改 `--bg/--surface/--text` 即可。

## 模板

全部继承 `base.html`。使用 `{{ i18n_key }}` 实现多语言。

| 模板 | 用途 |
|------|------|
| base.html | 导航栏、语言切换、布局 |
| index.html | 首页卡片入口 + 最近翻译 |
| login.html | 登录表单 |
| register.html | 注册表单 |
| subtitle.html | 字幕上传：语种 + 文件 + 提示词 |
| article.html | 文章上传 |
| result.html | 结果对照表格 + 注释 + 编辑 + 进度条 + 下载 |
| history.html | 翻译历史 |
| settings.html | API Key 配置 |

---

## 开发模式

### 新增处理器
1. `router()` 添加 `.route(...)`
2. 签名: `async fn xxx(State(state): State<Arc<AppState>>, jar: Jar, ...) -> Response`
3. 开头认证: `let user = require_user(&jar, &state.pool).await;`
4. 渲染: `render_with(&state, jar, "模板.html", ctx)`
5. 重定向: `ok(jar, Redirect::to("/path"))`

### 上传到服务器
始终用 `ssh-mcp_upload` 逐个文件上传，不要上传目录。改完本地文件确认逻辑正确再上传。

---

## 环境

- 开发机: Ubuntu 26.04
- Rust: 1.97.1
- DB: SQLite `~/automated-translation/data.db`
- 日志: `/tmp/app1.log`
- 默认 API Key: `.env` 中 `DEFAULT_API_KEY`
- 会话密钥: `.env` 中 `SESSION_SECRET`（`openssl rand -hex 32` 生成；
  不配则每次启动随机生成，重启后需重新登录）
- https 部署: 设 `COOKIE_SECURE=true`，否则 Cookie 不带 Secure
- 工作目录: `~/automated-translation`
