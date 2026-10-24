/**
 * 滚动 / 流式更新的帧率测量（P2 基线）。
 *
 * 引擎对齐：本机 WebView2 与 Edge 都是 155.0.4283.62，所以用 `channel: 'msedge'`
 * 起浏览器，量的是同一套 Blink 渲染行为；测的是**渲染层**（布局/绘制/合成），
 * 不含 Tauri 外壳与 React 调度差异，这一点写进报告里。
 *
 * 指标（都跟 vsync 无关，便于跨机比较）：
 * 1. `fps`：rAF 回调频率（headless 下可能被放大，只作参考）
 * 2. `busyRatio`：rAF 回调总耗时 / 总时长 —— 主线程在帧内被占用的比例
 * 3. `longTasks`：>50ms 长任务的数量与总时长（PerformanceObserver）
 * 4. `frameP50/frameP95`：rAF 间隔分位数
 *
 * 用法：
 *   node test/perf/scroll-fps.mjs --file test/perf/out/fixture-300.html --mode scroll
 *   node test/perf/scroll-fps.mjs --file … --mode stream --hz 30
 */

import { chromium } from 'playwright'
import { pathToFileURL } from 'node:url'
import { resolve } from 'node:path'

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
 * 在页面里跑一次测量（scroll 或 stream 两种模式）。
 *
 * @param page Playwright 页面。
 * @param options 模式、时长与频率。
 * @returns 测量结果。
 */
async function measure(page, { mode, seconds, hz }) {
  return page.evaluate(async ({ mode, seconds, hz }) => {
    const frames = []
    const tasks = []
    const observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) tasks.push(entry.duration)
    })
    observer.observe({ entryTypes: ['longtask'] })

    const scroller = document.querySelector('[data-conversation-scroll]')
    const items = [...document.querySelectorAll('[data-chat-flow-key]')]
    const last = items.at(-1)?.querySelector('[data-slot="conversation.chat.node"]') ?? null
    const template = last === null ? '' : last.innerHTML

    let busy = 0
    let lastTime = performance.now()
    const deadline = lastTime + seconds * 1000
    let streamTimer = 0
    let streamCount = 0
    const streamPeriod = hz > 0 ? 1000 / hz : 0

    // 1. 流式模式：按目标频率替换最后一条消息的 DOM 子树，模拟一次 token 提交。
    //    用 innerHTML 重写而不是文本追加，是为了覆盖「markdown 重解析 + 布局」这段成本。
    if (mode === 'stream' && last !== null && streamPeriod > 0) {
      const tick = () => {
        last.innerHTML = template
        streamCount++
        streamTimer = setTimeout(tick, streamPeriod)
      }
      tick()
    }

    // 2. 滚动模式：以恒定速度来回滚，逼出每一帧的布局与绘制。
    const maxScroll = scroller.scrollHeight - scroller.clientHeight
    let direction = 1
    let position = 0
    const stepPerFrame = Math.max(8, Math.round(maxScroll / 400))

    await new Promise((done) => {
      const frame = (now) => {
        const start = performance.now()
        frames.push(now - lastTime)
        lastTime = now
        if (mode === 'scroll') {
          position += direction * stepPerFrame
          if (position > maxScroll) { position = maxScroll; direction = -1 }
          if (position < 0) { position = 0; direction = 1 }
          scroller.scrollTop = position
          // 强制同步布局，让本帧真实付出布局代价（等同于读 offsetHeight）
          void scroller.scrollHeight
        }
        busy += performance.now() - start
        if (now < deadline) requestAnimationFrame(frame)
        else done()
      }
      requestAnimationFrame(frame)
    })

    clearTimeout(streamTimer)
    observer.disconnect()
    frames.shift()
    const sorted = [...frames].sort((a, b) => a - b)
    const pct = (q) => sorted.length === 0 ? 0 : sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * q))]
    const total = frames.reduce((sum, value) => sum + value, 0)
    return {
      mode,
      hz: mode === 'stream' ? hz : null,
      durationMs: Math.round(total),
      frames: frames.length,
      fps: total === 0 ? 0 : Math.round((frames.length / total) * 1000 * 10) / 10,
      busyRatio: total === 0 ? 0 : Math.round((busy / total) * 1000) / 1000,
      frameP50: Math.round(pct(0.5) * 10) / 10,
      frameP95: Math.round(pct(0.95) * 10) / 10,
      frameMax: Math.round((sorted.at(-1) ?? 0) * 10) / 10,
      longTasks: tasks.length,
      longTaskMs: Math.round(tasks.reduce((sum, value) => sum + value, 0)),
      streamUpdates: streamCount,
      domNodes: document.querySelectorAll('*').length,
      flowItems: items.length,
      scrollHeight: Math.round(scroller.scrollHeight),
    }
  }, { mode, seconds, hz })
}

const args = parseArgs(process.argv.slice(2))
const file = resolve(args.file ?? 'test/perf/out/fixture-300.html')
const mode = args.mode ?? 'scroll'
const seconds = Number(args.seconds ?? 8)
const runs = Number(args.runs ?? 3)
const hz = Number(args.hz ?? 30)

const browser = await chromium.launch({ channel: 'msedge', headless: process.env.PERF_HEADED ? false : true })
const results = []
for (let run = 0; run < runs; run++) {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 })
  await page.goto(pathToFileURL(file).href, { waitUntil: 'load' })
  await page.waitForTimeout(500)
  results.push(await measure(page, { mode, seconds, hz }))
  await page.close()
}
await browser.close()

const pick = (key) => results.map(r => r[key]).sort((a, b) => a - b)[Math.floor(results.length / 2)]
console.log(JSON.stringify({
  file,
  mode,
  hz: mode === 'stream' ? hz : null,
  runs: results.length,
  median: {
    fps: pick('fps'),
    busyRatio: pick('busyRatio'),
    frameP50: pick('frameP50'),
    frameP95: pick('frameP95'),
    frameMax: pick('frameMax'),
    longTasks: pick('longTasks'),
    longTaskMs: pick('longTaskMs'),
    domNodes: pick('domNodes'),
    flowItems: pick('flowItems'),
    scrollHeight: pick('scrollHeight'),
    streamUpdates: pick('streamUpdates'),
  },
  all: results,
}, null, 2))
