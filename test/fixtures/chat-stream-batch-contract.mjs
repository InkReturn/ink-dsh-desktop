/**
 * `chat_stream_batch` 补丁的 JavaScript 契约。
 *
 * 补丁在官方 `dsh-client-ui-chat/lib/client.js` 里插入 `useBatchedChatNode`，把节点订阅的
 * 渲染快照压到每 100ms 最多一次。这里用最小 React 运行时（useState/useRef/useEffect +
 * 假定时器）驱动这个函数，验证三条契约：
 *
 * 1. 初次渲染立刻给出当前节点（不延迟首帧）。
 * 2. 100ms 窗口内的连续 token 只提交一次，且提交的是**最后一个**值。
 * 3. 持续流式 500ms（每 10ms 一个 token）提交次数 ≤ 6（不是 debounce 那样被不断重置）。
 *
 * 用法：node test/fixtures/chat-stream-batch-contract.mjs <patched-client.js>
 */

import { readFileSync } from 'node:fs'

const target = process.argv[2]
if (target === undefined) throw new Error('usage: node chat-stream-batch-contract.mjs <patched-client.js>')
const source = readFileSync(target, 'utf8')

/** 取出补丁插入的缓冲实现（region 标记由补丁常量写入）。 */
function extractHelper(text) {
  const begin = text.indexOf('//#region inkdsh/chat-stream-batch')
  const end = text.indexOf('//#endregion', begin)
  if (begin < 0 || end < 0) throw new Error('helper region not found in patched source')
  return text.slice(begin, end)
}

/**
 * 造一个最小 React 运行时：hook 状态跨多次渲染保持，effect 的依赖变化照 React 的语义判断。
 *
 * @param helperSource 补丁插入的实现源码。
 * @returns 驱动对象：`setNode` 换 prop，`pump` 推进时钟，`renders/value` 观察结果。
 */
function createHarness(helperSource) {
  const slots = []
  const cleanups = new Map()
  let hookIndex = 0
  let pendingEffects = []
  let renders = 0
  // 只统计「由 setState 提交」的渲染：prop 驱动的那次重渲染是父组件行为，不属于批缓冲。
  let commits = 0
  let dirty = false
  let value
  let props = { node: undefined }
  let clock = 0
  let timerSeq = 0
  const timers = new Map()

  const react = {
    useState(initial) {
      const index = hookIndex++
      if (!(index in slots)) slots[index] = initial
      return [slots[index], (next) => {
        slots[index] = next
        dirty = true
      }]
    },
    useRef(initial) {
      const index = hookIndex++
      if (!(index in slots)) slots[index] = { current: initial }
      return slots[index]
    },
    useEffect(effect, deps) {
      const index = hookIndex++
      const previous = slots[index]
      const changed = previous === undefined
        || deps === undefined
        || deps.length !== previous.deps.length
        || deps.some((item, i) => item !== previous.deps[i])
      slots[index] = { deps }
      if (changed) pendingEffects.push({ index, effect })
    },
  }

  // 组件体就是 useBatchedChatNode(props.node)。
  const factory = new Function('react', 'setTimeout', 'clearTimeout', `${helperSource}\nreturn useBatchedChatNode`)
  const component = factory(
    react,
    (callback, delay) => {
      const id = ++timerSeq
      timers.set(id, { at: clock + delay, callback })
      return id
    },
    (id) => timers.delete(id),
  )

  /** 跑一次渲染：重置 hook 游标、算新值、再跑本轮 effect。 */
  const render = () => {
    hookIndex = 0
    dirty = false
    renders += 1
    value = component(props.node)
    for (const { index, effect } of pendingEffects.splice(0)) {
      cleanups.get(index)?.()
      const cleanup = effect()
      cleanups.set(index, typeof cleanup === 'function' ? cleanup : () => {})
    }
  }

  /** 推进时钟到 +ms，执行到期定时器，并在 setState 后补一次提交渲染。 */
  const pump = (ms) => {
    const deadline = clock + ms
    while (true) {
      let next
      for (const [id, timer] of timers) {
        if (timer.at <= deadline && (next === undefined || timer.at < next.timer.at)) next = { id, timer }
      }
      if (next === undefined) break
      clock = next.timer.at
      timers.delete(next.id)
      next.timer.callback()
      if (dirty) {
        // React 把 setState 后的组件重渲染安排在提交阶段：这里立刻补上。
        const node = props.node
        commits += 1
        render()
        props = { node }
      }
    }
    clock = deadline
  }

  return {
    setNode(node) {
      props = { node }
      render()
    },
    pump,
    get renders() { return renders },
    get commits() { return commits },
    get value() { return value },
    get timers() { return timers.size },
  }
}

const harness = createHarness(extractHelper(source))

// 1. 初次渲染：立刻是当前节点，且只渲染一次。
const nodeA = { id: 'A' }
harness.setNode(nodeA)
if (harness.value !== nodeA) throw new Error('first render must expose the current node immediately')
if (harness.renders !== 1) throw new Error(`first render must happen once, got ${harness.renders}`)

// 2. 100ms 窗口内连续三次更新：窗口内保持旧值，窗口结束提交最后一个。
harness.setNode({ id: 'B' })
harness.pump(10)
harness.setNode({ id: 'C' })
harness.pump(10)
harness.setNode({ id: 'D' })
harness.pump(70)
if (harness.value.id !== 'A') throw new Error(`window must hold renders, got ${harness.value.id}`)
harness.pump(15)
if (harness.value.id !== 'D') throw new Error(`latest value must commit, got ${harness.value.id}`)

// 3. 持续流式 500ms（每 10ms 一个 token）：提交次数受限，终态必达。
const before = harness.commits
for (let tick = 0; tick < 50; tick++) {
  harness.setNode({ id: `S${tick}` })
  harness.pump(10)
}
harness.pump(120)
const commits = harness.commits - before
if (commits > 6) throw new Error(`too many commits during streaming: ${commits}`)
if (harness.value.id !== 'S49') throw new Error(`final value must arrive, got ${harness.value.id}`)

console.log(JSON.stringify({ ok: true, commits, final: harness.value.id }))
