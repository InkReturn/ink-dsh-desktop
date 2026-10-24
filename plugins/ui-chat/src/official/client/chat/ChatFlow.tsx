/** Ordered Chat rows, local echoes, and running status share the flow slot's parent. */
import { memo, useEffect, useRef, useState } from 'react'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import type { ChatFlowSlotProps } from '../contract/slots.ts'
import { BatchBuffer } from '../../../virtual.js'
import { ChatNodeSeat } from './ChatNodeSeat.tsx'
import { ChatGroupSeat } from './ChatGroupSeat.tsx'
import { PendingSteeringBubble, PendingSubmissionBubble } from './MessageItem.tsx'
import { chatRenderKey } from './render-entry.ts'
import { RunningStatus } from './RunningStatus.tsx'

/** 流式节点提交间隔；与 inkdsh 的批量策略保持一致。 */
const FLOW_BATCH_MS = 100

/**
 * 把官方按事件即时重建的 flow 条目压到 100ms 一批。
 *
 * 官方 `entries` 在每个 token/事件后都会换成新数组；直接用会让整棵 flow 以
 * 事件频率重渲染。这里只延迟“渲染快照”，不改变 order/scroll/navigation
 * 等上层业务状态，因此滚动跟随、历史加载与 Turn 导航仍由官方逻辑驱动。
 *
 * @param entries 官方按提交顺序给出的渲染条目。
 * @returns 最近一次批量提交的条目数组。
 */
function useBatchedEntries(entries: ChatFlowSlotProps['entries']): ChatFlowSlotProps['entries'] {
  const [visible, setVisible] = useState(entries)
  const buffer = useRef<BatchBuffer<ChatFlowSlotProps['entries']> | null>(null)
  if (buffer.current === null) buffer.current = new BatchBuffer(next => setVisible(next), FLOW_BATCH_MS)
  useEffect(() => {
    buffer.current?.push(entries)
  }, [entries])
  useEffect(() => () => {
    buffer.current?.flushNow()
    buffer.current?.dispose()
  }, [])
  return visible
}

/** Render the flow without adding a DOM parent or changing keyed row positions. */
export const ChatFlow = memo(function ChatFlow({
  entries, pendingInputs, lastInputTurn, deferCollapse,
  useSession, useChat, useChatNode, useChatNodeBottom, useChatNodeProcess, useChatGroup, usePresentation,
  useStore, actions, renderSlot, renderFactorySlot, t, useGroupAction, useGroupHeaderAction,
  cwd, openFile, inspectCall, forkAt, loadImage, fileMentions,
}: ChatFlowSlotProps) {
  const nodeStore = useChat(snapshot => snapshot.nodes)
  const running = useSession(snapshot => snapshot.running)
  const latestTurnAnchor = useChat(snapshot => snapshot.navigation.items().at(-1)?.anchorKey)
  const runningStartTime = useChatNode(latestTurnAnchor ?? '', (node) => {
    const location = node?.location
    return location?.kind === 'turn' || location?.kind === 'step'
      ? location.turn.status === 'open' ? location.turn.start?.time : undefined
      : undefined
  })
  const seatProps = {
    nodeStore, useChatNode, useChatNodeBottom, useChatNodeProcess, usePresentation,
    useStore, actions, renderSlot, t, useGroupAction, deferCollapse,
    cwd, openFile, inspectCall, forkAt, loadImage, fileMentions,
  }
  const visibleEntries = useBatchedEntries(entries)
  const rows = visibleEntries.map((entry) => {
    switch (entry.kind) {
      case 'node':
        return <ChatNodeSeat {...seatProps} key={chatRenderKey(entry)} nodeKey={entry.key}
          {...entry.groupPart === undefined ? {} : { groupPart: entry.groupPart }} />
      case 'group':
        return <ChatGroupSeat {...seatProps} key={chatRenderKey(entry)} groupKey={entry.key}
          useChatGroup={useChatGroup} useGroupHeaderAction={useGroupHeaderAction} />
      default:
        return assertNever(entry)
    }
  })
  const pendingRows = pendingInputs.map(item => 'requestId' in item ? (
    <PendingSubmissionBubble key={item.requestId} submission={item}
      loadImage={loadImage} renderFactorySlot={renderFactorySlot} t={t} />
  ) : (
    <PendingSteeringBubble key={item.id} content={item.content}
      loadImage={loadImage} renderFactorySlot={renderFactorySlot} t={t} />
  ))
  const tail = visibleEntries.at(-1)
  const node = tail?.kind === 'node' ? nodeStore.get(tail.key) : undefined
  // An empty opening control follows one local transcript echo, never steering.
  // All rows share this keyed list so inserting the control keeps the echo mounted.
  if (node?.kind === 'turn-process' && node.location.kind === 'turn'
    && node.location.turn.status === 'open' && node.location.turn.turn !== lastInputTurn) {
    const index = pendingInputs.findIndex(item => 'requestId' in item && item.placement === 'transcript')
    if (index !== -1) rows.splice(rows.length - 1, 0, ...pendingRows.splice(index, 1))
  }
  return [...rows, ...pendingRows, ...running ? [<RunningStatus key="running" startTime={runningStartTime} t={t} />] : []]
})
