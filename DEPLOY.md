# 生产部署与运维手册

自动化翻译（AI 字幕 / 文章翻译）· Vue 3 SPA + Axum + SQLite

> 本文是上线用的完整手册：构建 → 配置 → systemd → nginx/HTTPS → 验证 → 备份/回滚 → 排错。
> 接口契约见 [FRONTEND-CONTRACT.md](FRONTEND-CONTRACT.md)，日常开发见 [README.md](README.md)。

---

## 0. 一页速查

```bash
# 构建（在开发机 / CI 上）
cd frontend && npm ci && npm run build && cd ..
cargo build --release

# 上传（二进制 + 前端产物，保留服务器上的 .env 和 data.db）
scp target/release/automated-translation user@host:~/automated-translation/automated-translation.new
tar czf - static | ssh user@host 'tar xzf - -C ~/automated-translation'

# 切换（服务器上）
cd ~/automated-translation
chmod -R a+r static && chmod 600 .env
mv -f automated-translation.new automated-translation
sudo systemctl restart automated-translation

# 验证
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:3001/        # 200
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:3001/api/me   # 401
```

---

## 1. 项目概览

```
浏览器
  │  GET /  /login  /result/<uuid> …   → Vue Router（SPA）
  │  fetch /api/*                       → JSON 接口（Cookie 认证）
  │  GET  /download/<uuid>/<format>     → 文件下载
  ▼
Axum (Rust)
  ├─ /api/*           12 个接口（14 个方法）
  ├─ /download/*      导出 vtt / srt / txt / src / csv
  ├─ ServeDir("static") → index.html + assets/
  └─ SQLite (data.db)
        │
        └─ DeepSeek Chat API（翻译 / 拆句 / 注释）
```

| 层 | 选型 |
|---|---|
| 后端 | Rust 2021、Axum 0.8、sqlx(SQLite)、tokio |
| 前端 | Vue 3.5 + TypeScript + Vite 7 + Vue Router 4 |
| 认证 | HMAC-SHA256 签名 Cookie（HttpOnly / SameSite=Lax / 7 天） |
| 外部依赖 | DeepSeek Chat API（唯一） |

**运行时只需要**：二进制 + `static/` + `.env`。**不需要 Node、不需要模板目录**。

---

## 2. 产物与目录

```
automated-translation/
├── automated-translation     # release 二进制（约 14 MB）
├── static/                   # 前端构建产物（约 240 KB）
│   ├── index.html
│   └── assets/*.js|*.css     # 带 hash，可长期缓存
├── data.db                   # SQLite 数据库（自动创建）
├── data.db-shm / -wal        # 出现这两个说明用了 WAL（当前未启用）
├── .env                      # 生产配置，权限 600
└── app.log                   # 仅当配置了 LOG_FILE
```

源码结构：

```
src/          Rust：routes.rs（API）/ translator.rs（DeepSeek）/ parser.rs / session.rs / i18n.rs
frontend/     Vue SPA：api / views / components / composables / styles
dev/          本地 mock 模型、启动脚本（不参与生产）
```

---

## 3. 部署前准备

| 项 | 要求 |
|---|---|
| 构建机 | Rust（stable）+ Node ≥ 20（仅构建前端用） |
| 服务器 | x86_64 Linux（Ubuntu 24.04 验证过）、systemd |
| 端口 | 一个本机端口（示例用 `3001`），由 nginx 反代 |
| 域名/证书 | 可选；用 HTTPS 时必须有证书 |

**先备份线上数据库**（本次升级会改表结构）：

```bash
ssh user@host 'cp ~/automated-translation/data.db ~/automated-translation/data.db.bak-$(date +%F-%H%M)'
```

---

## 4. 构建

```bash
git clone <repo> && cd automated-translation

# 1) 前端：产物直接写进 ../static（vite.config.ts 里配好的）
cd frontend
npm ci
npm run build          # 约 2 秒
cd ..

# 2) 后端
cargo build --release  # 首次约 2~3 分钟
ls -lh target/release/automated-translation static/index.html
```

> `frontend/package-lock.json` 已入库，用 `npm ci` 而不是 `npm install`，保证版本可复现。

---

## 5. 环境变量（`.env`）

放在**二进制同级目录**，权限 `600`，属主为服务运行用户。

