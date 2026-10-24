/**
 * 生成聊天滚动性能 fixture（真实 CSS + 真实 DOM 形状）。
 *
 * 为什么这么做：官方没有现成的「万级 DOM 会话」样本，而 `dsh web` 打开一个
 * 磁盘上的会话需要可路由模型（没有 provider 时 `agentAvailable:false`，聊天区
 * 不渲染）。所以基线测量只复现**渲染层**：样式表与 DOM 结构直接取自官方安装产物
 * `@deepseek-ai/dsh-client-ui-chat/lib/client.js`，内容用等效 markdown 块撑起 DOM 量。
 *
 * 关键点：
 * 1. CSS 是运行时注入的 `<style data-plugin-css=…ChatView.module.css>`，正文即
 *    bundle 里的 `css$16` 字符串 —— 直接抄过来，保证与线上逐字节一致。
 * 2. 类名来自 CSS Modules 映射对象 `ChatView_module_css_default` —— 从 bundle 解析，
 *    不在脚本里硬编码哈希，核心升级后仍能跑。
 * 3. `content-visibility` 由 `--cv` 开关控制，用来对比补丁前后。
 *
 * 用法：
 *   node test/perf/make-fixture.mjs --items 300 [--out test/perf/out/fixture.html]
 */

import { readFileSync, mkdirSync, writeFileSync } from 'node:fs'
import { homedir } from 'node:os'
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

/**
 * 定位官方 chat 包里的 `lib/client.js`（优先用 dsh 全局安装里的核心副本）。
 *
 * @returns 官方产物绝对路径。
 */
function locateChatBundle() {
  const candidates = [
    join(homedir(), 'AppData/Roaming/npm/node_modules/@deepseek-ai/dsh/node_modules/@deepseek-ai/dsh-client-ui-chat/lib/client.js'),
    join(homedir(), '.dsh.dev/profiles/web/node_modules/@deepseek-ai/dsh-client-ui-chat/lib/client.js'),
  ]
  for (const path of candidates) {
    try {
      readFileSync(path)
      return path
    }
    catch {}
  }
  throw new Error('cannot locate @deepseek-ai/dsh-client-ui-chat/lib/client.js')
}

/**
 * 从 bundle 里抽出 chat 的 CSS 正文（运行时注入的那一份）。
 *
 * @param source bundle 全文。
 * @returns CSS 文本。
 */
function extractChatCss(source) {
  const re = /const css\$\d+ = ("(?:[^"\\]|\\.)*");/g
  let match
  while ((match = re.exec(source)) !== null) {
    const literal = match[1]
    if (literal.includes('data-chat-flow') && literal.includes('flowItem')) {
      return JSON.parse(literal)
    }
  }
  throw new Error('chat css literal not found')
}

/**
 * 从 bundle 里抽出 CSS Modules 类名映射对象。
 *
 * @param source bundle 全文。
 * @returns `{ flowItem: 'EvIC1a_flowItem', … }` 形式的映射。
 */
function extractClassMap(source) {
  const start = source.indexOf('var ChatView_module_css_default = {')
  if (start < 0) throw new Error('class map not found')
  const end = source.indexOf('};', start)
  const body = source.slice(start + 'var ChatView_module_css_default = '.length, end + 1)
  return JSON.parse(body.replace(/,(\s*})/, '$1'))
}

/**
 * 造一条消息的 markdown 等价 DOM 片段（标题/段落/列表/表格/代码块/引用）。
 *
 * @param index 消息序号。
 * @returns HTML 片段。
 */
function messageHtml(index) {
  const para = `第 ${index} 条：这一段用来撑起文本节点数量，覆盖换行与行内代码 \`retry(${index})\`，以及中英文混排宽度计算。`
  const list = Array.from({ length: 4 }, (_, i) => `<li>项 ${i + 1}：这一行是列表项，长度中等，用于模拟真实回复的结构</li>`).join('')
  const rows = Array.from({ length: 3 }, (_, i) => `<tr><th>指标 ${i + 1}</th><td>${40 + i}</td><td>${55 + i}</td></tr>`).join('')
  const code = Array.from({ length: 12 }, (_, i) => `const value${i} = compute(${index}, ${i}, { retries: ${(i % 3) + 1} })`).join('\n')
  return [
    `<h2>第 ${index} 轮分析</h2>`,
    `<p>${para}</p>`,
    `<h3>待办</h3><ul>${list}</ul>`,
    `<p>${para}</p>`,
    `<table><thead><tr><th>指标</th><th>改前</th><th>改后</th></tr></thead><tbody>${rows}</tbody></table>`,
    `<pre><code>${code}</code></pre>`,
    `<blockquote><p>${para}</p></blockquote>`,
    `<p>${para}${para}</p>`,
  ].join('\n')
}

/**
 * 生成整页 HTML。
 *
 * @param options 生成参数。
 * @returns 完整 HTML 文本。
 */
function buildHtml({ css, classes, items, withContentVisibility }) {
  const extraCss = withContentVisibility
    ? `.${classes.flowItem}{content-visibility:auto;contain-intrinsic-size:auto 96px}`
    : ''
  const flow = Array.from({ length: items }, (_, index) => {
    const key = JSON.stringify(['node', `key-${index}`, null])
    return `<div class="${classes.flowItem}" data-chat-anchor-key='${key}' data-chat-flow-key='${key}' data-chat-node-key="key-${index}" data-chat-flow-kind="assistant">
  <div data-slot="conversation.chat.node">${messageHtml(index)}</div>
</div>`
  }).join('\n')

  return `<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>inkdsh perf fixture</title>
<style>${css}</style>
<style>${extraCss}
html,body{margin:0;height:100%;background:#fff;font-family:system-ui,-apple-system,"Segoe UI",sans-serif}
[data-conversation-scroll]{height:100vh;overflow-y:auto}
</style></head>
<body>
<div data-conversation-scroll>
  <div class="${classes.scroll}">
    <div class="${classes.column}" data-chat-flow>
${flow}
    </div>
  </div>
</div>
</body></html>`
}

const args = parseArgs(process.argv.slice(2))
const items = Number(args.items ?? 300)
const out = args.out ?? join(import.meta.dirname, 'out', `fixture-${items}${process.argv.includes('--cv') ? '-cv' : ''}.html`)
const bundle = locateChatBundle()
const source = readFileSync(bundle, 'utf8')
const css = extractChatCss(source)
const classes = extractClassMap(source)
const html = buildHtml({ css, classes, items, withContentVisibility: process.argv.includes('--cv') })
mkdirSync(dirname(out), { recursive: true })
writeFileSync(out, html)
console.log(JSON.stringify({ bundle, out, items, bytes: html.length, flowItemClass: classes.flowItem }, null, 2))
