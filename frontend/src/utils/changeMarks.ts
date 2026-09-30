import type { FileDiff } from './editDiff'

export { sessionDiffsFor } from './relatedFiles'

/** 至少 4 个字符且不全是标点/括号才算有辨识度的单行。 */
function meaningful(line: string): boolean {
  const t = line.trim()
  return t.length >= 4 && /[\p{L}\p{N}]/u.test(t)
}

/** 本会话对该文件的改动在当前内容中的落点：行号(1-based)集合 */
export function changedLines(content: string, diffs: FileDiff[]): Set<number> {
  const marks = new Set<number>()
  const fileLines = content.replace(/\r\n/g, '\n').split('\n').map(l => l.trimEnd())
  const index = new Map<string, number[]>()
  fileLines.forEach((l, i) => {
    const arr = index.get(l)
    if (arr) arr.push(i)
    else index.set(l, [i])
  })

  // replace 模式的 old/new 常为行内片段：首行可为文件行的后缀、末行可为前缀、单行可为子串，中间行需完全一致
  const matchAt = (block: string[], start: number): boolean => {
    if (start + block.length > fileLines.length) return false
    if (block.length === 1) return fileLines[start].includes(block[0])
    const last = block.length - 1
    if (!fileLines[start].endsWith(block[0])) return false
    for (let k = 1; k < last; k++) if (fileLines[start + k] !== block[k]) return false
    return fileLines[start + last].startsWith(block[last])
  }

  for (const diff of diffs) {
    const blocks: string[][] = []
    if (diff.op === 'add') {
      const all = diff.lines.filter(l => l.kind === 'add').map(l => l.text.trimEnd())
      if (all.length) blocks.push(all)
    } else {
      let cur: string[] = []
      for (const l of diff.lines) {
        if (l.kind === 'add') cur.push(l.text.trimEnd())
        else if (cur.length) {
          blocks.push(cur)
          cur = []
        }
      }
      if (cur.length) blocks.push(cur)
    }

    let cursor = 0
    for (const block of blocks) {
      if (block.length === 1 && !meaningful(block[0])) continue
      // 优先从上一块之后查找，再回绕到文件开头
      let hit: number | undefined
      for (let offset = 0; offset < fileLines.length && hit === undefined; offset++) {
        const start = (cursor + offset) % fileLines.length
        if (matchAt(block, start)) hit = start
      }
      if (hit === undefined) {
        // 整块已被后续改动打散：逐行回落，仅标记在文件中唯一出现的有辨识度行
        for (const line of block) {
          const at = index.get(line)
          if (at?.length === 1 && meaningful(line)) marks.add(at[0] + 1)
        }
        continue
      }
      for (let k = 0; k < block.length; k++) marks.add(hit + k + 1)
      cursor = hit + block.length
    }
  }
  return marks
}

/** 首个改动行号；无改动返回 null。 */
export function firstChangedLine(marks: Set<number>): number | null {
  let min: number | null = null
  for (const n of marks) if (min === null || n < min) min = n
  return min
}