| 变量 | 必填 | 默认 | 说明 |
|---|---|---|---|
| `DATABASE_URL` | 是 | `sqlite://data.db` | 相对 `WorkingDirectory`；路径改了就要跟着改 |
| `HOST` | 否 | `0.0.0.0` | 建议 `127.0.0.1`，只让 nginx 访问 |
| `PORT` | 否 | `3000` | 示例用 `3001` |
| `SESSION_SECRET` | **是** | 随机生成 | `openssl rand -hex 32`；不设则重启后全部掉线 |
| `COOKIE_SECURE` | https 必开 | `false` | https 下设 `true`；http 下开会导致登录不上 |
| `DEFAULT_API_KEY` | 否 | 空 | 全局兜底 Key。**留空 = 用户必须自己配 Key**，公网建议留空 |
| `LOG_FILE` | 否 | 空 | 空则输出到 stdout（journald）；设了要自己配轮转 |
| `DEEPSEEK_API_URL` | 否 | 官方端点 | 走代理/网关/mock 时改 |
| `TRANSLATE_CONCURRENCY` | 否 | `3` | 1–8，翻译请求并发数 |

生产 `.env` 示例：

```ini
DATABASE_URL=sqlite://data.db
HOST=127.0.0.1
PORT=3001
SESSION_SECRET=<openssl rand -hex 32 生成>
COOKIE_SECURE=true
#DEFAULT_API_KEY=sk-xxx
#LOG_FILE=app.log
TRANSLATE_CONCURRENCY=3
```

> 优先级：**环境变量 > `.env`**（`dotenvy` 不覆盖已存在的变量）。
> 用 systemd 时如果在 unit 里写了 `Environment=`，它会盖掉 `.env` 的同名项。

---

## 6. 首次部署

### 6.1 目录与权限

```bash
sudo useradd -r -m -d /home/automated-translation -s /usr/sbin/nologin automated-translation || true
sudo mkdir -p /home/automated-translation
sudo chown -R automated-translation:automated-translation /home/automated-translation

# 上传二进制 + static/
sudo -u automated-translation cp automated-translation /home/automated-translation/
sudo -u automated-translation tar czf - static | sudo -u automated-translation tar xzf - -C /home/automated-translation

# 配置
sudo -u automated-translation tee /home/automated-translation/.env >/dev/null <<'EOF'
DATABASE_URL=sqlite://data.db
HOST=127.0.0.1
PORT=3001
SESSION_SECRET=请替换成-openssl-rand-hex-32
COOKIE_SECURE=true
EOF
sudo chmod 600 /home/automated-translation/.env
sudo chmod -R a+rX /home/automated-translation/static
```

### 6.2 systemd

`/etc/systemd/system/automated-translation.service`：

```ini
[Unit]
Description=Automated Translation (Vue SPA + Axum)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=automated-translation
Group=automated-translation
WorkingDirectory=/home/automated-translation
ExecStart=/home/automated-translation/automated-translation
Restart=always
RestartSec=3
KillSignal=SIGTERM

# 基础加固
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/home/automated-translation

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now automated-translation
sudo systemctl status automated-translation --no-pager
sudo journalctl -u automated-translation -n 50 --no-pager
```

> `WorkingDirectory` 必须是含 `static/` 和 `.env` 的目录，否则会报「前端资源缺失」。

### 6.3 nginx + HTTPS

```nginx
server {
    listen 80;
    server_name your.domain.com;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl http2;
    server_name your.domain.com;

    ssl_certificate     /etc/letsencrypt/live/your.domain.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/your.domain.com/privkey.pem;

    client_max_body_size 20m;      # 字幕/文章上传

    location / {
        proxy_pass http://127.0.0.1:3001;
        proxy_set_header Host $http_host;   # 保留端口也可以，Origin 校验只比主机名
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 120s;
    }

    # 带 hash 的静态资源可以长缓存
    location /assets/ {
        proxy_pass http://127.0.0.1:3001;
        proxy_set_header Host $http_host;
        expires 30d;
        add_header Cache-Control "public, immutable";
    }

    # index.html 不缓存，保证发版后立刻生效
    location = /index.html {
        proxy_pass http://127.0.0.1:3001;
        proxy_set_header Host $http_host;
        add_header Cache-Control "no-cache";
    }
}
```

```bash
sudo certbot --nginx -d your.domain.com     # 首次
sudo nginx -t && sudo systemctl reload nginx
```

> 把 `COOKIE_SECURE=true` 和 HTTPS 一起开；只用 http 调试时先设 `false`。

---

## 7. 升级部署（发新版）

