/**
 * 应用内性能测量（真 `dsh web` + 真会话 + 假模型流式）。
 *
 * 环境准备见 docs/perf/app-baseline.md：隔离 DSH_HOME + 假 provider + 用
 * `dsh headless` 生成的真会话。这里只负责「打开页面 → 点会话 → 量指标」。
 *
 * 三种模式：
 * 1. `switch`：依次点开 N 个会话，量「点击 → 首条消息渲染」耗时与 DOM 规模。
 * 2. `scroll`：在最长的会话里连续滚动，量帧率 / 主线程占用 / 长任务。
 * 3. `stream`：在输入框发一条消息，量流式期间（假模型按固定速率吐 token）的帧率。
 *
 * 用法：
 *   node test/perf/app-measure.mjs --url "http://127.0.0.1:3098/?token=…" --mode switch
 */

import { chromium } from 'playwright'

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
 * 关掉首次进入的欢迎弹层（存在才点）。
 *
 * @param page Playwright 页面。
 */
async function dismissOverlays(page) {
  for (const name of ['继续', '稍后配置']) {
    const button = page.getByRole('button', { name })
    if (await button.count()) await button.first().click().catch(() => {})
  }
  await page.keyboard.press('Escape').catch(() => {})
  await page.waitForTimeout(600)
}

/**
 * 打开第 index 个会话并等待首条消息渲染。
 *
 * @param page Playwright 页面。
 * @param index 会话在侧边栏里的序号。
 * @returns 渲染耗时与 DOM 规模。
 */
async function openSession(page, index) {
  // 只挑「有真实回复」的会话行：侧边栏第一行是空白新会话，点它永远没有 flowItem。
  const items = page.locator('[data-row-key^="session:"]').filter({ hasText: '次假响应' })
  const count = await items.count()
  if (count <= index) return { index, error: `only ${count} session rows` }
  const started = Date.now()
  await items.nth(index).click()
  await page.waitForFunction(() => document.querySelectorAll('[data-chat-flow-key]').length > 0, { timeout: 30_000 }).catch(() => {})
  const firstRenderMs = Date.now() - started
  await page.waitForTimeout(1200)
  const stats = await page.evaluate(() => ({
    dom: document.querySelectorAll('*').length,
    flowItems: document.querySelectorAll('[data-chat-flow-key]').length,
    scrollHeight: Math.round(document.querySelector('[data-conversation-scroll]')?.scrollHeight ?? 0),
  }))
  return { index, firstRenderMs, ...stats }
}

/**
 * 在页面里跑一段滚动测量。
 *
 * @param page Playwright 页面。
 * @param seconds 测量时长。
 * @returns 测量结果。
 */
async function measureScroll(page, seconds) {
  return page.evaluate(async (seconds) => {
    const scroller = document.querySelector('[data-conversation-scroll]')
    if (scroller === null) return { error: 'no scrollport' }
    const frames = []
    const tasks = []
    const observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) tasks.push(entry.duration)
    })
    observer.observe({ entryTypes: ['longtask'] })
    let busy = 0
    let last = performance.now()
    const deadline = last + seconds * 1000
    const max = scroller.scrollHeight - scroller.clientHeight
    let position = scroller.scrollTop
    let direction = -1
    await new Promise((done) => {
      const frame = (now) => {
        const start = performance.now()
        frames.push(now - last)
        last = now
        position += direction * Math.max(120, Math.round(max / 300))
        if (position <= 0) { position = 0; direction = 1 }
        if (position >= max) { position = max; direction = -1 }
        scroller.scrollTop = position
        busy += performance.now() - start
        if (now < deadline) requestAnimationFrame(frame)
        else done()
      }
      requestAnimationFrame(frame)
    })
    observer.disconnect()
    frames.shift()
    const sorted = [...frames].sort((a, b) => a - b)
    const total = frames.reduce((sum, value) => sum + value, 0)
    const pct = q => Math.round(sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * q))] * 10) / 10
    return {
      frames: frames.length,
      fps: total === 0 ? 0 : Math.round((frames.length / total) * 1000 * 10) / 10,
      busyRatio: total === 0 ? 0 : Math.round((busy / total) * 1000) / 1000,
      frameP50: pct(0.5),
      frameP95: pct(0.95),
      longTasks: tasks.length,
      longTaskMs: Math.round(tasks.reduce((sum, value) => sum + value, 0)),
      flowItems: document.querySelectorAll('[data-chat-flow-key]').length,
      dom: document.querySelectorAll('*').length,
    }
  }, seconds)
}

