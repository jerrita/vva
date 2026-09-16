/** 内联 SVG 图标集：stroke 2、round，颜色继承 currentColor（对齐 frontend/AGENTS.md） */

interface IconProps {
  size?: number
  strokeWidth?: number
}

function Svg({
  size = 16,
  strokeWidth = 2,
  children,
  fill = 'none',
}: IconProps & { children: React.ReactNode; fill?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      fill={fill}
      stroke={fill === 'none' ? 'currentColor' : 'none'}
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {children}
    </svg>
  )
}

export function IconPlus({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M12 5v14M5 12h14" />
    </Svg>
  )
}

export function IconX({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M18 6L6 18M6 6l12 12" />
    </Svg>
  )
}

export function IconCheck({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M20 6L9 17l-5-5" />
    </Svg>
  )
}

export function IconChevronDown({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M6 9l6 6 6-6" />
    </Svg>
  )
}

export function IconChevronRight({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M9 18l6-6-6-6" />
    </Svg>
  )
}

export function IconArrowUp({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M12 19V5M5 12l7-7 7 7" />
    </Svg>
  )
}

export function IconArrowLeft({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M19 12H5M12 19l-7-7 7-7" />
    </Svg>
  )
}

export function IconFolderClosed({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
    </Svg>
  )
}

export function IconFolderOpen({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M6 14l1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2" />
    </Svg>
  )
}

export function IconCopy({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <rect x="9" y="9" width="13" height="13" rx="2" />
      <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
    </Svg>
  )
}

export function IconFile({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <polyline points="14 2 14 8 20 8" />
    </Svg>
  )
}

export function IconDownload({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M12 3v12M7 10l5 5 5-5" />
      <path d="M5 21h14" />
    </Svg>
  )
}

export function IconZap({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M13 2L3 14h9l-1 8 10-12h-9l1-8Z" />
    </Svg>
  )
}

/** 用户消息操作：回退（撤销箭头） */
export function IconRevert({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <path d="M9 14 4 9l5-5" />
      <path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11" />
    </Svg>
  )
}

/** 用户消息操作：分叉（git 分支） */
export function IconFork({ size, strokeWidth }: IconProps) {
  return (
    <Svg size={size} strokeWidth={strokeWidth}>
      <line x1="6" x2="6" y1="3" y2="15" />
      <circle cx="18" cy="6" r="3" />
      <circle cx="6" cy="18" r="3" />
      <path d="M18 9a9 9 0 0 1-9 9" />
    </Svg>
  )
}

/** 顶栏：会话列表开合（收起态带展开箭头，展开态面板填充） */
export function IconPanelLeft({ collapsed, size }: IconProps & { collapsed: boolean }) {
  return (
    <Svg size={size} strokeWidth={1.8}>
      <rect x="3" y="4.5" width="18" height="15" rx="2.5" />
      <path d="M9.5 4.5v15" />
      {collapsed ? (
        <path d="M6 9.5l2.5 2.5L6 14.5" />
      ) : (
        <rect x="4.8" y="6.3" width="3.4" height="11.4" rx="1" fill="currentColor" stroke="none" />
      )}
    </Svg>
  )
}

/** 顶栏：相关文件侧栏开合（右缘面板填充） */
export function IconPanelRight({ size }: IconProps) {
  return (
    <Svg size={size} strokeWidth={1.8}>
      <rect x="3" y="4.5" width="18" height="15" rx="2.5" />
      <path d="M14.5 4.5v15" />
      <rect x="15.8" y="6.3" width="3.4" height="11.4" rx="1" fill="currentColor" stroke="none" />
    </Svg>
  )
}

/** 顶栏：用量面板开关 */
export function IconActivity({ size }: IconProps) {
  return (
    <Svg size={size} strokeWidth={1.8}>
      <polyline points="22 12 18 12 15 21 9 3 6 12 2 12" />
    </Svg>
  )
}

/** 发送中停止按钮（实心方块） */
export function IconStop({ size }: IconProps) {
  return (
    <Svg size={size} fill="currentColor">
      <rect x="6" y="6" width="12" height="12" rx="2" />
    </Svg>
  )
}
