/**
 * 生成长会话 fixture（供 P2 性能基线/回归测试使用）。
 *
 * 为什么需要它：官方没有现成的「万级 DOM 会话」样本，手点几十轮既慢又不可复现。
 * 这里按官方 session 日志格式（`dsh-session` 的 SessionEvent，v4 jsonl + zstd）
 * 直接合成一份，事件类型与字段全部取自 `@deepseek-ai/dsh-session@0.2.1-rc.2`
 * 的 `types.d.ts`；目录命名照抄 `dsh-session-persistence-jsonl` 的 `projectKey()`
 * 与 `sessionDir()`。
 *
 * 写入范围：只写 `--home` 指向的目录（默认 `~/.dsh.dev`，即 debug 构建的数据根），
 * 绝不触碰 release 的 `~/.dsh`。已有 session 不会被覆盖（id 带随机后缀）。
 *
 * 用法：
 *   node test/perf/make-session.mjs --turns 120
 *   node test/perf/make-session.mjs --turns 20 --cwd D:\\tmp\\perf --home C:\\Users\\x\\.dsh.dev
 */

import { mkdirSync, writeFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'
import { zstdCompressSync } from 'node:zlib'

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
 * 可复现的伪随机数发生器（mulberry32）。
 *
 * @param seed 初始种子。
 * @returns 返回 `[0,1)` 的函数。
 */
function rng(seed) {
  let a = seed >>> 0
  return () => {
    a = (a + 0x6D2B79F5) >>> 0
    let t = Math.imul(a ^ (a >>> 15), 1 | a)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

/**
 * 官方目录键：分隔符与盘符压成 `-`，非安全字符走 `~XXXX`（照抄 projectKey）。
 *
 * @param cwd 会话的工作目录。
 * @returns 单层文件系统安全目录名。
 */
function projectKey(cwd) {
  let readable = ''
  let separatorRun = false
  for (let i = 0; i < cwd.length; i++) {
    const code = cwd.charCodeAt(i)
    const ch = String.fromCharCode(code)
    if (ch === '/' || ch === '\\' || ch === ':') {
      if (!separatorRun) readable += '-'
      separatorRun = true
    }
    else if (ch !== '~' && /^[A-Za-z0-9._-]$/.test(ch)) {
      readable += ch
      separatorRun = false
    }
    else {
      readable += `~${code.toString(16).toUpperCase().padStart(4, '0')}`
      separatorRun = false
    }
  }
  return `--${(readable.replace(/^-+/, '') || 'root').slice(0, 251)}--`
}

/** 生成一个 v4 版本的 uuid（官方 id 形态）。 */
function uuid() {
  const hex = Array.from({ length: 32 }, (_, i) => Math.floor(Math.random() * 16).toString(16))
  return `${hex.slice(0, 8).join('')}-${hex.slice(8, 12).join('')}-4${hex.slice(13, 16).join('')}-a${hex.slice(17, 20).join('')}-${hex.slice(20, 32).join('')}`
}

/**
 * 造一段 markdown 正文，覆盖聊天里最占 DOM 的几种块（标题/列表/表格/代码/引用）。
 *
 * @param r 随机源。
 * @param index 轮次序号，用于让每轮内容不同。
 * @param richness 内容丰度（1=短，2=中，3=长）。
 * @returns markdown 文本。
 */
function assistantMarkdown(r, index, richness) {
  const phrases = [
    '先确认一下这里的约束条件，再决定改哪一层。',
    '这个路径的失败模式是静默的，必须显式打日志才能看见。',
    '把补丁锚点收窄到一条语句，回滚粒度才够细。',
    '缓存命中率不是瓶颈，重渲染次数才是。',
    '这里的顺序依赖很容易被并发打乱，需要串行化。',
    '先量化再优化：没有基线就没有验收。',
  ]
  const pick = () => phrases[Math.floor(r() * phrases.length)]
  const parts = [`## 第 ${index + 1} 轮分析\n`, `${pick()}${pick()}\n`]

  if (richness >= 2) {
    parts.push('### 待办\n')
    for (let i = 0; i < 3 + Math.floor(r() * 3); i++) parts.push(`- 项 ${i + 1}：${pick()}\n`)
    parts.push('')
  }

  if (richness >= 3) {
    parts.push('```ts\n')
    parts.push(`// 示例片段 ${index + 1}\n`)
    for (let i = 0; i < 6 + Math.floor(r() * 6); i++) {
      parts.push(`const value${i} = compute(${index}, ${i}, { retries: ${i % 3 + 1}, label: '${pick().slice(0, 8)}' })\n`)
    }
    parts.push('```\n\n')
    parts.push('| 指标 | 改前 | 改后 |\n| --- | --- | --- |\n')
    for (let i = 0; i < 3; i++) parts.push(`| 指标 ${i + 1} | ${40 + Math.floor(r() * 20)} | ${55 + Math.floor(r() * 10)} |\n`)
    parts.push('')
    parts.push(`> ${pick()}\n\n`)
    parts.push(`${pick()}${pick()}${pick()}\n`)
  }
  return parts.join('')
}

/**
 * 合成一份会话日志并写入 DSH home。
 *
 * @param options 生成参数。
 * @returns 写入的 session 目录与统计。
 */
function generate({ home, cwd, turns, seed, richness }) {
  const r = rng(seed)
  const sessionId = `session-${uuid()}`
  const createdAt = Date.now() - turns * 60_000
  const lines = []

  // 1. 会话头：字段与官方写出的 header 完全一致（含 isSeeded/delegationDepth/agentPreset）。
  lines.push(JSON.stringify({
    type: 'session',
    version: 4,
    id: sessionId,
    createdAt,
    cwd,
    isSeeded: false,
    delegationDepth: 0,
    agentPreset: 'standard',
  }))

  let seq = 0
  let time = createdAt
  /** 追加一条事件：统一维护 seq 连续与 time 递增。 */
  const push = (type, data, surface = false) => {
    time += 40 + Math.floor(r() * 400)
    const event = { type, seq: seq++, time, data }
    if (surface) event.surfaceOp = 'append'
    lines.push(JSON.stringify(event))
  }
  const messageId = () => `msg-${uuid()}`

  for (let turn = 0; turn < turns; turn++) {
    push('turn/start', { turn })
    push('step/start', { turn, step: 0 })

    // 2. 用户消息：短句，模拟真实提问。
    push('user/message', {
      id: messageId(),
      role: 'user',
      content: [{ type: 'text', text: `第 ${turn + 1} 轮：帮我看看这一层为什么慢，以及该在哪里下补丁。` }],
      source: { kind: 'user' },
    }, true)

    // 3. 助手消息：富文本正文，撑起 DOM 量。
    push('assistant/message', {
      turn,
      step: 0,
      message: {
        id: messageId(),
        role: 'assistant',
        content: [{ type: 'text', text: assistantMarkdown(r, turn, richness) }],
        source: { kind: 'model', provider: 'perf-fixture', model: 'perf-fixture' },
      },
      stream: [],
    }, true)

    push('step/end', { turn, step: 0 })
    push('turn/end', { turn, reason: { kind: 'completed' } })
  }

  const dir = join(home, 'sessions', projectKey(cwd), sessionId)
  mkdirSync(dir, { recursive: true })

  // 4. 单帧 zstd（官方读取器支持多帧，也支持单帧）。
  const payload = `${lines.join('\n')}\n`
  const compressed = zstdCompressSync(Buffer.from(payload, 'utf8'))
  writeFileSync(join(dir, 'session.v4.jsonl.zstd'), compressed)

  return {
    sessionId,
    dir,
    events: seq,
    turns,
    rawBytes: payload.length,
    compressedBytes: compressed.length,
  }
}

const args = parseArgs(process.argv.slice(2))
const home = args.home ?? join(homedir(), '.dsh.dev')
const cwd = args.cwd ?? join(homedir(), 'Documents', 'deepseek-harness', 'default-workspace')
const turns = Number(args.turns ?? 120)
const seed = Number(args.seed ?? 20261024)
const richness = Number(args.richness ?? 3)

const result = generate({ home, cwd, turns, seed, richness })
console.log(JSON.stringify(result, null, 2))
