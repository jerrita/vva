# 文档索引

## 使用

- [usage.md](usage.md)：命令、参数、扩展机制与 Agent Space 入口
- [config-reference.md](config-reference.md)：配置文件全部字段、加载与合并、模型寻址、env 文件

## 设计

| 文档 | 内容 |
|---|---|
| [architecture.md](architecture.md) | LCD 问题、分层、数据流 |
| [domain_model.md](domain_model.md) | 四层实体、ContentPart、引用注脚、强类型元数据 |
| [storage.md](storage.md) | JSONL 布局、SessionStore、容灾 |
| [providers.md](providers.md) | 供应商原生适配、freeform 工具、结构化输出、SSE |
| [ui.md](ui.md) | TUI：会话订阅、渲染、输入、模态 |
| [web_sync.md](web_sync.md) | Web SSE 快照 / 增量协议 |
| [background_tasks.md](background_tasks.md) | 后台任务展示、持久化与自动唤醒 |
| [agent.md](agent.md) | Agent Space：Router、生命周期、curator、rollover、Scheduler |

## 参考

- [code_map.md](code_map.md)：目录结构与模块职责
- [../AGENTS.md](../AGENTS.md)：设计约束与开发指南
- [../frontend/AGENTS.md](../frontend/AGENTS.md)：Web 前端规范
