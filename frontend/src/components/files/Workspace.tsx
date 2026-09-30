import { useMemo, type ReactNode } from 'react'
import type { FileFocus, PreviewFile } from '../../hooks/usePanelLayout'
import type { ContentPart, RelatedFile, Session } from '../../types'
import { sessionDiffsFor } from '../../utils/changeMarks'
import { IconArrowLeft, IconLayoutIde, IconSearch, IconX } from '../icons'
import { cleanPath, relativePath } from '../trace/describe'
import { Explorer, isChanged, isMac, sessionFileFocus } from './Explorer'
import { Viewer } from './Viewer'

/** 文件区共享上下文：会话、涉及文件与当前文件定位 */
export interface FilesContext {
  session: Session
  draft: ContentPart[] | undefined
  files: RelatedFile[]
  activePath: string | null
  focus: FileFocus
  focusSeq: number
  searchSeq: number
  revealDir: { path: string; seq: number } | null
  onOpen: PreviewFile
  onReveal: (dir: string) => void
}

function relOf(path: string, workspace: string): string {
  return relativePath(cleanPath(path), workspace)
}

function useActiveFile(ctx: FilesContext) {
  const { session, draft, files, activePath } = ctx
  const rel = activePath ? relOf(activePath, session.workspace_root) : null
  const related = useMemo(() => (rel ? files.find(file => file.rel === rel) : undefined), [files, rel])
  // 仅在该文件有改动时计算差异，避免每个流式增量都全量遍历会话
  const changed = related != null && isChanged(related)
  const diffs = useMemo(
    () => (rel && changed ? sessionDiffsFor(session, draft, rel) : []),
    [session, draft, rel, changed],
  )
  return { related, diffs }
}

function ActiveViewer({ ctx, leading, trailing }: { ctx: FilesContext; leading?: ReactNode; trailing?: ReactNode }) {
  const { related, diffs } = useActiveFile(ctx)
  if (!ctx.activePath) return null
  return (
    <Viewer
      key={ctx.activePath}
      sessionId={ctx.session.id}
      workspace={ctx.session.workspace_root}
      path={ctx.activePath}
      focus={ctx.focus}
      focusSeq={ctx.focusSeq}
      related={related}
      diffs={diffs}
      leading={leading}
      trailing={trailing}
      onReveal={ctx.onReveal}
    />
  )
}

/**
 * 对话模式：右侧单列抽屉，在「浏览」与「文件」两视图间切换；
 * 两视图常驻 DOM，返回浏览时目录展开与搜索词保持原状。
 */
export function FileDrawer({
  ctx,
  open,
  width,
  view,
  onBack,
  onClose,
  onSwitchIde,
}: {
  ctx: FilesContext
  open: boolean
  width: number
  view: 'explorer' | 'file'
  onBack: () => void
  onClose: () => void
  onSwitchIde?: () => void
}) {
  const showFile = view === 'file' && ctx.activePath != null
  const close = (
    <button type="button" className="icon-ghost" onClick={onClose} title="关闭文件面板" aria-label="关闭文件面板">
      <IconX size={15} />
    </button>
  )
  return (
    <aside
      className="file-sidebar"
      style={{ width: `${width}px` }}
      aria-label="文件"
      inert={!open}
      aria-hidden={!open}
      onKeyDown={event => {
        if (event.key === 'Escape' && showFile) {
          event.stopPropagation()
          onBack()
        }
      }}
    >
      <div className="drawer-view" hidden={showFile}>
        <Explorer
          workspace={ctx.session.workspace_root}
          files={ctx.files}
          activePath={ctx.activePath}
          enabled={open}
          searchSeq={ctx.searchSeq}
          revealDir={ctx.revealDir}
          onOpen={ctx.onOpen}
          head={
            <div className="pane-head">
              <h2 className="pane-title">文件</h2>
              {onSwitchIde && (
                <button
                  type="button"
                  className="icon-ghost"
                  onClick={onSwitchIde}
                  title={`切换到 IDE 模式（${isMac() ? '⌘' : 'Ctrl+'}\\）`}
                  aria-label="切换到 IDE 模式"
                >
                  <IconLayoutIde size={16} />
                </button>
              )}
              {close}
            </div>
          }
        />
      </div>
      {showFile && (
        <div className="drawer-view">
          <ActiveViewer
            ctx={ctx}
            leading={
              <button type="button" className="icon-ghost" onClick={onBack} title="返回文件列表（Esc）" aria-label="返回文件列表">
                <IconArrowLeft size={15} />
              </button>
            }
            trailing={close}
          />
        </div>
      )}
    </aside>
  )
}

