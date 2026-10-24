/**
 * `content-visibility` 的两个专项观测（不进验收门槛，只作已知问题与互扰守门）。
 *
 * 1. `--mode stream-insert`：模拟流式新消息插入，逐帧检查新行是否**立刻**有真实高度、
 *    是否落在可视区、滚动位置漂移多少。这是补丁 A（压流式提交频率）与补丁 B
 *    （`content-visibility` 跳过屏外渲染）唯一可能互相干扰的地方：
 *    个别 WebView2 版本对动态插入节点的 `content-visibility` 会「先白一帧再渲染」。
 * 2. `--mode scrollbar-jump`：`contain-intrinsic-size` 只有 `auto` 记忆已渲染过的行，
 *    从未渲染的屏外行按 96px 估算。拖动滚动条到未知区域时，总高会被重算 → 位置跳动。
 *    这里量「跳转后 scrollHeight 变化量」与「视口内容位移」作为已知问题数值。
 *
 * 用法：
 *   node test/perf/cv-effects.mjs --file test/perf/out/fixture-2000-cv.html --mode stream-insert
 *   node test/perf/cv-effects.mjs --file test/perf/out/fixture-2000.html --mode scrollbar-jump
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

const args = parseArgs(process.argv.slice(2))
const file = resolve(args.file ?? 'test/perf/out/fixture-2000-cv.html')
const mode = args.mode ?? 'stream-insert'
const rounds = Number(args.rounds ?? 20)

const browser = await chromium.launch({ channel: 'msedge', headless: true })
const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 })
await page.goto(pathToFileURL(file).href, { waitUntil: 'load' })
await page.waitForTimeout(500)

const result = mode === 'stream-insert'
  ? await page.evaluate(async (rounds) => {
    const scroller = document.querySelector('[data-conversation-scroll]')
    const column = document.querySelector('[data-chat-flow]')
    const flowClass = [...column.children][0]?.className ?? ''
    // 1. 先滚到底部，模拟「正在看最新输出」的位置。
    scroller.scrollTop = scroller.scrollHeight - scroller.clientHeight
    await new Promise(r => requestAnimationFrame(r))
    const frames = []
    for (let i = 0; i < rounds; i++) {
      // 2. 插入一条新消息（等价于流式刚冒出来的那一行），内容足够撑起真实高度。
      const node = document.createElement('div')
      node.className = flowClass
      node.setAttribute('data-chat-flow-key', JSON.stringify(['node', `live-${i}`, null]))
      node.innerHTML = `<div data-slot="conversation.chat.node"><h2>实时第 ${i} 条</h2>${'<p>流式插入的内容，用于验证可视区不会先白一帧。</p>'.repeat(4)}</div>`
      column.appendChild(node)
      // 3. 下一帧立刻观测：新行高度、是否在可视区、滚动漂移。
      const rect = await new Promise(r => requestAnimationFrame(() => r(node.getBoundingClientRect())))
      const viewportBottom = scroller.scrollTop + scroller.clientHeight
      frames.push({
        height: Math.round(rect.height),
        inViewport: rect.top < scroller.clientHeight && rect.bottom > 0,
        blank: rect.height < 50,
        scrollDrift: Math.round(Math.abs(scroller.scrollTop + scroller.clientHeight - scroller.scrollHeight)),
        atBottom: Math.round(viewportBottom - (scroller.scrollTop + scroller.clientHeight)),
      })
    }
    return {
      mode: 'stream-insert',
      rounds,
      blankFrames: frames.filter(f => f.blank).length,
      minHeight: Math.min(...frames.map(f => f.height)),
      maxHeight: Math.max(...frames.map(f => f.height)),
      inViewportFrames: frames.filter(f => f.inViewport).length,
      maxScrollDrift: Math.max(...frames.map(f => f.scrollDrift)),
      frames: frames.slice(0, 5),
    }
  }, rounds)
  : await page.evaluate(async () => {
    const scroller = document.querySelector('[data-conversation-scroll]')
    const items = () => [...document.querySelectorAll('[data-chat-flow-key]')]
    // 1. 记录初始总高（全部按真实高度或估算值给出）。
    const before = { scrollHeight: scroller.scrollHeight, max: scroller.scrollTop }
    // 2. 跳到中部（模拟拖滚动条到从未渲染的区域）。
    scroller.scrollTop = Math.floor((scroller.scrollHeight - scroller.clientHeight) * 0.5)
    await new Promise(r => requestAnimationFrame(r))
    const afterJump = scroller.scrollHeight
    // 3. 等几帧让内容落地，再读总高变化。
    await new Promise(r => setTimeout(r, 300))
    const settled = scroller.scrollHeight
    const heights = items().map(el => el.getBoundingClientRect().height)
    const rendered = heights.filter(h => h > 0)
    return {
      mode: 'scrollbar-jump',
      items: heights.length,
      scrollHeightBefore: before.scrollHeight,
      scrollHeightAfterJump: afterJump,
      scrollHeightSettled: settled,
      jumpRatio: Math.round(((before.scrollHeight - settled) / before.scrollHeight) * 1000) / 1000,
      jumpPx: before.scrollHeight - settled,
      renderedItems: rendered.length,
      medianRenderedHeight: rendered.length === 0 ? 0 : Math.round([...rendered].sort((a, b) => a - b)[Math.floor(rendered.length / 2)]),
    }
  })

console.log(JSON.stringify({ file, ...result }, null, 2))
await browser.close()
