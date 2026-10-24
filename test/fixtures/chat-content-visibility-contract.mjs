/**
 * `chat_content_visibility` 补丁的 JavaScript 契约。
 *
 * 补丁在官方 `ChatView_module_css_default` 映射对象之后追加一段注入代码：用映射值拼出
 * 选择器，插入一条独立的 `<style id="inkdsh-content-visibility">`。这里用一个最小 `document`
 * 桩验证四条契约：
 *
 * 1. 注入的样式文本 = `.${flowItem}{content-visibility:auto;contain-intrinsic-size:auto 96px}`，
 *    即选择器跟着核心给的哈希类名走，而不是脚本里写死。
 * 2. 样式挂在 `document.head` 上、带固定 id（便于幂等判断与排查）。
 * 3. 重复执行不重复插入（官方组件热更 / 多次挂载都必须幂等）。
 * 4. `document` 不存在（Node / SSR 环境）时不抛错。
 *
 * 用法：node test/fixtures/chat-content-visibility-contract.mjs <patched-client.js>
 */

import { readFileSync } from 'node:fs'

const target = process.argv[2]
if (target === undefined) throw new Error('usage: node chat-content-visibility-contract.mjs <patched-client.js>')
const source = readFileSync(target, 'utf8')

/** 取出补丁插入的注入段（region 标记由补丁常量写入）。 */
function extractRegion(text) {
  const begin = text.indexOf('//#region inkdsh/chat-content-visibility')
  const end = text.indexOf('//#endregion', begin)
  if (begin < 0 || end < 0) throw new Error('injection region not found in patched source')
  return text.slice(begin, end)
}

/** 从源码里解析类名映射对象（不依赖哈希字面量）。 */
function extractClassMap(text) {
  const start = text.indexOf('var ChatView_module_css_default = {')
  if (start < 0) throw new Error('class map not found')
  const end = text.indexOf('};', start)
  const body = text.slice(start + 'var ChatView_module_css_default = '.length, end + 1)
  return JSON.parse(body.replace(/,(\s*})/, '$1'))
}

/** 造一个最小 document 桩，记录被创建与挂载的节点。 */
function createDocument() {
  const created = []
  const styles = []
  return {
    created,
    styles,
    createElement(tagName) {
      const node = { tagName, id: '', textContent: '' }
      created.push(node)
      return node
    },
    getElementById(id) {
      return styles.find(node => node.id === id) ?? null
    },
    head: {
      appendChild(node) {
        styles.push(node)
      },
    },
  }
}

const region = extractRegion(source)
const classes = extractClassMap(source)
const expected = `.${classes.flowItem}{content-visibility:auto;contain-intrinsic-size:auto 96px}`

// 1+2. 首次执行：插入一条正确的样式。
const doc = createDocument()
const run = new Function('document', 'ChatView_module_css_default', region)
run(doc, classes)
if (doc.styles.length !== 1) throw new Error(`expected exactly one style, got ${doc.styles.length}`)
const style = doc.styles[0]
if (style.tagName !== 'style') throw new Error(`expected a style element, got ${style.tagName}`)
if (style.id !== 'inkdsh-content-visibility') throw new Error(`unexpected style id: ${style.id}`)
if (style.textContent !== expected) {
  throw new Error(`unexpected css: ${style.textContent}\nexpected: ${expected}`)
}

// 3. 再执行一次：幂等，不重复插入。
run(doc, classes)
if (doc.styles.length !== 1) throw new Error('injection must be idempotent')

// 4. 无 document 环境：不抛错（补丁在 Node 侧被 import 时也必须安全）。
const sandbox = new Function('ChatView_module_css_default', `const document = undefined;\n${region}`)
sandbox(classes)

console.log(JSON.stringify({ ok: true, selector: `.${classes.flowItem}`, css: expected }))
