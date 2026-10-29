# 前端改造交接说明

给接手前端重构的人（或 AI）看。**动手前先读 `README.md`，再读这份。**

---

## 1. 现状：这是服务端渲染，不是 SPA

```
浏览器 ──表单 POST──> Axum handler ──> 渲染 Tera 模板 ──303 重定向──> 浏览器
```

- 没有前端构建步骤，没有 npm，没有组件库
- 所有页面由 `templates/*.html` 继承 `base.html` 渲染
- 表单提交后服务端返回 **303 重定向**，不是 JSON
- 唯一的 JSON 接口是 `GET /api/status/{id}`（轮询进度用）
- 静态资源只有 `static/style.css` + `static/app.js`

**这意味着**：如果要做成 React/Vue SPA，需要先给后端加一层 JSON API（登录、上传、结果、编辑……），
工作量远大于"改页面"。本次重构建议保持 SSR。

---

## 2. 三条路线，代价递增

| 路线 | 改动 | 风险 |
|---|---|---|
| A. 只改模板 + CSS（推荐） | 重写 `templates/` 和 `style.css`，保持变量名和 DOM id | 低 |
| B. A + HTMX/Alpine | 加局部刷新、SSE 替代轮询 | 中，需新增 `/api/stream/{id}` |
| C. SPA 重写 | 后端加 JSON API + 前端构建链 | 高，等于重做产品 |

---

## 3. 后端契约（不可随意改的部分）

### 表单提交

所有表单都是 `POST` + `application/x-www-form-urlencoded` / `multipart`，
成功返回 `303` + `Location`，失败返回同一个 HTML 页面并在上下文里带 `error`。

| 表单 | 字段 | 成功跳转 |
|---|---|---|
| `/register` | `username`, `password`, `password_confirm` | `/login?registered=1` |
| `/login` | `username`, `password` | `/` |
| `/subtitle` | `file`(multipart) 或 `text`, `source_lang`, `target_lang`, `prompt` | `/result/{id}` |
| `/article` | 同上 | `/result/{id}`（毫秒级返回，翻译在后台） |
| `/settings` | `api_key` | `/settings?updated=1` |
| `/annotate/{id}` | `topic`, `note_lang` | `/result/{id}` |
| `/continue-translate/{id}` | 无 | `/result/{id}` 或 `/result/{id}?busy=1` |

### 编辑译文（唯一的业务 JSON POST）

```
POST /edit/{id}
Content-Type: application/json
{"0": "第一句新译文", "1": "", "5": "..."}
```

**key 是数组下标（0-based），不是页面上显示的序号。** 空字符串表示清空该条（导出回退原文）。

> 字幕的显示序号 `e.index` 来自原文件编号，可能不连续、甚至重复；
> 而下标恒等于 `loop.index0`。两者不能混用。

### 进度轮询

```
GET /api/status/{id}
{"total": 246, "done": 137, "status": "running", "entries": [...]}
```

`status` 取值：`splitting` / `running` / `done` / `failed`。

---

## 4. 模板上下文变量

变量有两个来源：

1. **全量注入**：`routes.rs::t_ctx()` 每页都会注入 i18n key 列表里的所有键
2. **逐页注入**：各 handler 里 `ctx.insert(...)`（共 57 处）

**新增/改名变量必须两处同步**，否则 Tera 渲染直接失败，页面显示
`Template error: Failed to render 'xxx.html'`——而且这个报错**不告诉你缺哪个变量**。

结果页特有的变量：

| 变量 | 说明 |
|---|---|
| `translation` | 记录本身（id、filename、created_at…） |
| `entries` | 字幕条目数组或文章片段数组（字段名刻意保持一致：`index/text/translation/note`） |
| `is_subtitle` / `is_article` | 内容类型 |
| `total_entries` / `done_entries` / `unmatched` / `unmatched_text` | 进度 |
| `status` / `busy` / `legacy` | 任务状态 / 并发互斥提示 / 旧格式提示 |
| `note_error` | 注释失败标记 |

---

## 5. DOM 契约（改模板必须同步改 JS）

`result.html` 里的轮询脚本硬依赖这些 id / class：

| 选择器 | 用途 |
|---|---|
| `#trans-{e.index}` | 译文单元格，轮询时按 index 定位填充 |
| `#note-{e.index}` | 注释单元格 |
| `#progress-bar` / `#progress-text` | 进度条宽度与文字 |
| `#poll-status` | "Translating… 12/246" 状态文字 |
| `#continue-form` | 全部完成后隐藏 |
| `#edit-bar` | 编辑工具条（display 切换） |
| `.trans-view` / `.trans-edit` | 查看态 / 编辑态，两者按顺序一一对应 |
| `<textarea name="{loop.index0}">` | **提交时的 key 就是它** |

`app.js` 依赖：

| 选择器 | 用途 |
|---|---|
| `[data-drop]` 容器 | 拖拽上传区，内部需有 `input[type=file]` 和 `[data-filename]` |
| `form` 内 `button[type=submit]` | 提交后加 `.is-loading` 转圈 |
| `window.toast(msg, type)` | 全局提示，模板里可直接调用 |

**如果换掉 DOM 结构，必须同步重写 `result.html` 底部的轮询脚本，否则进度不更新、编辑保存失效。**

---

