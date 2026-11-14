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
├── routes.rs        # /api JSON 接口 + 静态回退 + Origin 校验
├── session.rs       # 会话签名（HMAC-SHA256）与 Cookie 属性
└── i18n.rs          # 四语文案表（前端字典由它生成）
frontend/            # Vue 3 SPA（源码）
static/              # 前端构建产物（npm run build 生成）
dev/mock-deepseek.mjs# 无 API Key 时的本地 mock 模型
.env                 # DATABASE_URL, DEFAULT_API_KEY
```

后端**不再有模板**：所有页面由 Vue Router 管理，Rust 只在找不到静态文件时回退 index.html。

### 本地目录布局

工作目录本身就是仓库根，`src/`、`frontend/`、`static/`、`dev/` 直接在这一层（2026-11 整理后的结构）：

```
~/automated-translation\      # = 仓库根，git 跟踪的是这里
├── src/ frontend/ static/ dev/   # 见上
├── demo/                         # 重构前的设计 demo（Vue + mock，独立 npm 工程，不进仓库）
├── docs/shots/                   # 页面截图（验收留档）
├── archive/                      # 旧版本与备份，不参与构建
│   ├── legacy-tera-ssr-2026-07/  # SPA 重构之前的 Tera SSR 源码（根 src/static/templates 的旧副本）
│   ├── backups/                  # pre-spa 的 tar.gz、整目录 zip、旧二进制
│   └── manifest-before-2026-11-14.txt  # 整理前的完整文件清单
└── data-spa.db                   # MOCK=1 跑 run-demo.sh 用的演示库
```

- `demo/` 仍可独立跑：`cd demo && npm run dev`；它的 `npm run gen:i18n` 读 `../src/i18n.rs`。
- `archive/` 里的东西**不要引用**，只是留档；恢复整棵旧目录用 `archive/backups/automated-translation-2026-11-14.zip`（解出来是整理前的 `automated-translation/` 双层结构）。
- 根目录的 `英文字幕.vtt` / `bright_ideas_for_a_bright_future_en.vtt` 是示例素材，格式同上。

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
| POST | `/api/auth/register` | register（自动登录） | 公开 |
| POST | `/api/auth/login` | login | 公开 |
| POST | `/api/auth/logout` | logout | 公开 |
| GET | `/api/me` | me | 需登录 |
| GET/POST | `/api/translations` | list_translations / create_translation | 需登录 |
| GET | `/api/translations/{id}` | get_translation（含 entries） | 需登录 |
| GET | `/api/translations/{id}/status` | get_status（轮询） | 需登录 |
| POST | `/api/translations/{id}/edits` | put_edits（JSON 下标） | 需登录 |
| POST | `/api/translations/{id}/annotate` | annotate | 需登录 |
| POST | `/api/translations/{id}/continue` | continue_translation | 需登录 |
| GET/PUT | `/api/settings` | settings_get / settings_put | 需登录 |
| POST | `/api/lang` | set_lang（写 Cookie） | 公开 |
| GET | `/download/{id}/{format}` | download | 需登录 |
| GET | 其余路径 | ServeDir → 回退 index.html | 公开 |

完整字段与错误码见 `FRONTEND-CONTRACT.md`。

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

- 文案表仍然是 Rust 的 `i18n.rs`（四元组），**前端字典由它生成**：
  `cd frontend && npm run gen:i18n` → `frontend/src/i18n/dict.ts`
- 语言检测在前端 `src/i18n/index.ts`：localStorage（手动选择）> `Accept-Language` > 简体
- 切换语言时前端同时 `POST /api/lang` 写 `lang` Cookie，后端导出 CSV 时用它决定表头
- 港式用词与台湾不同：帳戶/帳號、用戶/使用者、註釋/註解、檔案名稱/檔名
- 后端 `I18n` 现在只服务于 CSV 表头与 `/api/lang` 的白名单校验

### 新增翻译文本步骤
1. `i18n.rs` 的 `get_translation()` 添加四元组
2. `cd frontend && npm run gen:i18n`
3. 页面里 `t('key')`

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

Vue 3 + TypeScript + Vite，源码在 `frontend/`，构建产物在 `static/`。

- 入口：`src/main.ts` → `App.vue` → `layouts/AppLayout.vue` + `router.ts`
- 页面：`views/`（Home / Login / Register / Upload / Result / History / Settings）
- 组件：`components/`（AppNav、ResultTable、ProgressBar、DropZone、ToastHost…）
- 组合式：`composables/`（useAuth、usePolling、useToast）
- 样式：`styles/tokens.css`（调色板 + 结构令牌，浅色磨砂）+ `styles/base.css`
- 接口层：`api/types.ts`（契约）、`api/http.ts`（fetch）、`api/mock.ts`（离线调试）

改视觉只动 `tokens.css`；组件里不允许出现硬编码颜色。

---

## 开发模式

### 新增 API
1. `routes::router()` 的 `api` 路由表里加 `.route("/xxx", get(handler))`
2. 签名：`async fn xxx(State(state): State<Arc<AppState>>, jar: Jar, ...) -> Response`
3. 认证：`let user = match require_user(&jar, &state).await { Ok(u) => u, Err(e) => return e.into_response() };`
4. 成功返回 `Json(json!({...}))`，失败用 `api_error(StatusCode::…, "code")`
5. 前端：在 `api/types.ts` 加类型、`api/http.ts` 加方法、`api/mock.ts` 同步一份（跑测试用）

### 前端开发
```bash
cd frontend
npm run dev        # 5173，/api 已代理到 3000
npm run typecheck
npm test
npm run build      # 写进 ../static
```

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
