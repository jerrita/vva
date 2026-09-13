import { useEffect, useState } from 'react'
import { api } from '../api'

function toError(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/** 全局配置不存在时的起步模板 */
const EMPTY_TEMPLATE = `{
  // 默认模型规格 (provider/model)
  "settings": { "default": "openai/gpt-5.6-terra" },
  "providers": {}
}
`

export function SettingsModal({
  onClose,
  onSaved,
}: {
  onClose: () => void
  onSaved: () => void
}) {
  const [path, setPath] = useState('')
  const [content, setContent] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    api
      .getConfig()
      .then(info => {
        setPath(info.path)
        setContent(info.content ?? EMPTY_TEMPLATE)
      })
      .catch(e => setError(toError(e)))
      .finally(() => setLoading(false))
  }, [])

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [onClose])

  const save = async () => {
    if (content === null || saving) return
    setSaving(true)
    setError(null)
    try {
      await api.saveConfig(content)
      onSaved()
      onClose()
    } catch (e) {
      setError(toError(e))
      setSaving(false)
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="settings-modal" onClick={event => event.stopPropagation()}>
        <div className="settings-head">
          <div className="confirm-title">Settings</div>
          <span className="settings-path" title={path}>
            {path}
          </span>
        </div>
        {error && <div className="settings-error">{error}</div>}
        {loading ? (
          <div className="settings-loading">加载中…</div>
        ) : (
          <textarea
            className="settings-textarea"
            value={content ?? ''}
            onChange={event => setContent(event.target.value)}
            spellCheck={false}
            autoFocus
          />
        )}
        <div className="confirm-actions">
          <button className="ghost-button" onClick={onClose}>
            取消
          </button>
          <button className="new-create" disabled={saving || loading} onClick={save}>
            {saving ? '保存中…' : '保存'}
          </button>
        </div>
      </div>
    </div>
  )
}
