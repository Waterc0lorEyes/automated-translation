# 架构与接口契约（SPA 版）

> 这份文件取代了原来的「前端改造交接说明」。现在前后端已经分离成两个工程：
> Rust 只提供 JSON API 与静态文件，页面逻辑全在 `frontend/`。

---

## 1. 架构

```
浏览器
  │  GET /            → static/index.html（Vite 构建产物）
  │  GET /assets/*    → static/assets/*（带 hash，长缓存）
  │  GET /upload/...  → 回退 index.html，交给 Vue Router
  │
  ├─ fetch /api/*    → Axum JSON 接口（Cookie 认证）
  └─ GET /download/… → 直接下载文件（浏览器跳转，不走 fetch）

Rust (Axum)
  ├─ /api/*          JSON，见第 3 节
  ├─ /download/{id}/{format}
  └─ 其余 GET        ServeDir("static") → 找不到就回退 index.html
```

- 前端：Vue 3 + TypeScript + Vite 7 + Vue Router 4，无 UI 组件库。
- 后端：Axum 0.8 + SQLite，业务逻辑（拆句/翻译/注释/导出）保持原样。
- 认证：签名 Cookie（`{user_id}.{HMAC-SHA256}`），前端 `credentials: 'same-origin'`。

---

## 2. 目录

```
自动化翻译/
├── src/                 # Rust
│   ├── routes.rs        # 全部 /api 处理器 + SPA 回退 + Origin 校验
│   ├── translator.rs    # DeepSeek 调用、分批、落库、导出格式化（未改动）
│   ├── session.rs       # Cookie 签名
│   ├── i18n.rs          # 后端文案（CSV 表头、语言白名单）
│   └── ...
├── frontend/            # Vue SPA
│   ├── src/api/         # 类型 + http 适配层（+ 可选 mock）
│   ├── src/views/       # 页面
│   ├── src/components/  # 组件
│   ├── src/composables/ # useAuth / usePolling / useToast
│   ├── src/i18n/        # dict.ts 由 Rust 生成，不要手改
│   ├── src/styles/      # 令牌 + 基础层
│   └── scripts/gen-i18n.py
├── static/              # 前端构建产物（index.html + assets/），由 vite 生成
└── dev/mock-deepseek.mjs # 无 Key 时的本地 mock
```

---

## 3. API

所有接口都在 `/api` 下，JSON 请求体，返回 JSON；`204` 表示无内容。
未登录统一返回 `401 {"error":"unauthorized"}`。

| 方法 | 路径 | 请求 | 成功响应 |
|---|---|---|---|
| POST | `/api/auth/register` | `{username, password, password_confirm}` | `200 {user}` + Set-Cookie |
| POST | `/api/auth/login` | `{username, password}` | `200 {user}` + Set-Cookie |
| POST | `/api/auth/logout` | — | `204` |
| GET | `/api/me` | — | `200 {user}` / `401` |
| GET | `/api/translations` | — | `200 {items:[summary]}` |
| GET | `/api/translations/{id}` | — | `200 record`（含 `entries`） |
| GET | `/api/translations/{id}/status` | — | `200 {total,done,status,entries}` |
| POST | `/api/translations` | multipart | `202 {id}` |
| POST | `/api/translations/{id}/edits` | `{"0":"译文","1":""}` | `204` |
| POST | `/api/translations/{id}/annotate` | `{topic, note_lang}` | `202` / `409 busy` |
| POST | `/api/translations/{id}/continue` | — | `202` / `409 busy` |
| GET | `/api/settings` | — | `200 {has_key, masked_key}` |
| PUT | `/api/settings` | `{api_key}` | `204` |
| POST | `/api/lang` | `{lang}` | `204` + Set-Cookie |
| GET | `/download/{id}/{format}` | — | 文件流（`Content-Disposition`） |

### 上传（multipart）

| 字段 | 说明 |
|---|---|
| `kind` | `subtitle` 或 `article` |
| `file` | 文件（带 filename，可选） |
| `text` | 粘贴内容；与 file 同时存在时 text 优先 |
| `source_lang` / `target_lang` | 语言代码，默认 `en` → `zh` |
| `prompt` | 自定义提示词，可空 |

返回 `202 {id}`，翻译在后台进行；`article` 会先进入 `splitting`，拆分完成后转 `running`。

### 记录结构

