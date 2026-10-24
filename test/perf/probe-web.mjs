/**
 * 打开 DSH web UI 并 dump 当前状态（会话列表 / DOM 规模 / 控制台错误）。
 *
 * 用途：验证 fixture 会话能否被官方核心正常解析与渲染，作为 P2 测量的前置检查。
 *
 * 用法：node test/perf/probe-web.mjs <baseUrl> [screenshotPath]
 */
import { chromium } from 'playwright'

const baseUrl = process.argv[2]
const shot = process.argv[3]
const browser = await chromium.launch({ channel: 'msedge', headless: true })
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
const errors = []
page.on('console', m => { if (m.type() === 'error') errors.push(m.text().slice(0, 300)) })
page.on('pageerror', e => errors.push(`pageerror: ${String(e).slice(0, 300)}`))

await page.goto(baseUrl, { waitUntil: 'domcontentloaded' })
await page.waitForTimeout(7000)
console.log('title:', await page.title())
console.log('dom nodes:', await page.evaluate(() => document.querySelectorAll('*').length))
console.log('body:', (await page.locator('body').innerText()).slice(0, 500).replace(/\n+/g, ' | '))
console.log('errors:', errors.slice(0, 10))
if (shot) await page.screenshot({ path: shot, fullPage: false })
await browser.close()
