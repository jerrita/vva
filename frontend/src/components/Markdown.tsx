import "katex/dist/katex.min.css";
import { Children, isValidElement, memo, type ReactNode } from "react";
import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import rehypeKatex from "rehype-katex";
import rehypeHighlight from "rehype-highlight";
import remarkCjkFriendly from "remark-cjk-friendly";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import { normalizeMathDelimiters } from "../utils/math";
import { CopyButton } from "./CopyButton";
import { useLightbox } from "./Lightbox";

function childText(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(childText).join("");
  if (isValidElement(node))
    return childText((node.props as { children?: ReactNode }).children);
  return "";
}

/** pre → 代码块：细标题栏（语言名 + 复制）+ 高亮代码体；展示公式退回普通 pre */
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
        <span className="code-window-title">{lang ?? "code"}</span>
        <span className="code-window-actions">
          <CopyButton text={childText(children)} />
        </span>
      </div>
      <pre>{children}</pre>
    </div>
  );
}

/** Markdown 内嵌图片：点击调起灯箱查看原图；链接包图时阻止跳转 */
function MarkdownImage({ src, alt, title }: { src?: string; alt?: string; title?: string }) {
  const openLightbox = useLightbox();
  if (!src) return <img alt={alt} title={title} />;
  return (
    <img
      className="lightboxable"
      src={src}
      alt={alt}
      title={title}
      onClick={(event) => {
        event.preventDefault();
        openLightbox(src);
      }}
    />
  );
}

/** 默认清洗会剥掉 data: URL；放行图片 data URL（生成图常以 base64 内嵌） */
function urlTransform(url: string): string {
  return /^data:image\//i.test(url) ? url : defaultUrlTransform(url);
}

export const Markdown = memo(function Markdown({ text }: { text: string }) {
  return (
    <div className="markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkCjkFriendly, remarkMath]}
        rehypePlugins={[
          rehypeHighlight,
          [rehypeKatex, { throwOnError: false }]
        ]}
        urlTransform={urlTransform}
        components={{
          a: (props) => <a {...props} target="_blank" rel="noreferrer" />,
          img: MarkdownImage,
          pre: CodeWindow
        }}
      >
        {normalizeMathDelimiters(text)}
      </ReactMarkdown>
    </div>
  );
});
