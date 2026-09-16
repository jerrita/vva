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
export function loadFlag(key: string): () => boolean {
  return () => localStorage.getItem(key) === '1'
}

export function storeFlag(value: boolean): string {
  return value ? '1' : '0'
}
