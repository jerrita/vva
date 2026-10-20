/** 后端工具名常量（对应 src/tools/names.rs）：每个名字只对应一种协议，按具体名字精确匹配。 */
export const READ = 'Read'
export const WRITE = 'Write'
export const EDIT = 'Edit'
export const GLOB = 'Glob'
export const GREP = 'Grep'
export const SHELL = 'Shell'
export const APPLY_PATCH = 'apply_patch'
export const HASHLINE_READ = 'hashline_read'
export const HASHLINE_GREP = 'hashline_grep'
export const HASHLINE_EDIT = 'hashline_edit'

export const isReadTool = (name: string) => name === READ || name === HASHLINE_READ
export const isGrepTool = (name: string) => name === GREP || name === HASHLINE_GREP
/** 参数为补丁原文（非 JSON）的 freeform 编辑工具 */
export const isPatchTool = (name: string) => name === APPLY_PATCH || name === HASHLINE_EDIT
/** 产生文件改动的工具 */
export const isChangeTool = (name: string) => name === EDIT || name === WRITE || isPatchTool(name)
