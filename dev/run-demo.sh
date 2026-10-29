#!/usr/bin/env bash
# 启动实例。
#
#   ./dev/run-demo.sh          # 用 .env 里的配置（真 Key / 真库）
#   MOCK=1 ./dev/run-demo.sh   # 强制用本地假模型 + 演示库，不消耗 API 额度
#
# 注意：环境变量的优先级高于 .env（dotenvy 不会覆盖已存在的变量），
# 所以只有 MOCK=1 时才注入 mock 配置，避免悄悄顶掉生产 Key。
set -euo pipefail

cd "$(dirname "$0")/.."

BIN="./target/release/automated-translation"
if [ ! -x "$BIN" ]; then
  echo "找不到 $BIN，先跑 cargo build --release" >&2
  exit 1
fi

if [ ! -f static/index.html ]; then
  echo "static/index.html 不存在，先跑 (cd frontend && npm run build)" >&2
  exit 1
fi

if [ "${MOCK:-0}" = "1" ]; then
  export DEFAULT_API_KEY="${DEFAULT_API_KEY:-mock-key}"
  export DEEPSEEK_API_URL="${DEEPSEEK_API_URL:-http://127.0.0.1:3199/chat/completions}"
  export DATABASE_URL="${DATABASE_URL:-sqlite://data-spa.db}"

  if ! curl -s -o /dev/null --max-time 1 "$DEEPSEEK_API_URL"; then
    node dev/mock-deepseek.mjs >/tmp/mock-deepseek.log 2>&1 &
    echo "mock deepseek: pid=$! log=/tmp/mock-deepseek.log"
    sleep 0.6
  fi
  echo "MOCK 模式：假模型 + ${DATABASE_URL}"
else
  echo "使用 .env 配置"
fi

exec "$BIN"
