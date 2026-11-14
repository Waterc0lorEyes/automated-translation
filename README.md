# AI 字幕 & 文章翻译

一个自托管的 AI 翻译站点：上传 VTT/SRT 字幕或文章，用 DeepSeek 翻译成目标语言，
支持逐句对照、人工编辑、AI 注释和多格式导出。Rust + Axum 后端 + Vue 3 SPA 前端，单二进制部署。

线上地址：<https://tomatocat.art>

> 本仓库是**后端**（Rust/Axum，只提供 `/api` 与静态托管）。前端在独立仓库
> [**automated-translation-web**](https://github.com/Waterc0lorEyes/automated-translation-web)，
> 构建产物需要放进本仓库的 `static/`（见下「构建前端」）。

---

## 目录

- [功能特性](#功能特性)
- [技术栈](#技术栈)
- [快速开始](#快速开始)
- [配置项](#配置项)
- [目录结构](#目录结构)
- [核心设计](#核心设计)
- [HTTP 接口](#http-接口)
- [导出格式](#导出格式)
- [多语言](#多语言)
- [测试](#测试)
- [部署](#部署)
- [生产部署与运维手册](DEPLOY.md)
- [运维](#运维)
- [已知限制](#已知限制)

---

## 功能特性

### 字幕翻译

- 支持 `.vtt` / `.srt`，自动识别格式（也支持直接粘贴）
- 保留原字幕的定位信息（`align:center line:81%`）和断行，导出的字幕在播放器里位置不变
- 翻译在后台跑，页面实时刷新进度，可随时离开
- 未翻完的条目可以一键「补译」，不会重复翻已完成的
- 逐条人工编辑，可清空（清空后导出回退原文）

### 文章翻译

- 上传 TXT 或粘贴，**先由 AI 拆句、再由 AI 复核拆分结果**（合并碎句、切开长句、删空、补漏）
- 拆句在后台进行，上传接口毫秒级返回，页面显示「正在拆分」并自动刷新
- 之后按字幕同样的逻辑逐句翻译，结果按句对照展示
- 拆分失败会如实报错，不会偷偷降级成整篇一次性翻译

### AI 注释

- 字幕和文章都支持，输入主题（如「俚语、文化背景、法律术语」）即可
- 可指定注释语言（中文 / English / 日本語）
- 注释追加式合并，同一记录重复生成不会覆盖已有注释

### 其他

- 中英四语言界面：简体 / 繁体（台）/ 繁体（港）/ English，**跟随浏览器语言自动切换**
- 账号体系：注册、登录、每用户独立 API Key
- 历史记录、下载、深色霓虹主题、拖拽上传、Toast 提示

---

## 技术栈

| 层 | 选型 |
|---|---|
| 语言 | Rust 2021（开发时 1.99.1） |
| Web | Axum 0.8 + Tower HTTP |
| 页面 | Vue Router 管理的前端路由，后端只做 SPA 回退 |
| 数据库 | SQLite（sqlx，编译期不做查询校验） |
| 密码 | Argon2 + password-hash |
| 会话 | HMAC-SHA256 签名 Cookie（hmac + sha2） |
| 外部服务 | DeepSeek Chat API（reqwest + rustls） |
| 前端 | Vue 3 + TypeScript + Vite 7 + Vue Router（SPA） |

---

## 快速开始

### 1. 准备

- Rust 工具链（`rustup`）
- 一个 DeepSeek API Key：<https://platform.deepseek.com/>

### 2. 配置

```bash
cp .env.example .env
```

编辑 `.env`，至少填上：

```ini
DEFAULT_API_KEY=sk-你的key
SESSION_SECRET=$(openssl rand -hex 32)
```

### 3. 构建前端

前端在独立仓库里，把它 clone 到本仓库的 `frontend/` 下（`.gitignore` 已忽略这个目录）：

```bash
git clone https://github.com/Waterc0lorEyes/automated-translation-web.git frontend
cd frontend
npm ci
STATIC_DIR=../static npm run build   # 产物写进 ../static（index.html + assets/）
```

### 4. 运行

```bash
cargo run
# 访问 http://localhost:3000
```

> **注意**：`static/` 是按相对路径加载的，必须在项目根目录启动。

#### 没有 API Key 时

```bash
node dev/mock-deepseek.mjs &          # 假模型，监听 3199
DEEPSEEK_API_URL=http://127.0.0.1:3199/chat/completions \
DEFAULT_API_KEY=mock-key cargo run
```

或者一条命令（release 构建 + 假模型 + 演示库，不消耗 API 额度）：

```bash
cargo build --release
MOCK=1 ./dev/run-demo.sh              # http://0.0.0.0:18080
```

不带 `MOCK=1` 时直接用 `.env` 的配置（真 Key、真库）。

#### 前端开发模式

```bash
cd frontend && npm install && npm run dev   # 5173，/api 已代理到 3000
```

（前提是前端仓库已经 clone 到 `frontend/`，见上面「构建前端」。）

### 5. 构建发布版

```bash
cargo build --release
# 产物：target/release/automated-translation
```

部署时把 **二进制 + static/ + .env** 放到同一目录即可（不再需要 templates/），
数据库文件会自动创建。

---

## 配置项

全部通过环境变量（`.env` 或 systemd 环境）配置：

| 变量 | 默认值 | 说明 |
|---|---|---|
| `DATABASE_URL` | `sqlite://data.db` | SQLite 路径，相对 WorkingDirectory |
| `HOST` | `0.0.0.0` | 监听地址 |
| `PORT` | `3000` | 监听端口 |
| `DEFAULT_API_KEY` | 空 | 全局兜底 Key；用户未配置自己的 Key 时使用 |
| `LOG_FILE` | 空 | 日志文件路径；为空则输出到 stdout（journald） |
| `SESSION_SECRET` | 随机生成 | 会话签名密钥。**不设置时每次启动随机生成，重启后所有登录失效** |
| `COOKIE_SECURE` | `false` | https 部署必须设为 `true`，否则 Cookie 不带 Secure |
| `DEEPSEEK_API_URL` | 官方端点 | 走代理/兼容网关/本地 mock 时改这里 |
| `TRANSLATE_CONCURRENCY` | `3` | 同时发出的翻译请求数，范围 1–8 |

---

## 目录结构

```
automated-translation/
├── src/                      # Rust 后端
│   ├── main.rs               # 启动、路由装配、中间件挂载
│   ├── config.rs             # 环境变量解析
│   ├── db.rs                 # 连接池、建表、启动时重置中断任务
│   ├── models.rs             # User / Translation / SubtitleEntry / ArticleSegment
│   ├── parser.rs             # VTT/SRT 解析（保留定位信息与断行）
│   ├── translator.rs         # DeepSeek 客户端、拆句、翻译、注释、导出格式化
│   ├── routes.rs             # /api JSON 接口 + 静态回退 + Origin 校验
│   ├── session.rs            # 会话签名与 Cookie 属性
│   └── i18n.rs               # 四语文案表（前端字典由它生成）
├── frontend/                 # Vue 3 SPA（独立仓库，clone 到这里；.gitignore 已忽略）
│   ├── src/api/              # 类型 + http 适配层（+ 可选 mock）
│   ├── src/views/            # 首页 / 上传 / 结果 / 历史 / 设置 / 登录 / 注册
│   ├── src/components/       # 导航、表格、进度条、拖拽区、Toast…
│   ├── src/composables/      # useAuth / usePolling / useToast
│   ├── src/i18n/             # dict.ts 由 Rust 生成，禁止手改
│   └── src/styles/           # 设计令牌 + 基础层（浅色磨砂）
├── static/                   # 前端构建产物，不入库（构建方式见上文）
├── dev/mock-deepseek.mjs     # 无 Key 时的本地 mock 模型
├── .env.example
├── FRONTEND-CONTRACT.md      # 架构与 API 契约（改接口先看这里）
└── SKILL.md                  # 给 AI 助手看的开发速查
```

---

## 核心设计

### 数据模型

```sql
users(id, username UNIQUE, password_hash, api_key, created_at)

translations(id, user_id, filename, source_text, translated_text,
             source_lang, target_lang, status, created_at)
```

`translated_text` 只有两种合法形态，由 `translator::parse_content()` 按 JSON 结构判定
（**不看文件扩展名**）：

| 形态 | 结构 | 来源 |
|---|---|---|
| 字幕 | 裸数组 `[{index,start,end,text,translation,note,settings}]` | `/subtitle` |
| 文章 | `{"kind":"article","segments":[{index,text,translation,note}]}` | `/article` |

解析不出来时页面提示「该记录是旧版本格式」，下载返回 410——不做旧数据兼容。

`status` 状态机：

```
splitting ──> running ──> done
    │            │
    └────────────┴──────> failed
```

进程启动时会把残留的 `splitting`/`running` 重置为 `failed`，避免记录被永久占锁。

### 文章翻译流水线

```
POST /article
   │  落占位记录 segments=[] status=splitting
   ├──────────────> 立即返回 303 /result/{id}
   ▼
[后台] AI 拆句（4000 字符分块） → {"sentences":[...]}
   ▼
[后台] AI 校验（120 句/批）   → 合并 / 切分 / 删除 / 补齐
   ▼
写入 segments，status=running
   ▼
[后台] 分批翻译（id 协议）→ 每完成一批增量落库
   ▼
status=done（异常则 failed）
```

前端在 `splitting` 阶段显示等待文案并轮询，状态一变就 `location.reload()`。

### 翻译协议：JSON Output + id 回填

每次请求发送：

```json
{"items":[{"id":0,"text":"..."},{"id":1,"text":"..."}]}
```

模型返回：

```json
{"pairs":[{"id":0,"translation":"..."}]}
```

要点：

- 请求带 `response_format: {"type":"json_object"}`；服务端不认时自动去掉重试一次
- `id` 就是该条在数组里的下标，**回填不做任何字符串匹配**
- 只接受本批次发出过的 id，越界/未知 id 丢弃并计数
- 同一句台词重复出现时各 id 独立回填
- 输出被 `max_tokens` 截断时，扫描已完整的 `{...}` 对象捞回前面成功的部分

分批策略：起始 100 条，每轮结束若仍有空缺就把批量减半（`100 → 50 → 25 → 12 → 6`），
最多 5 轮；不再回显原文，所以 100 条不会撞上 8192 token 上限。

### 并发与一致性

| 问题 | 方案 |
|---|---|
| 多个批次请求耗时 | `buffer_unordered` 并发，默认 3，`TRANSLATE_CONCURRENCY` 可调 |
| 并发写入互相覆盖 | 所有落库都是「读最新 → 改 → 写回」，并由全局锁串行化 |
| 翻译/补译/注释重复触发 | 原子 `UPDATE ... WHERE status NOT IN ('running','splitting')` 抢占，抢不到跳 `?busy=1` |
| 写库丢失 | 只写本次产生的增量（`merge_filled` / `merge_edits` / `merge_notes`），不整份覆盖 |

> 早期的整份覆盖写法实测会在「翻译进行中点注释」时丢掉全部译文；现在注释和译文互不覆盖。

### 会话与安全

- Cookie 内容是 `{user_id}.{HMAC-SHA256(user_id)}`，签名不符一律当未登录
- Cookie 属性：`HttpOnly` + `SameSite=Lax` + `Path=/` + `Max-Age=7d`（+ https 下的 `Secure`）
- 密码用 Argon2 默认参数
- SQL 全部参数绑定；模板自动转义；前端动态内容用 `textContent`

---

## HTTP 接口

页面路由（`/`、`/login`、`/upload/*`、`/result/*` 等）由 Vue Router 接管，
后端只负责回退 `static/index.html`。真正的接口如下，完整字段见
[FRONTEND-CONTRACT.md](FRONTEND-CONTRACT.md)：

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/auth/register` | 注册（成功后直接登录） |
| POST | `/api/auth/login` | 登录 |
| POST | `/api/auth/logout` | 退出 |
| GET | `/api/me` | 当前用户 + Key 掩码 |
| GET | `/api/translations` | 历史列表（最近 100 条，不含 entries） |
| GET | `/api/translations/{id}` | 单条记录（含 entries） |
| GET | `/api/translations/{id}/status` | 进度轮询 `total/done/status/entries` |
| POST | `/api/translations` | 上传（multipart）并启动翻译 |
| POST | `/api/translations/{id}/edits` | 编辑译文（`{"0":"译文","1":""}`，空串=清空） |
| POST | `/api/translations/{id}/annotate` | 生成注释（`topic`、`note_lang`） |
| POST | `/api/translations/{id}/continue` | 补译未完成条目 |
| GET/PUT | `/api/settings` | 读取 / 保存个人 API Key |
| POST | `/api/lang` | 切换语言，写 `lang` Cookie |
| GET | `/download/{id}/{format}` | 导出（浏览器直接跳转） |

所有写接口都会校验 `Origin`，跨站请求返回 403。

记录 ID 对外是 **UUIDv7**（如 `01a18708-9621-76b7-…`），数据库内部仍用自增主键；
旧的数字 ID 不再可用（返回 404）。

---

## 导出格式

### 字幕

| format | 内容 | 扩展名 |
|---|---|---|
| `vtt` | WebVTT，保留 `align/line` 定位 | `.vtt` |
| `srt` | SRT，时间轴逗号格式 | `.srt` |
| `txt` | 每句一行译文 | `.txt` |

### 文章

| format | 内容 | 扩展名 |
|---|---|---|
| `txt` | 译文分句，一句一行 | `.txt` |
| `src` | 原文分句，一句一行 | `.txt` |
| `csv` | 原文 / 译文 / 注释 对照表 | `.csv` |

CSV 针对 Excel 做了处理：UTF-8 BOM、CRLF 换行、字段含逗号/引号/换行时按 RFC 4180 转义，
表头跟随界面语言。

其余格式一律返回 400。

---

## 多语言

四种语言：`zh` / `zh-TW`（台）/ `zh-HK`（港）/ `en`。

语言来源优先级（前端 `src/i18n/index.ts`）：

1. 用户手动选择（localStorage，同时 `POST /api/lang` 写 Cookie）
2. 浏览器 `Accept-Language`（`zh-HK`/`zh-MO` → 香港，`zh-TW`/`zh-Hant*` → 台湾，
   `zh-CN`/`zh` → 简体，`en*` → English）
3. 默认简体

后端只在导出 CSV 时用 `lang` Cookie 决定表头语言。

港台用词做了区分（帳戶/帳號、用戶/使用者、註釋/註解、檔案名稱/檔名）。

**新增文案**：

1. 在 `src/i18n.rs` 的 `get_translation()` 里加一条四元组
   `["简体", "繁體(台)", "繁體(港)", "English"]`
2. `cd frontend && npm run gen:i18n` 重新生成 `src/i18n/dict.ts`
3. 页面里用 `t('key')`

前端字典是生成物，**不要手改**；CI 里可以跑 `npm run gen:i18n && git diff --exit-code`。

---

## 测试

```bash
cargo test        # 70 个单元测试
cargo clippy      # 0 warning

cd frontend
npm run typecheck # 0 error
npm test          # 10 个冒烟测试（happy-dom 挂载整个 SPA）
```

覆盖的重点：

- 字幕解析/导出（定位信息、断行、SRT 逗号）
- 会话签名（伪造、篡改、错误密钥、畸形 token）
- JSON 提取（对象信封、裸数组、代码围栏、**截断恢复**、字符串型 id）
- 并发落库（注释不覆盖译文、编辑不被晚到的翻译覆盖、清空生效）
- CSV 导出（BOM、表头、逗号/引号/换行转义）
- 语言检测（繁简港、q 值排序、边界输入）
- Header 生成（中文名、控制字符、CRLF 注入）

---

## 部署

> 完整手册（构建、systemd、nginx、HTTPS、备份回滚、故障排查、上线检查清单）见
> **[DEPLOY.md](DEPLOY.md)**。下面是最短路径。

### 服务器环境

Ubuntu 24.04 / x86_64 / glibc 2.39，systemd 托管，nginx 反向代理到本机端口。

### 首次部署

```bash
# 本地构建：先前端，再后端
cd frontend && npm ci && STATIC_DIR=../static npm run build && cd ..
cargo build --release

# 上传（二进制 + 前端产物，保留服务器上的 .env 和 data.db）
scp target/release/automated-translation user@host:~/automated-translation/
tar czf - static | ssh user@host 'tar xzf - -C ~/automated-translation'
```

服务器上：

```bash
cd ~/automated-translation
chmod -R a+r static
```

`.env` 生产配置示例：

```ini
DATABASE_URL=sqlite://data.db
HOST=0.0.0.0
PORT=3001
DEFAULT_API_KEY=sk-xxx
LOG_FILE=app.log
SESSION_SECRET=<openssl rand -hex 32>
COOKIE_SECURE=true
```

### systemd 单元

`/etc/systemd/system/automated-translation.service`：

```ini
[Unit]
Description=Automated Translation
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=<user>
Group=<user>
WorkingDirectory=/home/<user>/automated-translation
ExecStart=/home/<user>/automated-translation/automated-translation
Restart=always
RestartSec=3
KillSignal=SIGTERM

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now automated-translation
```

### nginx

```nginx
location / {
    proxy_pass http://127.0.0.1:3001;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
}
```

证书用 acme.sh / certbot 签发。

### 更新部署

```bash
# 备份 → 停服 → 上传 → 启服
ssh host 'cd ~ && tar czf ~/backups/app-$(date +%F-%H%M).tar.gz automated-translation/ && sudo systemctl stop automated-translation'
scp target/release/automated-translation host:~/automated-translation/automated-translation.new
tar czf - static | ssh host 'tar xzf - -C ~/automated-translation'
ssh host 'cd ~/automated-translation && mv -f automated-translation.new automated-translation && sudo systemctl start automated-translation'
```

### 回滚

```bash
sudo systemctl stop automated-translation
cd ~ && tar xzf ~/backups/app-<时间戳>.tar.gz
sudo systemctl start automated-translation
```

数据库结构变更由代码自动完成（`CREATE TABLE IF NOT EXISTS` + `ALTER TABLE ADD COLUMN`），
无需手工迁移；但**旧格式数据不做兼容**，升级前建议先备份 `data.db`。

---

## 运维

```bash
# 状态 / 日志
systemctl status automated-translation
journalctl -u automated-translation -f
tail -f ~/automated-translation/app.log      # 若配置了 LOG_FILE

# 重启
sudo systemctl restart automated-translation

# 备份数据库（服务运行中也能安全复制）
cp ~/automated-translation/data.db ~/backups/data.db-$(date +%F-%H%M)
```

---

## 已知限制

- **同一记录同时只能跑一个任务**（翻译/补译/注释互斥），第二个请求会提示「有任务正在运行」
- **编辑清空 = 删除译文**，导出时回退为原文
- 拆句阶段若进程崩溃，重启后该记录变 `failed`，需要重新上传（原文仍在 `source_text` 里）
- 历史记录无分页、无删除入口
- SQLite 单文件、未开 WAL，适合单实例；不适合多实例共享
- `app.log` 没有轮转，长期运行建议改用 journald 或配 logrotate
- 文章拆句时长文按 4000 字符分块，超长文章拆句阶段耗时较长
- CSV 对照表按句输出，不保留原文的段落结构

---

## 许可

内部项目，未指定开源许可。
