import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from 'react'

const LightboxContext = createContext<(src: string) => void>(() => {})

/** 调起灯箱查看原图：会话图片、气泡附件、Markdown 与生成图片共用 */
export function useLightbox() {
  return useContext(LightboxContext)
}

/** 全屏灯箱：点击遮罩或 Esc 关闭，置于所有弹窗之上 */
export function LightboxProvider({ children }: { children: ReactNode }) {
  const [src, setSrc] = useState<string | null>(null)
  const open = useCallback((next: string) => setSrc(next), [])
  const close = useCallback(() => setSrc(null), [])

  useEffect(() => {
    if (!src) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') close()
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [src, close])

  return (
    <LightboxContext.Provider value={open}>
      {children}
      {src && (
        <div className="lightbox-overlay" onClick={close}>
          <img src={src} alt="" />
        </div>
      )}
    </LightboxContext.Provider>
  )
}