/**
 * 发一条消息并测量流式期间的帧率。
 *
 * @param page Playwright 页面。
 * @param seconds 观测时长。
 * @returns 测量结果。
 */
async function measureStream(page, seconds) {
  const started = await page.evaluate(async (seconds) => {
    const frames = []
    const tasks = []
    const observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) tasks.push(entry.duration)
    })
    observer.observe({ entryTypes: ['longtask'] })
    const before = document.querySelectorAll('[data-chat-flow-key]').length
    let busy = 0
    let last = performance.now()
    let glyphs = 0
    const deadline = last + seconds * 1000
    await new Promise((done) => {
      const frame = (now) => {
        const start = performance.now()
        frames.push(now - last)
        last = now
        // 用「最后一个 flowItem 的文本长度」当作流式进度的代理
        const items = document.querySelectorAll('[data-chat-flow-key]')
        glyphs = (items[items.length - 1]?.textContent ?? '').length
        busy += performance.now() - start
        if (now < deadline) requestAnimationFrame(frame)
        else done()
      }
      requestAnimationFrame(frame)
    })
    observer.disconnect()
    frames.shift()
    const sorted = [...frames].sort((a, b) => a - b)
    const total = frames.reduce((sum, value) => sum + value, 0)
    const pct = q => Math.round(sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * q))] * 10) / 10
    return {
      before,
      after: document.querySelectorAll('[data-chat-flow-key]').length,
      lastItemChars: glyphs,
      frames: frames.length,
      fps: total === 0 ? 0 : Math.round((frames.length / total) * 1000 * 10) / 10,
      busyRatio: total === 0 ? 0 : Math.round((busy / total) * 1000) / 1000,
      frameP95: pct(0.95),
      longTasks: tasks.length,
      longTaskMs: Math.round(tasks.reduce((sum, value) => sum + value, 0)),
      dom: document.querySelectorAll('*').length,
    }
  }, seconds)
  return started
}

const args = parseArgs(process.argv.slice(2))
const url = args.url
const mode = args.mode ?? 'switch'
const seconds = Number(args.seconds ?? 8)
const runs = Number(args.runs ?? 1)

if (url === undefined) throw new Error('--url is required')

const browser = await chromium.launch({ channel: 'msedge', headless: true })
const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 })
await page.goto(url, { waitUntil: 'domcontentloaded' })
await page.waitForTimeout(6000)
await dismissOverlays(page)

const output = { mode, url: url.replace(/token=[^&]+/, 'token=***'), results: [] }

if (mode === 'switch') {
  // 1. 依次打开 5 个会话，各量一次「点击 → 首条渲染」。
  for (let run = 0; run < runs; run++) {
    for (let index = 0; index < 5; index++) {
      // 冷开：每个会话前重载页面，避免命中上一轮已渲染的缓存。
      if (args.cold !== undefined && !(run === 0 && index === 0)) {
        await page.reload({ waitUntil: 'domcontentloaded' })
        await page.waitForTimeout(5000)
        await dismissOverlays(page)
      }
      const result = await openSession(page, index)
      output.results.push(result)
    }
  }
  const ok = output.results.filter(r => r.firstRenderMs !== undefined)
  output.summary = {
    switches: ok.length,
    meanMs: Math.round(ok.reduce((sum, r) => sum + r.firstRenderMs, 0) / Math.max(1, ok.length)),
    maxMs: Math.max(0, ...ok.map(r => r.firstRenderMs)),
    flowItems: ok.map(r => r.flowItems),
  }
}
else if (mode === 'scroll') {
  // 2. 打开最长的会话（第 0 条）后连续滚动。
  const opened = await openSession(page, Number(args.index ?? 0))
  output.opened = opened
  for (let run = 0; run < runs; run++) output.results.push(await measureScroll(page, seconds))
}
else if (mode === 'stream') {
  // 3. 新开会话发一条消息，量流式期间帧率。
  const opened = await openSession(page, Number(args.index ?? 0))
  output.opened = opened
  const box = page.locator('[contenteditable="true"], textarea').first()
  await box.click()
  await page.keyboard.type('性能压测：请按固定速率输出一段长文本')
  await page.waitForTimeout(400)
  // 官方 composer 的 Enter 是换行，发送要按「发送消息」按钮。
  const send = page.locator('button[aria-label="发送消息"], button[aria-label="Send message"]').first()
  if (await send.count()) await send.click()
  else await page.keyboard.press('Control+Enter')
  await page.waitForTimeout(600)
  for (let run = 0; run < runs; run++) output.results.push(await measureStream(page, seconds))
}

console.log(JSON.stringify(output, null, 2))
await browser.close()
