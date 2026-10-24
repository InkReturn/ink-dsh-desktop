/**
 * 本地假模型服务（OpenAI chat-completions 兼容，SSE 流式）。
 *
 * 为什么需要：官方 web 内核只有在「存在可路由 provider」时才会把磁盘上的会话
 * 读进 UI（`agentAvailable: true`），否则聊天区不渲染 transcript，
 * 性能测量和补丁 A 的流式压测都没法做。这里起一个零成本的假端点，
 * 用固定速率吐 token，让流式渲染变成**可控输入**（每秒 N 个 delta）。
 *
 * 只在本地 127.0.0.1 监听，不联网、不需要密钥。
 *
 * 用法：
 *   node test/perf/fake-model.mjs --port 4010 --chunks 120 --interval 25
 *
 * 可选：--seed 让每次回复内容一致（默认按请求序号变化）。
 */

import { createServer } from 'node:http'

/** 解析 `--key value` 形式的命令行参数。 */
function parseArgs(argv) {
  const out = {}
  for (let i = 0; i < argv.length; i += 2) {
    const key = argv[i]
    if (key === undefined || !key.startsWith('--')) throw new Error(`bad argument: ${key}`)
    out[key.slice(2)] = argv[i + 1]
  }
  return out
}

/**
 * 造一段 markdown 回复文本，并按块切分。
 *
 * @param index 请求序号。
 * @param chunks 切分块数。
 * @returns 文本块数组。
 */
function replyChunks(index, chunks) {
  const pieces = []
  pieces.push(`## 第 ${index} 次假响应\n\n`)
  pieces.push('这一段用来撑起渲染成本：标题、段落、列表、表格、代码块都会各来一份。\n\n')
  pieces.push('### 列表\n')
  for (let i = 0; i < 5; i++) pieces.push(`- 列表项 ${i + 1}：用于模拟真实回复的结构密度\n`)
  pieces.push('\n### 代码\n\n```ts\n')
  for (let i = 0; i < 12; i++) pieces.push(`const v${i} = compute(${index}, ${i}, { retries: ${(i % 3) + 1} })\n`)
  pieces.push('```\n\n### 表格\n\n| 指标 | 改前 | 改后 |\n| --- | --- | --- |\n')
  for (let i = 0; i < 3; i++) pieces.push(`| 指标 ${i + 1} | ${40 + i} | ${55 + i} |\n`)
  pieces.push('\n收尾段落：这一段重复几次以拉长流式输出时间。\n')
  for (let i = 0; i < 6; i++) pieces.push(`第 ${i + 1} 段收尾文本，用于延长流式持续时间，便于观察逐 token 渲染。`)

  // 按目标块数把整段文本均匀切开，保证块数稳定、便于算速率。
  const text = pieces.join('')
  const size = Math.max(1, Math.ceil(text.length / chunks))
  const out = []
  for (let i = 0; i < text.length; i += size) out.push(text.slice(i, i + size))
  return out
}

const args = parseArgs(process.argv.slice(2))
const port = Number(args.port ?? 4010)
const chunks = Number(args.chunks ?? 120)
const interval = Number(args.interval ?? 25)
let requestCount = 0

const server = createServer((req, res) => {
  console.log(JSON.stringify({ at: new Date().toISOString(), method: req.method, url: req.url, ua: req.headers['user-agent'] }))
  if (req.method === 'GET' && req.url?.startsWith('/v1/models')) {
    res.writeHead(200, { 'Content-Type': 'application/json' })
    res.end(JSON.stringify({ object: 'list', data: [{ id: 'perf-model', object: 'model', owned_by: 'local' }] }))
    return
  }
  if (req.method !== 'POST' || !req.url?.startsWith('/v1/chat/completions')) {
    res.writeHead(404, { 'Content-Type': 'application/json' })
    res.end(JSON.stringify({ error: { message: 'not found' } }))
    return
  }
  let body = ''
  req.on('data', (chunk) => { body += chunk })
  req.on('end', () => {
    let streaming = true
    try { streaming = JSON.parse(body).stream !== false }
    catch {}
    const index = ++requestCount
    const id = `chatcmpl-perf-${index}`
    const created = Math.floor(Date.now() / 1000)
    const parts = replyChunks(index, chunks)

    // 1. 非流式：一次性返回，便于排查连通性。
    if (!streaming) {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      res.end(JSON.stringify({
        id, object: 'chat.completion', created, model: 'perf-model',
        choices: [{ index: 0, message: { role: 'assistant', content: parts.join('') }, finish_reason: 'stop' }],
        usage: { prompt_tokens: 10, completion_tokens: parts.length, total_tokens: 10 + parts.length },
      }))
      return
    }

    // 2. 流式：按固定间隔逐块推 SSE，速率 = 1000/interval 块/秒。
    res.writeHead(200, {
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache',
      'Connection': 'keep-alive',
    })
    const send = (payload) => res.write(`data: ${JSON.stringify(payload)}\n\n`)
    send({ id, object: 'chat.completion.chunk', created, model: 'perf-model', choices: [{ index: 0, delta: { role: 'assistant' }, finish_reason: null }] })
    let cursor = 0
    const timer = setInterval(() => {
      if (cursor >= parts.length) {
        clearInterval(timer)
        send({ id, object: 'chat.completion.chunk', created, model: 'perf-model', choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })
        send({ id, object: 'chat.completion.chunk', created, model: 'perf-model', choices: [], usage: { prompt_tokens: 10, completion_tokens: parts.length, total_tokens: 10 + parts.length } })
        res.write('data: [DONE]\n\n')
        res.end()
        return
      }
      send({ id, object: 'chat.completion.chunk', created, model: 'perf-model', choices: [{ index: 0, delta: { content: parts[cursor++] }, finish_reason: null }] })
    }, interval)
    // 注意：req 的 'close' 在请求体读完时就会触发（不是客户端断开），
    // 用它清定时器会让响应永远写不完 —— 必须挂 res 的 close。
    res.on('close', () => clearInterval(timer))
  })
})

server.listen(port, '127.0.0.1', () => {
  console.log(JSON.stringify({ listening: `http://127.0.0.1:${port}/v1`, chunks, intervalMs: interval, chunksPerSecond: Math.round(1000 / interval) }))
})