```jsonc
// summary（列表）
{ "id":1, "filename":"a.vtt", "kind":"subtitle|article|legacy",
  "source_lang":"en", "target_lang":"zh", "status":"running",
  "created_at":"2026-10-29 17:18", "total":246, "done":137 }

// record = summary + entries
"entries": [
  {"index":1,"start":"00:00:14.844","end":"00:00:16.804",
   "text":"...","translation":"...","note":null,"settings":"align:center line:81%"}
]
```

`kind=legacy` 表示 `translated_text` 不是已知结构（旧数据），此时 `entries` 为空、下载返回 `410`。

### 错误码

| error | HTTP | 含义 |
|---|---|---|
| `unauthorized` | 401 | 未登录 / Cookie 失效 |
| `invalid_credentials` | 401 | 用户名或密码错误 |
| `empty_fields` / `password_mismatch` / `password_short` | 400 | 注册校验失败 |
| `username_taken` | 409 | 用户名已存在 |
| `no_content` | 422 | 文件/粘贴内容为空 |
| `parse_failed` | 422 | 字幕解析不出条目 |
| `no_api_key` | 400 | 用户没配 Key 且服务端没有兜底 Key |
| `busy` | 409 | 该记录已有任务在跑 |
| `legacy_format` | 409 / 410 | 旧数据结构 |
| `unsupported_format` | 400 | 下载格式不在白名单 |
| `bad_origin` | 403 | 跨站写请求被拒 |

---

## 4. 认证与安全

- Cookie：`HttpOnly` + `SameSite=Lax` + `Path=/` + `Max-Age=7d`（https 下加 `Secure`）。
- 写操作额外做 **Origin 校验**：`Origin` 与 `Host` 的**主机名**必须一致，否则 `403 bad_origin`。
  只比主机名、不比端口，因为 nginx 默认的 `proxy_set_header Host $host` 会丢掉端口；
  严格要求端口一致会把正常请求误杀（这个坑实测过）。
  `SameSite=Lax` 已挡住跨站 POST，这层是防止未来改 Cookie 策略时裸奔。
- 密码 Argon2；SQL 全参数绑定；下载文件名 RFC 5987 编码。
- 前端不用 `innerHTML` 渲染译文（Vue 默认转义）。

---

## 5. 前端约定

### 类型即契约

`frontend/src/api/types.ts` 与 `src/models.rs`、`routes.rs` 的 JSON 一一对应。
后端改字段 → `npm run typecheck` 立刻报错。

### 轮询

`usePolling.ts`：只在 `splitting|running` 时轮询，`done|failed` 立刻停；
组件卸载即清理；串行请求，不会叠加。

### i18n

- 文案唯一事实来源仍然是 Rust 的 `src/i18n.rs`。
- `npm run gen:i18n` 生成 `frontend/src/i18n/dict.ts`，**不要手改**。
- 新增文案：改 `i18n.rs` → 跑脚本 → 模板里 `t('key')`。
- 建议 CI 校验：`npm run gen:i18n && git diff --exit-code src/i18n/dict.ts`。
- 切换语言会同时写 `lang` Cookie（`POST /api/lang`），保证 CSV 表头也跟着变。

### 主题

`src/styles/tokens.css` 两层：调色板 + 结构令牌。组件只用结构令牌。
当前只有浅色磨砂一套主题。

---

## 6. 开发

```bash
# 终端 1：需要 API Key 才能翻译；没有就起 mock
node dev/mock-deepseek.mjs
DEEPSEEK_API_URL=http://127.0.0.1:3199/chat/completions cargo run

# 终端 2：Vite（/api、/download 已代理到 3000）
cd frontend && npm install && npm run dev   # http://localhost:5173
```

只改 Rust 时不需要前端 dev server：`cargo run` 直接服务 `static/` 里的构建产物。

---

## 7. 构建与部署

```bash
cd frontend && npm ci && npm run build   # 产物直接写进 ../static
cd .. && cargo build --release
```

部署只需要 **二进制 + static/ + .env**（不再需要 templates/）。

```bash
scp target/release/automated-translation host:~/automated-translation/
tar czf - static | ssh host 'tar xzf - -C ~/automated-translation'
ssh host 'cd ~/automated-translation && mv -f automated-translation.new automated-translation && sudo systemctl restart automated-translation'
```

`.env` 与之前一致；前台改成 SPA 后**不需要新增配置**。

---

## 8. 测试

```bash
cargo test            # 70 passed（Rust：解析/翻译协议/导出/会话/文件名）
cargo clippy          # 0 warning
cd frontend
npm run typecheck     # 0 error
npm test              # 10 passed（happy-dom 挂载整个 SPA）
```
