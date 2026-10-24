import { chromium } from 'playwright'
const url = process.argv[2]
const browser = await chromium.launch({ channel: 'msedge', headless: true })
const page = await browser.newPage()
const urls = []
page.on('request', r => { if (r.url().includes('/plugins/??')) urls.push(decodeURIComponent(r.url())) })
await page.goto(url, { waitUntil: 'domcontentloaded' })
await page.waitForTimeout(8000)
const all = urls.join(',').split(',').map(s => s.trim())
const wanted = (process.argv[3] ?? "aqua").split(",")
console.log('plugin client count:', all.length)
for (const w of wanted) console.log(w.padEnd(20), all.some(u => u.toLowerCase().includes(w)) ? 'OK' : '-')
await browser.close()
