#!/usr/bin/env bash
# 起一个可公网访问的 demo 实例：mock DeepSeek + 真实后端（独立数据库）。
# 用于没有 API Key 的验收环境；生产部署请直接跑二进制并配上真正的 Key。
#
#   ./dev/run-demo.sh           # 前台运行，Ctrl-C 结束
#   PORT=18080 ./dev/run-demo.sh
set -euo pipefail

cd "$(dirname "$0")/.."

export DATABASE_URL="${DATABASE_URL:-sqlite://data-spa.db}"
export DEFAULT_API_KEY="${DEFAULT_API_KEY:-mock-key}"
export DEEPSEEK_API_URL="${DEEPSEEK_API_URL:-http://127.0.0.1:3199/chat/completions}"
export SESSION_SECRET="${SESSION_SECRET:-demo-session-secret-please-change-0123456789}"
export COOKIE_SECURE="${COOKIE_SECURE:-false}"
export PORT="${PORT:-18080}"
export HOST="${HOST:-0.0.0.0}"

BIN="./target/release/automated-translation"
if [ ! -x "$BIN" ]; then
  echo "找不到 $BIN，先跑 cargo build --release" >&2
  exit 1
fi

if [ ! -f static/index.html ]; then
  echo "static/index.html 不存在，先跑 (cd frontend && npm run build)" >&2
  exit 1
fi

# mock 模型：已经有进程在监听就复用
if ! curl -s -o /dev/null --max-time 1 http://127.0.0.1:3199/chat/completions; then
  node dev/mock-deepseek.mjs >/tmp/mock-deepseek.log 2>&1 &
  echo "mock deepseek: pid=$! log=/tmp/mock-deepseek.log"
  sleep 0.6
fi

echo "starting on http://${HOST}:${PORT}  (db=${DATABASE_URL})"
exec "$BIN"