/** IDE 模式左栏：文件浏览 */
export function ExplorerPane({ ctx, width }: { ctx: FilesContext; width: number }) {
  return (
    <aside className="pane explorer-pane" style={{ width: `${width}px` }} aria-label="文件">
      <Explorer
        workspace={ctx.session.workspace_root}
        files={ctx.files}
        activePath={ctx.activePath}
        enabled
        searchSeq={ctx.searchSeq}
        revealDir={ctx.revealDir}
        onOpen={ctx.onOpen}
        head={
          <div className="pane-head">
            <h2 className="pane-title" title={ctx.session.workspace_root}>
              {ctx.session.workspace_root.split('/').filter(Boolean).pop() ?? ctx.session.workspace_root}
            </h2>
          </div>
        }
      />
    </aside>
  )
}

/** IDE 模式中栏：标签 + 文件查看；无文件时列出本会话改动作为起点 */
export function EditorPane({
  ctx,
  tabs,
  onSelectTab,
  onCloseTab,
}: {
  ctx: FilesContext
  tabs: string[]
  onSelectTab: (path: string) => void
  onCloseTab: (path: string) => void
}) {
  const workspace = ctx.session.workspace_root
  const activeRel = ctx.activePath ? relOf(ctx.activePath, workspace) : null
  const byRel = new Map(ctx.files.map(file => [file.rel, file]))
  const changed = ctx.files.filter(isChanged)
  return (
    <section className="pane editor-pane" aria-label="编辑区">
      {tabs.length > 0 && (
        <div className="tabs" role="tablist" aria-label="打开的文件">
          {tabs.map(path => {
            const rel = relOf(path, workspace)
            const name = rel.split('/').pop() ?? rel
            const mark = byRel.get(rel)
            const active = rel === activeRel
            return (
              <div
                key={path}
                role="tab"
                aria-selected={active}
                tabIndex={0}
                className={`tab${active ? ' active' : ''}`}
                title={rel}
                onClick={() => onSelectTab(path)}
                onAuxClick={event => {
                  if (event.button === 1) onCloseTab(path)
                }}
                onKeyDown={event => {
                  if (event.key === 'Enter' || event.key === ' ') {
                    event.preventDefault()
                    onSelectTab(path)
                  }
                }}
              >
                {mark && isChanged(mark) && <span className="glyph change" aria-label="本会话修改" />}
                <span className="tab-name">{name}</span>
                <button
                  type="button"
                  className="tab-close"
                  onClick={event => {
                    event.stopPropagation()
                    onCloseTab(path)
                  }}
                  aria-label={`关闭 ${name}`}
                >
                  <IconX size={11} strokeWidth={2.2} />
                </button>
              </div>
            )
          })}
        </div>
      )}
      {ctx.activePath ? (
        <ActiveViewer ctx={ctx} />
      ) : (
        <div className="editor-empty">
          <p className="editor-empty-title">
            <IconSearch size={15} /> 按 <kbd>{isMac() ? '⌘P' : 'Ctrl P'}</kbd> 跳转到文件，或从左侧目录打开
          </p>
          {changed.length > 0 && (
            <div className="editor-empty-list">
              <p className="editor-empty-label">本次会话改动的文件</p>
              {changed.slice(0, 8).map(file => (
                <button key={file.rel} type="button" className="x-row" onClick={() => ctx.onOpen(file.path, sessionFileFocus(file))}>
                  <span className="glyph change" aria-hidden />
                  <span className="x-name">{file.name}</span>
                  <span className="x-dir">{file.dir}</span>
                  <span className="x-meta">
                    <span className="diff-add">+{file.plus}</span>
                    <span className="diff-del">−{file.minus}</span>
                  </span>
                </button>
              ))}
            </div>
          )}
        </div>
      )}
    </section>
  )
}
