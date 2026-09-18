import "katex/dist/katex.min.css";
import { Children, isValidElement, memo, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import rehypeKatex from "rehype-katex";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import { normalizeMathDelimiters } from "../utils/math";
import { CopyButton } from "./CopyButton";

function childText(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(childText).join("");
  if (isValidElement(node))
    return childText((node.props as { children?: ReactNode }).children);
  return "";
}

/** pre → macOS 窗口：圆点标题栏（语言名 + 复制）+ 高亮代码体；展示公式退回普通 pre */
function CodeWindow({ children }: { children?: ReactNode }) {
  const code = Children.toArray(children).find(isValidElement);
  if (typeof code?.type !== "string" || code.type !== "code") {
    return <pre className="math-pre">{children}</pre>;
  }
  const className = ((code?.props ?? {}) as { className?: unknown }).className;
  const lang =
    typeof className === "string"
      ? /language-([\w#+.-]+)/.exec(className)?.[1]
      : undefined;
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
  );
}

export const Markdown = memo(function Markdown({ text }: { text: string }) {
  return (
    <div className="markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[
          rehypeHighlight,
          [rehypeKatex, { throwOnError: false }]
        ]}
        components={{
          a: (props) => <a {...props} target="_blank" rel="noreferrer" />,
          pre: CodeWindow
        }}
      >
        {normalizeMathDelimiters(text)}
      </ReactMarkdown>
    </div>
  );
});
