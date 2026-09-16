export function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

/** 路径的父目录部分（无分隔符时为空串）。 */
export function parentDir(path: string): string {
  return path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''
}

const LANGUAGE_BY_EXTENSION: Record<string, string> = {
  bash: 'bash',
  c: 'c',
  cc: 'cpp',
  cpp: 'cpp',
  cs: 'csharp',
  css: 'css',
  go: 'go',
  h: 'c',
  hpp: 'cpp',
  html: 'xml',
  java: 'java',
  js: 'javascript',
  json: 'json',
  jsx: 'javascript',
  md: 'markdown',
  mjs: 'javascript',
  py: 'python',
  rb: 'ruby',
  rs: 'rust',
  scss: 'scss',
  sh: 'bash',
  sql: 'sql',
  swift: 'swift',
  ts: 'typescript',
  tsx: 'typescript',
  toml: 'ini',
  xml: 'xml',
  yaml: 'yaml',
  yml: 'yaml',
}

export function languageForPath(path: string | null): string | null {
  if (!path) return null
  const name = path.split('/').pop()?.toLowerCase() ?? ''
  if (name === 'dockerfile') return 'dockerfile'
  const extension = name.split('.').pop()
  return extension ? (LANGUAGE_BY_EXTENSION[extension] ?? null) : null
}

/** hljs 高亮 HTML 按行切分，跨行 span 在行首重开标签。 */
export function splitHighlightedLines(html: string): string[] {
  const lines = ['']
  const openTags: string[] = []
  const tokens = html.match(/<[^>]+>|[^<]+/g) ?? []

  for (const token of tokens) {
    if (token.startsWith('<span')) {
      lines[lines.length - 1] += token
      openTags.push(token)
      continue
    }
    if (token === '</span>') {
      lines[lines.length - 1] += token
      openTags.pop()
      continue
    }

    const parts = token.split('\n')
    lines[lines.length - 1] += parts[0]
    for (const part of parts.slice(1)) {
      lines[lines.length - 1] += openTags.map(() => '</span>').reverse().join('')
      lines.push(openTags.join('') + part)
    }
  }

  lines[lines.length - 1] += openTags.map(() => '</span>').reverse().join('')
  return lines
}
