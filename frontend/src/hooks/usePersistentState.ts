import { useEffect, useRef, useState } from 'react'

/**
 * localStorage 持久化 state：初始值经 load 读取，变更经 store 序列化写回。
 * store 经 ref 间接引用，避免内联序列化函数导致逐渲染写盘。
 */
export function usePersistentState<T>(
  key: string,
  load: () => T,
  store: (value: T) => string,
): [T, React.Dispatch<React.SetStateAction<T>>] {
  const [value, setValue] = useState<T>(load)
  const storeRef = useRef(store)
  storeRef.current = store

  useEffect(() => {
    localStorage.setItem(key, storeRef.current(value))
  }, [key, value])

  return [value, setValue]
}

/** 布尔开关的 localStorage 形态（'1' / '0'）。 */
export function usePersistentFlag(key: string) {
  return usePersistentState(
    key,
    () => localStorage.getItem(key) === '1',
    value => (value ? '1' : '0'),
  )
}

/** 字符串集合的 localStorage 形态（JSON 数组，忽略损坏值与非字符串项）。 */
export function usePersistentSet(key: string) {
  return usePersistentState<Set<string>>(
    key,
    () => {
      try {
        const value: unknown = JSON.parse(localStorage.getItem(key) ?? '[]')
        return new Set(Array.isArray(value) ? value.filter(item => typeof item === 'string') : [])
      } catch {
        return new Set()
      }
    },
    value => JSON.stringify([...value]),
  )
}

/** 不可变地翻转集合成员（存在则移除，否则加入）。 */
export function toggled<T>(set: Set<T>, item: T): Set<T> {
  const next = new Set(set)
  if (!next.delete(item)) next.add(item)
  return next
}