```bash
# 0) 备份
ssh user@host 'cd ~/automated-translation && \
  tar czf ~/backups/app-$(date +%F-%H%M).tar.gz . --exclude=./data.db && \
  cp data.db ~/backups/data.db-$(date +%F-%H%M)'

# 1) 上传
scp target/release/automated-translation user@host:~/automated-translation/automated-translation.new
tar czf - static | ssh user@host 'tar xzf - -C ~/automated-translation'

# 2) 切换
ssh user@host 'cd ~/automated-translation && \
  chmod -R a+rX static && \
  mv -f automated-translation.new automated-translation && \
  sudo systemctl restart automated-translation'

# 3) 看日志确认迁移成功
ssh user@host 'sudo journalctl -u automated-translation -n 30 --no-pager'
```

### 本次（SSR → SPA）升级特别注意

| 变化 | 影响 |
|---|---|
| `templates/` 被删除 | 服务器上旧模板目录可以删掉 |
| `static/` 变成构建产物 | 必须整个替换，不能只传 `style.css` |
| 接口从表单 POST 改为 `/api/*` JSON | 前端已适配，无需外部调用方 |
| 记录 URL 改为 UUIDv7 | **旧的书签/历史链接会 404**，需要提前告知用户 |
| 数据库新增 `public_id` 列 | 启动时自动加列 + 回填 + 建唯一索引，日志会打印「为 N 条历史记录补充了 public_id」 |
| 启动时重置 `running`/`splitting` | 老行为，本次沿用：升级会中断正在跑的任务（状态置 failed，可补译） |

> 建议在低峰期升级；升级前先把 `data.db` 备份一份。

---

## 8. 回滚

```bash
# 回滚代码 + 二进制 + 静态资源
ssh user@host 'sudo systemctl stop automated-translation && \
  cd ~ && tar xzf ~/backups/app-<时间戳>.tar.gz -C ~/automated-translation && \
  sudo systemctl start automated-translation'

# 如果数据库也要回滚（谨慎：会丢掉升级后的新数据）
ssh user@host 'sudo systemctl stop automated-translation && \
  cp ~/backups/data.db-<时间戳> ~/automated-translation/data.db && \
  sudo systemctl start automated-translation'
```

> 数据库只向前兼容：新结构在旧代码下能跑（多一列不影响），所以一般**只回滚代码、不回滚库**。

---

## 9. 上线验证清单

```bash
HOST=http://127.0.0.1:3001
curl -s -o /dev/null -w '首页      %{http_code}\n' $HOST/
curl -s -o /dev/null -w '登录页    %{http_code}\n' $HOST/login
curl -s -o /dev/null -w 'API 未登录 %{http_code}\n' $HOST/api/me      # 期望 401
curl -s -o /dev/null -w '静态资源  %{http_code}\n' $HOST$(curl -s $HOST/ | grep -o '/assets/index-[^"]*\.js' | head -1)
```

浏览器逐项过：

- [ ] 打开首页 → 跳登录页（未登录）
- [ ] 注册 / 登录 → 回到首页，导航显示用户名
- [ ] 设置页保存 API Key → 提示「设置已保存」，掩码更新
- [ ] 上传一个 2 行字幕 → 结果页先「翻译中」，几秒后出译文
- [ ] 结果页「编辑」→ 改一条 → 保存 → Toast「已保存」
- [ ] 下载 vtt / srt（字幕）或 txt / src / csv（文章）
- [ ] 文章上传 → 先「拆分中」→ 自动转表格
- [ ] 切语言（简/繁/港/EN）→ 界面和标题都变
- [ ] 手机宽度（≤840px）→ 汉堡菜单可用
- [ ] https 下确认 Cookie 带 `Secure`（DevTools → Application → Cookies）

---

## 10. 运维

### 日志

```bash
sudo journalctl -u automated-translation -f          # 推荐
tail -f ~/automated-translation/app.log              # 仅当配置了 LOG_FILE
```

用 `LOG_FILE` 时记得配 logrotate（程序本身不轮转）：

```
/home/automated-translation/app.log {
    daily
    rotate 14
    compress
    missingok
    notifempty
    copytruncate
}
```

### 备份 / 恢复

```bash
# 备份（服务运行中也可以，SQLite 单文件）
cp data.db ~/backups/data.db-$(date +%F-%H%M)

# 恢复
sudo systemctl stop automated-translation
cp ~/backups/data.db-<时间戳> data.db
sudo systemctl start automated-translation
```

