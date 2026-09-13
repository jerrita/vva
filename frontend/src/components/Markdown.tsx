import { Children, isValidElement, memo, type ReactNode } from 'react'
import ReactMarkdown from 'react-markdown'
import rehypeHighlight from 'rehype-highlight'
import remarkGfm from 'remark-gfm'
import { CopyButton } from './CopyButton'

function childText(node: ReactNode): string {
  if (node == null || typeof node === 'boolean') return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(childText).join('')
  if (isValidElement(node)) return childText((node.props as { children?: ReactNode }).children)
  return ''
}

/** pre → macOS 窗口：圆点标题栏（语言名 + 复制）+ 高亮代码体 */
function CodeWindow({ children }: { children?: ReactNode }) {
  const code = Children.toArray(children).find(isValidElement)
  const className = ((code?.props ?? {}) as { className?: unknown }).className
  const lang =
    typeof className === 'string' ? /language-([\w#+.-]+)/.exec(className)?.[1] : undefined
  return (
    <div className="code-window">
      <div className="code-window-bar">
        <span className="code-window-dots" aria-hidden>
          <i />
          <i />
          <i />
        </span>
        {lang && <span className="code-window-title">{lang}</span>}
        <span className="code-window-actions">
          <CopyButton text={childText(children)} />
        </span>
      </div>
      <pre>{children}</pre>
    </div>
  )
}

export const Markdown = memo(function Markdown({ text }: { text: string }) {
  return (
    <div className="markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeHighlight]}
        components={{
          a: props => <a {...props} target="_blank" rel="noreferrer" />,
          pre: CodeWindow,
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  )
})
