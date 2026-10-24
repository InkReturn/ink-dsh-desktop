/**
 * 基线/回归扫描：一次跑完「DOM 规模 × content-visibility × 模式」的组合，
 * 把中位数汇总成一个 JSON，供 `docs/perf/` 引用。
 *
 * 用途：P2 改补丁前后各跑一次，同一台机器、同一命令，直接对比。
 *
 * 用法：
 *   node test/perf/sweep.mjs --out docs/perf/baseline-local.json
 */

import { execFileSync } from 'node:child_process'
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'

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

const args = parseArgs(process.argv.slice(2))
const out = args.out ?? 'docs/perf/baseline-local.json'
const items = (args.items ?? '300,1000,2000').split(',').map(Number)
const seconds = Number(args.seconds ?? 8)
const runs = Number(args.runs ?? 3)
const here = import.meta.dirname

/** 生成（或复用）一份 fixture。 */
function ensureFixture(count, cv) {
  const name = cv ? `fixture-${count}-cv` : `fixture-${count}`
  execFileSync(process.execPath, [join(here, 'make-fixture.mjs'), '--items', String(count), ...(cv ? ['--cv'] : [])], { stdio: 'ignore' })
  return join(here, 'out', `${name}.html`)
}

/** 跑一条测量并解析出 JSON。 */
function run(file, mode, hz) {
  const stdout = execFileSync(process.execPath, [join(here, 'scroll-fps.mjs'), '--file', file, '--mode', mode, '--seconds', String(seconds), '--runs', String(runs), '--hz', String(hz)], { encoding: 'utf8' })
  return JSON.parse(stdout)
}

const rows = []
for (const count of items) {
  for (const cv of [false, true]) {
    const file = ensureFixture(count, cv)
    // 1. 滚动：整列滚动时的帧率与主线程占用。
    const scroll = run(file, 'scroll', 0)
    rows.push({ items: count, contentVisibility: cv, mode: 'scroll', hz: null, ...scroll.median })
    // 2. 流式代理：按 30Hz / 10Hz 重写最后一条消息，模拟 token 提交频率。
    for (const hz of [30, 10]) {
      const stream = run(file, 'stream', hz)
      rows.push({ items: count, contentVisibility: cv, mode: 'stream', hz, ...stream.median })
    }
  }
}

const report = {
  generatedAt: new Date().toISOString(),
  engine: 'msedge (Chromium 155 — 与本机 WebView2 155.0.4283.62 同版本)',
  viewport: '1440x900 @ dpr1',
  secondsPerRun: seconds,
  runs,
  rows,
}

mkdirSync(dirname(out), { recursive: true })
writeFileSync(out, `${JSON.stringify(report, null, 2)}\n`)

const header = '| items | DOM | cv | mode | hz | fps | busy | p95 frame | longTask | scrollHeight |'
console.log(header)
console.log('| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |')
for (const row of rows) {
  console.log(`| ${row.items} | ${row.domNodes} | ${row.contentVisibility ? 'on' : 'off'} | ${row.mode} | ${row.hz ?? '-'} | ${row.fps} | ${row.busyRatio} | ${row.frameP95} | ${row.longTaskMs} | ${row.scrollHeight} |`)
}
console.log(`\nwritten: ${out}`)