### 常用命令

```bash
sudo systemctl restart automated-translation
sudo systemctl status automated-translation --no-pager
curl -s http://127.0.0.1:3001/api/me        # 401 = 进程活着
```

---

## 11. 安全清单

- [ ] `SESSION_SECRET` 是随机 32 字节，且**没有**提交到仓库
- [ ] `.env` 权限 `600`，属主为服务用户
- [ ] https 下 `COOKIE_SECURE=true`
- [ ] `HOST=127.0.0.1`，只允许 nginx 访问应用端口
- [ ] `DEFAULT_API_KEY` 留空（或确认愿意承担全站额度），让用户自带 Key
- [ ] 写接口有 Origin 校验（内置，拒绝跨站 POST）
- [ ] 服务器防火墙只开 80/443，应用端口不对公网开放
- [ ] 备份目录不在 web 根下、权限 700

---

## 12. 故障排查

| 现象 | 原因 | 处理 |
|---|---|---|
| 首页 500「前端资源缺失」 | 没跑前端构建，或 `WorkingDirectory` 不对 | `npm run build`；确认 `static/index.html` 存在且工作目录正确 |
| 登录后立刻掉线 | `COOKIE_SECURE=true` 但用 http 访问 | 改回 `false` 或上 https |
| 重启后全部用户要重新登录 | 没配 `SESSION_SECRET`，每次启动随机 | 写入 `.env` 并重启 |
| 上传后一直「翻译中」不变 | API Key 无效 / 余额不足 / 网络不通 | 看 `journalctl` 里的 `DeepSeek ... 401/402`；换 Key 或用 `DEEPSEEK_API_URL` 指向网关 |
| 上传立刻提示「请先在设置页配置 API Key」 | 用户没配 Key 且 `DEFAULT_API_KEY` 为空 | 设计如此；让用户自己配，或配置兜底 Key |
| 写接口 403 `bad_origin` | 反向代理改写了 Host，或跨站调用 | nginx 用 `proxy_set_header Host $http_host;`；跨站调用需要同源 |
| 打开旧链接 404 | 记录 ID 已改为 UUID | 从历史页重新进入 |
| 静态资源 404 | `static/` 没传全，或用了旧路径 `/static/...` | 整个目录重传；新路径是 `/assets/...` |
| 数据库锁 `database is locked` | 多实例 / 长事务 | 只跑一个进程；SQLite 单实例 |
| 启动日志「为 N 条历史记录补充了 public_id」 | 正常迁移 | 无需处理 |

---

## 13. 已知限制

- **单实例**：SQLite 未开 WAL，不要同时跑多个进程/多副本。
- **同一记录同时只能跑一个任务**：翻译/补译/注释互斥，第二个请求返回 `409 busy`。
- **启动会重置未完成任务**：进程崩溃/重启后，`running`/`splitting` 记录变 `failed`（原文还在，可重新上传或补译）。
- **无删除、无分页**：历史最多返回 100 条（首页 5 条）。
- **无内置限流**：公网开放注册时建议在 nginx 层加 `limit_req`。
- **导出格式白名单**：字幕 `vtt/srt/txt`，文章 `txt/src/csv`，其他返回 400。
- **旧数据不兼容**：`translated_text` 无法解析为已知结构时返回 `legacy`，页面提示重新上传。

---

## 14. API 速查

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/auth/register` | 注册（自动登录） |
| POST | `/api/auth/login` | 登录 |
| POST | `/api/auth/logout` | 退出 |
| GET | `/api/me` | 当前用户 + Key 掩码 |
| GET | `/api/translations` | 历史列表 |
| GET | `/api/translations/{uuid}` | 单条（含 entries） |
| GET | `/api/translations/{uuid}/status` | 轮询 `total/done/status/entries` |
| POST | `/api/translations` | multipart 上传并启动翻译 |
| POST | `/api/translations/{uuid}/edits` | 编辑译文（`{"0":"…"}`） |
| POST | `/api/translations/{uuid}/annotate` | 生成注释 |
| POST | `/api/translations/{uuid}/continue` | 补译 |
| GET/PUT | `/api/settings` | 读取 / 保存 Key |
| POST | `/api/lang` | 切换语言 Cookie |
| GET | `/download/{uuid}/{format}` | 导出 |

错误统一为 `{"error":"code"}`，完整错误码见 [FRONTEND-CONTRACT.md](FRONTEND-CONTRACT.md#3-api)。
