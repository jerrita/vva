import type { ContentPart, RelatedFile, Session } from '../types'

/** 剥离模型输出路径外层包裹的引号。 */
function cleanPath(raw: string): string {
  let p = raw.trim()
  while ((p.startsWith('"') && p.endsWith('"')) || (p.startsWith("'") && p.endsWith("'"))) {
    p = p.slice(1, -1).trim()
  }
  return p
}

/**
 * 从会话轮次与当前草稿中提取涉及的文件（Read / Write / Edit / 附件）
 */
export function extractRelatedFiles(
  session: Session | null,
  draft?: ContentPart[],
): RelatedFile[] {
  if (!session) return []
  const map = new Map<
    string,
    {
      path: string
      name: string
      actions: Set<'read' | 'write' | 'edit' | 'attachment'>
      turnIndex: number
    }
  >()

  const record = (
    path: string,
    action: 'read' | 'write' | 'edit' | 'attachment',
    turnIndex: number,
  ) => {
    const cleaned = cleanPath(path)
    if (!cleaned) return
    const name = cleaned.split('/').filter(Boolean).pop() || cleaned
    const existing = map.get(cleaned)
    if (existing) {
      existing.actions.add(action)
      existing.turnIndex = Math.max(existing.turnIndex, turnIndex)
    } else {
      map.set(cleaned, {
        path: cleaned,
        name,
        actions: new Set([action]),
        turnIndex,
      })
    }
  }

  const processParts = (parts: ContentPart[], turnIndex: number) => {
    for (const part of parts) {
      if (part.type === 'tool_call') {
        if (part.name === 'Read' || part.name === 'Write' || part.name === 'Edit') {
          try {
            const args = JSON.parse(part.arguments)
            if (args && typeof args === 'object' && typeof args.path === 'string') {
              record(
                args.path,
                part.name.toLowerCase() as 'read' | 'write' | 'edit',
                turnIndex,
              )
            }
          } catch {}
        }
      } else if (part.type === 'file') {
        if (part.filename) {
          record(part.filename, 'attachment', turnIndex)
        }
      }
    }
  }

  session.turns.forEach((turn, turnIndex) => {
    for (const msg of turn.messages) {
      processParts(msg.parts, turnIndex)
    }
  })

  if (draft && draft.length > 0) {
    processParts(draft, session.turns.length)
  }

  const files = [...map.values()].map(item => ({
    path: item.path,
    name: item.name,
    actions: [...item.actions],
    turnIndex: item.turnIndex,
  }))

  files.sort((a, b) => b.turnIndex - a.turnIndex || a.name.localeCompare(b.name))
  return files
}
