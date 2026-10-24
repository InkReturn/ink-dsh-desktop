/**
 * 用真实内核 + 假模型生成长会话（供应用内性能测量）。
 *
 * 为什么不用手写日志：会话要能被 `session/list` 列出来、被 UI 打开，就必须经过
 * 内核自己的写入路径（sqlite 索引 + 投影缓存）。所以这里直接跑
 * `dsh headless`（每次都走真实的事件写入），假模型负责按固定速率吐 token，
 * 于是「流式渲染」变成可控输入。
 *
 * 前置：
 * 1. 隔离 home（默认 `D:/inkdsh-perf/home`）里已配好 perf-local provider（见 docs/perf/README）
 * 2. 假模型已在 `127.0.0.1:4010` 运行：`node test/perf/fake-model.mjs`
 *
 * 用法：
 *   node test/perf/seed-sessions.mjs --count 5 --turns 20
 */

import { execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync, readdirSync } from 'node:fs'
import { join } from 'node:path'

/** dsh CLI 入口（直接走 node，避开 Windows 上的 .cmd 包装）。 */
const DSH_BIN = process.env.DSH_BIN
  ?? 'C:/Users/inkre/AppData/Roaming/npm/node_modules/@deepseek-ai/dsh/lib/bin.js'

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
 * 跑一次 headless 调用。
 *
 * @param home 隔离的 DSH_HOME。
 * @param cwd 会话工作目录（也是子进程 cwd）。
 * @param task 任务文本。
 * @param sessionId 续写已有会话时给出。
 * @returns `{ sessionId, ok }`。
 */
function runHeadless(home, cwd, task, sessionId) {
  const args = ['headless', '--json', task]
  if (sessionId !== undefined) args.push('--session-id', sessionId)
  let stdout = ''
  try {
    stdout = execFileSync(process.execPath, [DSH_BIN, ...args], {
      cwd,
      encoding: 'utf8',
      timeout: 120_000,
      env: { ...process.env, DSH_HOME: home, PERF_LOCAL_API_KEY: 'dummy' },
    })
  }
  catch (error) {
    // 续写失败时内核会把错误写进 stdout 之外，这里保留已有输出继续判断。
    stdout = String(error.stdout ?? '')
  }
  const line = stdout.split('\n').find(l => l.includes('"type":"session"'))
  const id = line === undefined ? undefined : JSON.parse(line).sessionId
  return { sessionId: id ?? sessionId, ok: stdout.includes('"phase":"turn_end"') }
}

const args = parseArgs(process.argv.slice(2))
const home = args.home ?? 'D:/inkdsh-perf/home'
const cwd = args.cwd ?? 'D:\\inkdsh-perf\\workspace'
const count = Number(args.count ?? 5)
const turns = Number(args.turns ?? 20)

const created = []
for (let s = 0; s < count; s++) {
  let sessionId
  for (let t = 0; t < turns; t++) {
    const task = `第 ${t + 1} 轮：请给出这一层的性能分析与改动建议，包含要点、代码片段和对比表格。`
    const result = runHeadless(home, cwd, task, sessionId)
    sessionId = result.sessionId
    if (!result.ok) {
      console.error(`session ${s + 1} turn ${t + 1} failed`)
      break
    }
    process.stdout.write(`\rsession ${s + 1}/${count} turn ${t + 1}/${turns}   `)
  }
  console.log(`\nsession ${s + 1}: ${sessionId}`)
  created.push(sessionId)
}

// 1. 把新会话挂到默认工作区，否则 web UI 的会话列表不会显示它们。
const workspacePath = join(home, 'storages/workspace.json')
const workspace = JSON.parse(readFileSync(workspacePath, 'utf8'))
const workspaceId = workspace.global.defaultWorkspaceId
const sessionsDir = join(home, 'sessions', '--D-inkdsh-perf-workspace--')
const onDisk = readdirSync(sessionsDir).filter(name => name.startsWith('session-'))
workspace.tables.workspaces[workspaceId].sessionIds = onDisk
workspace.tables.workspaces[workspaceId].path = cwd
writeFileSync(workspacePath, `${JSON.stringify(workspace, null, 2)}\n`)

console.log(JSON.stringify({ sessions: created, workspaceSessions: onDisk.length, workspacePath }, null, 2))
