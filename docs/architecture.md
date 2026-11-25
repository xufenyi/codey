# Codey 架构与边界

## 进程

一个 Rust 后台进程拥有实例锁、任务库、调度器、本机 HTTP 服务和主线程原生托盘事件循环。浏览器是可断开的客户端，不拥有任务生命周期。每个查询或执行使用独立的 Codex app-server 子进程；实际执行期间连接保持直到轮次终态、失败或中断，退出时 kill/wait 回收。

正式程序嵌入静态界面。不引入 Electron、Node 服务、VS Code 兼容对象、自建模型接口或云端。

## 模块

- `model`：任务、设置、审批记录与校验。
- `store`：实例锁、16 MiB 边界、原子写和崩溃恢复。
- `runtime`：PATH / 手动入口、npm 启动脚本解析、无 shell 的进程创建。
- `rpc`：双向 JSONL 请求、通知、审批与进程关闭。
- `engine`：执行、配额、账户、修订、审批及历史匹配。
- `server`：回环 HTTP、令牌与来源校验、静态资产和文件浏览。
- `tray`：主线程系统事件循环与退出操作。

## 状态与不变量

`scheduled → checking → sending → running → completed/failed/interrupted`，审批时进入 `approval`；临时配额问题回到 scheduled；账户/准备失败为 paused；变更或错过时间进入 confirmation；提交结果不确定进入 review。

所有提交先记录 `submittedAt` 和 `clientMessageId`。在输入可能被接受后，网络错误、无回执、进程退出和重启均不自动重发。只有收到明确协议拒绝时清除提交边界；人工编辑不能把 review 变回可调度记录。

确认动作绑定 confirmationId，编辑保存刷新账户与历史基线。编辑动作带 expectedUpdatedAt，避免覆盖已经变化的任务。存储失败停止新调度。程序首版全局串行。

## 协议

参考 [官方 app-server 文档](https://learn.chatgpt.com/docs/app-server?site_locale=en)，以及 `codex-cli 0.172.0` 的 `generate-ts` 输出。需要核对本机 Codex 版本的协议时，可执行 `codex app-server generate-ts --out ./protocol-reference`。该目录由 Git 忽略，供开发时查阅。

使用 initialize / initialized、account/read、account/rateLimits/read、model/list、config/read、thread/list、thread/read、thread/turns/list、thread/start、thread/resume、turn/start、turn/interrupt 和 turn/completed。消息 ID 当作不透明字符串处理。命令/文件变更审批支持一次允许和拒绝；其他反向交互返回错误并停止本轮，不假装已支持。

已有对话不覆盖模型或推理强度。新对话的模型设置属于该对话，不实现插件旧有的首轮后恢复默认模型行为。权限在 resume/start/turn 参数中传递，可能成为该对话后续轮次的设置；不修改全局 config.toml。
