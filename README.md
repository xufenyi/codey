# Codey

一个 Codex 任务调度器，帮你定时向 Codex 投放提示词，包括**额度恢复后自动投送**，解决 5 小时焦虑！

## 使用

本程序依赖 [Codex CLI](https://github.com/openai/codex)，请先安装并登录，更多信息请查阅[Codex 官方文档](https://developers.openai.com/codex/cli/)。

运行程序后会弹出默认浏览器并驻后台。首次启动在运行环境页面检测 Codex：优先查找系统环境变量，显示发现 Codex 的详细信息；失败时可以手动输入路径。支持识别 Windows `npm` 全局安装的 `codex.cmd` / `codex.ps1`，解析其安装目录中的实际二进制（本软件本身不需要 Node.js）。

## 安排任务

1. 选择本机项目目录。
2. 新建对话，或选择已有对话；支持搜索与分页。选择已有对话会填写其目录。
3. 手动输入提示词。
4. 指定本地日期时间，或选择“可用时执行”并设置 1–30 天等待期限；不要在同一对话同时发起手动执行和定时执行。
5. 新对话可从当前账户模型目录选择模型和推理强度。已有对话沿用自己的模型设置。
6. 选择 Codex 沙箱、审批策略与网络设置；这些配置直接传给 Codex。
7. 对已有对话选择内容变化后的发送、跳过或确认策略；设置错过执行时间时的处理方式。

## 本地数据

默认目录由操作系统确定：
- Windows 通常位于 `%LOCALAPPDATA%/Codey`
- Linux 通常位于 `$XDG_DATA_HOME/Codey` 或 `~/.local/share/Codey`
- macOS 通常位于 `~/Library/Application Support/Codey`

管理服务只监听 `127.0.0.1`，采用随机端口、启动会话令牌、Host/Origin 校验及 CSP。令牌通过 URL fragment 传给页面，随后从地址栏清除。Prompt 通过子进程 stdin 传递，不进入命令行参数。

```sh
codey --help
codey --check-runtime
codey --no-browser
codey --headless --no-browser --data-dir ./codey-data
```

`--headless` 用于无托盘环境或测试；正常使用直接运行程序。

## 开发者文档

协议与设计边界见 [架构说明](docs/architecture.md)。

Windows、macOS 和 Linux 的构建、分发及图标配置见[打包指南](docs/packaging.md)。