## 6. 多语言：四语，改一处要改三处

支持 `zh` / `zh-TW` / `zh-HK` / `en`。

新增一个界面文案要做三件事：

1. `src/i18n.rs` 的 `get_translation()` 加四元组
   `["简体", "繁體(台)", "繁體(港)", "English"]`
2. `src/routes.rs` 的 `t_ctx()` key 列表加上该 key
3. 模板里用 `{{ key }}`

漏掉第 2 步 = 渲染失败。

---

## 7. 数据形态（前端只读，不要自己造第三种）

`translated_text` 里只允许两种结构：

```jsonc
// 字幕
[{"index":1,"start":"00:00:14.844","end":"00:00:16.802","text":"...","translation":"...","note":null,"settings":"align:center line:81%"}]

// 文章
{"kind":"article","segments":[{"index":1,"text":"...","translation":"...","note":"..."}]}
```

判定逻辑在 `translator::parse_content()`，**不看文件扩展名**。加新结构必须同步改这里，
否则页面会显示"旧版本格式"。

`status` 字段是后端任务锁的依据（`try_claim` 原子更新）。**前端不要乐观地改它**，
也不要假设"点了按钮就一定开始跑"——并发时会返回 `?busy=1`。

---

## 8. 已经修好的安全点，别改回去

| 项 | 现状 |
|---|---|
| 会话 Cookie | `{user_id}.{HMAC-SHA256}`，**不是裸 user_id**（早期版本可伪造） |
| Cookie 属性 | `HttpOnly` + `SameSite=Lax` + `Path=/` + `Max-Age=7d`（https 下加 `Secure`） |
| 下载文件名 | RFC 5987 编码 + ASCII 回退，避免 header 注入 |
| 模板输出 | Tera 自动转义；JS 一律用 `textContent`，没有 `innerHTML` |
| SQL | 全部参数绑定 |

如果前端改成 `fetch` 直接发 JSON 变更请求，注意 **`SameSite=Lax` 只挡跨站 POST**，
别把状态变更做成 GET。

---

## 9. 本地怎么验证

### 起服务

```bash
cp .env.example .env    # 填 DEFAULT_API_KEY
cargo run               # 必须在项目根目录
```

### 没有真 Key 也能测：用 mock

```bash
DEEPSEEK_API_URL=http://127.0.0.1:3199/chat/completions cargo run
```

mock 只需实现 `POST`，读 `messages[1].content` 里的 `{"items":[{"id":0,"text":"..."}]}`，
返回 `{"choices":[{"message":{"content":"{\"pairs\":[{\"id\":0,\"translation\":\"...\"}]}"}}]}`。

### 改模板必须重启

Tera 在启动时一次性加载模板（`Tera::new("templates/**/*.html")`）。
改 `.html` 后不重启看不到效果；改 `static/` 下的文件则刷新即可。

### 检查清单（每次改完跑一遍）

```bash
cargo test          # 74 个单测
cargo clippy        # 应为 0 warning
```

再手动过一遍：

- [ ] 登录 / 注册页在**未登录**状态能切换语言（下拉框在 nav 里，不在 `{% if username %}` 内）
- [ ] 字幕上传 → 结果页有表格 + 进度条，轮询能推进
- [ ] 文章上传 → 先看到"正在拆分" → 自动刷新出表格
- [ ] 编辑模式打开 → 改一条 → 保存 → Toast 出现，单元格更新
- [ ] 清空一条译文 → 导出时回退为原文
- [ ] 字幕下载 vtt/srt/txt；文章下载 txt/src/csv
- [ ] 文章 CSV 用 Excel 打开中文不乱码（BOM）
- [ ] 四种语言各扫一眼，没有露出原始 key（如 `nav_subtitle`）

---

## 10. 视觉资产

设计令牌都在 `static/style.css` 的 `:root`：

```css
--bg / --surface / --surface-hover / --border
--text / --text-dim / --text-mute
--accent / --accent-2 / --accent-3 / --gradient
--radius / --radius-sm / --radius-lg
--shadow / --glow
--font / --mono
```

换主题只改这些；组件样式全部引用 token，没有散落的硬编码色值。

已实现的动效：View Transitions 页面过渡、卡片 hover 上浮、进度条流光、
Toast 滑入、按钮 loading、`prefers-reduced-motion` 全局降级。

---

## 11. 部署（改完要上线时）

```bash
cargo build --release
scp target/release/automated-translation user@host:~/automated-translation/automated-translation.new
tar czf - templates static | ssh user@host 'tar xzf - -C ~/automated-translation'
ssh user@host 'cd ~/automated-translation && chmod 644 static/* templates/* \
  && mv -f automated-translation.new automated-translation \
  && sudo systemctl restart automated-translation'
```

详细步骤、systemd 单元、nginx 配置见 `README.md` 的「部署」章节。

---

## 12. 最后三条建议

1. **先备份再动**。当前线上版本是稳定可用的，重构期间随时可能要回滚。
2. **别一次改完再测**。模板变量缺失的报错不含变量名，一次改太多会很难定位——
   建议按页面逐个改，改完立刻刷新验证。
3. **保持 `entries` 的字段名**（`index` / `text` / `translation` / `note`）。
   字幕和文章用同一套字段名是刻意的设计，让表格模板和轮询脚本能共用一份逻辑。
