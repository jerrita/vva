import { useCallback, useEffect, useRef, useState } from 'react'
import { api, toError } from '../api'
import type { FsEntry } from '../types'

/**
 * 工程文件树状态：根目录加载、子目录懒加载缓存、展开集合与路径揭示。
 * 目录请求以 pending 集合去重；workspace 变化时整体重置，避免跨工程缓存串味。
 */
export function useFileTree(workspace: string | null | undefined, enabled: boolean) {
  const [rootEntries, setRootEntries] = useState<FsEntry[]>([])
  const [rootLoading, setRootLoading] = useState(false)
  const [rootError, setRootError] = useState<string | null>(null)
  const [dirChildren, setDirChildren] = useState<Record<string, FsEntry[]>>({})
  const [dirLoading, setDirLoading] = useState<Record<string, boolean>>({})
  const [expandedDirs, setExpandedDirs] = useState<Set<string>>(new Set())

  const childrenRef = useRef<Record<string, FsEntry[]>>({})
  const pending = useRef(new Set<string>())

  // workspace 变化（切换会话/工程）时重置树缓存与展开状态
  useEffect(() => {
    childrenRef.current = {}
    setDirChildren({})
    setExpandedDirs(new Set())
  }, [workspace])

  // 加载工程根目录条目
  useEffect(() => {
    if (!enabled) return
    let cancelled = false
    setRootLoading(true)
    setRootError(null)
    api
      .fsEntries(workspace || undefined)
      .then(res => {
        if (!cancelled) {
          setRootEntries(res.entries)
          setRootLoading(false)
        }
      })
      .catch(err => {
        if (!cancelled) {
          setRootError(toError(err))
          setRootLoading(false)
        }
      })
    return () => {
      cancelled = true
    }
  }, [enabled, workspace])

  const fetchDir = useCallback(
    (path: string) => {
      if (childrenRef.current[path] || pending.current.has(path)) return
      pending.current.add(path)
      setDirLoading(prev => ({ ...prev, [path]: true }))
      api
        .fsEntries(workspace || undefined, path)
        .then(res => {
          childrenRef.current = { ...childrenRef.current, [path]: res.entries }
          setDirChildren(childrenRef.current)
        })
        .catch(err => {
          console.error('Failed to load directory entries:', err)
        })
        .finally(() => {
          pending.current.delete(path)
          setDirLoading(prev => ({ ...prev, [path]: false }))
        })
    },
    [workspace],
  )

  const expandDir = useCallback(
    (path: string) => {
      setExpandedDirs(prev => (prev.has(path) ? prev : new Set([...prev, path])))
      fetchDir(path)
    },
    [fetchDir],
  )

  const toggleDir = useCallback(
    (path: string) => {
      setExpandedDirs(prev => {
        const next = new Set(prev)
        if (next.has(path)) {
          next.delete(path)
        } else {
          next.add(path)
        }
        return next
      })
      fetchDir(path)
    },
    [fetchDir],
  )

  // 揭示路径：展开全部上级目录并预取条目
  const revealPath = useCallback(
    (path: string) => {
      const parts = path.split('/').filter(Boolean)
      if (parts.length <= 1) return
      let cur = ''
      for (let i = 0; i < parts.length - 1; i++) {
        cur = cur ? `${cur}/${parts[i]}` : parts[i]
        expandDir(cur)
      }
    },
    [expandDir],
  )

  return {
    rootEntries,
    rootLoading,
    rootError,
    dirChildren,
    dirLoading,
    expandedDirs,
    toggleDir,
    expandDir,
    revealPath,
  }
}
