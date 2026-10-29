// 本地 mock：模拟 DeepSeek /chat/completions，用于没有 API Key 的端到端验证。
//
// 用法：
//   node dev/mock-deepseek.mjs            # 监听 127.0.0.1:3199
//   DEEPSEEK_API_URL=http://127.0.0.1:3199/chat/completions cargo run
//
// 它会根据请求体里的 JSON 形状判断当前是哪一步：
//   {"text": "..."}                    → 拆句      → {"sentences":[...]}
//   {"sentences": [...]}               → 校验      → 原样返回
//   {"items":[{"id","text"}]}          → 翻译      → {"pairs":[...]}
//   {"items":[{"id","text","translation"}]} + 提示词含 note → 注释 → {"notes":[...]}
//   {"items":[... "translation" ...]}  → 补译      → {"pairs":[...]}

import { createServer } from 'node:http'

const PORT = Number(process.env.PORT ?? 3199)
const HOST = process.env.HOST ?? '127.0.0.1'

function splitSentences(text) {
  return String(text)
    .split(/(?<=[.!?。！？])\s+|\n{2,}/)
    .map((s) => s.trim())
    .filter(Boolean)
    .slice(0, 120)
}

function buildContent(system, user) {
  let data
  try {
    data = JSON.parse(user)
  } catch {
    return { sentences: splitSentences(user) }
  }

  if (Array.isArray(data.sentences)) return { sentences: data.sentences }
  if (typeof data.text === 'string' && !data.items) return { sentences: splitSentences(data.text) }

  if (Array.isArray(data.items)) {
    if (/note|annotation|注释|註解/i.test(system)) {
      return {
        notes: data.items.map((item) => ({
          id: item.id,
          note: `（mock 注释）${String(item.text ?? '').slice(0, 24)}…`,
        })),
      }
    }
    return {
      pairs: data.items.map((item) => ({
        id: item.id,
        translation: `［译］${String(item.text ?? '').replace(/\s+/g, ' ')}`,
      })),
    }
  }

  return { sentences: [] }
}

const server = createServer((req, res) => {
  if (req.method !== 'POST') {
    res.writeHead(405, { allow: 'POST' })
    res.end('method not allowed')
    return
  }

  let raw = ''
  req.on('data', (chunk) => {
    raw += chunk
  })

  req.on('end', () => {
    let payload = {}
    try {
      payload = JSON.parse(raw)
    } catch {
      res.writeHead(400, { 'content-type': 'application/json' })
      res.end(JSON.stringify({ error: 'invalid json' }))
      return
    }

    const messages = payload.messages ?? []
    const system = String(messages[0]?.content ?? '')
    const user = String(messages[1]?.content ?? '')
    const content = JSON.stringify(buildContent(system, user))

    res.writeHead(200, { 'content-type': 'application/json' })
    res.end(
      JSON.stringify({
        id: 'mock',
        choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }],
        usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
      }),
    )
  })
})

server.listen(PORT, HOST, () => {
  console.log(`mock deepseek listening on http://${HOST}:${PORT}/chat/completions`)
})
