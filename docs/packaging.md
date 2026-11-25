# Codey 打包指南

在各目标系统上从项目根目录执行 `cargo build --locked --release --bin codey`，即可生成该系统和架构的正式程序。`--locked` 使用 `Cargo.lock` 中的依赖版本，`--bin codey` 选中正式程序。以下命令使用 Cargo 的默认输出目录 `target/`。如果配置了 `CARGO_TARGET_DIR` 或给 Cargo 传了 `--target`，请按实际输出路径取文件。[Cargo build 文档](https://doc.rust-lang.org/cargo/commands/cargo-build.html)

构建机使用当前稳定版 Rust。HTML、CSS、JavaScript 和浏览器图标会随 Rust 编译嵌入程序，所以发布时只需收集程序、`README.md` 和 `LICENSE`。用户使用调度功能前，需要在目标机安装并登录 Codex CLI。[Codey 使用说明](../README.md)

## Windows

安装 [Rust MSVC 工具链](https://rust-lang.github.io/rustup/installation/windows-msvc.html)和 Visual Studio Build Tools。在安装器中选择「使用 C++ 的桌面开发」，包含 MSVC 和 Windows SDK。用 PowerShell 执行。

```powershell
cargo build --locked --release --bin codey
if ($LASTEXITCODE -ne 0) { throw 'Codey 构建失败' }
New-Item -ItemType Directory -Path dist -Force | Out-Null
$files = Resolve-Path -LiteralPath @(
    'target/release/codey.exe', 'README.md', 'LICENSE'
)
$zip = Join-Path (Resolve-Path -LiteralPath dist).ProviderPath 'codey-windows.zip'
Compress-Archive -LiteralPath $files.ProviderPath -DestinationPath $zip -Force
```

解压后双击 `codey.exe`，检查默认浏览器是否打开、系统托盘是否出现，以及托盘能否退出程序。Windows 的 release 版使用 GUI 子系统，排查启动或查看 `--help` 输出时可用 `cargo run --locked --bin codey -- --help`。

## macOS

安装稳定版 Rust 和 Xcode Command Line Tools。首次配置可执行 `xcode-select --install`。默认构建对应当前 Rust 工具链架构，通常是 Apple Silicon 的 `aarch64-apple-darwin` 或 Intel 的 `x86_64-apple-darwin`。分发时在文件名标明架构。

```sh
cargo build --locked --release --bin codey &&
mkdir -p dist &&
tar -czf "dist/codey-macos-$(uname -m).tar.gz" \
  -C "$PWD/target/release" codey -C "$PWD" README.md LICENSE
```

解压后在桌面会话的终端运行 `./codey`。若要让用户从 Finder 双击启动，可以在上述程序外加一层应用包。

| 应用包路径 | 内容 |
| --- | --- |
| `Codey.app/Contents/MacOS/codey` | release 可执行文件，保留执行权限 |
| `Codey.app/Contents/Info.plist` | `CFBundleExecutable=codey`、`CFBundlePackageType=APPL`、唯一的 `CFBundleIdentifier` 和版本号 |
| `Codey.app/Contents/Resources/Codey.icns` | Finder 应用图标，对应 `CFBundleIconFile=Codey.icns` |

用 `plutil -lint Codey.app/Contents/Info.plist` 检查配置，再用 `open Codey.app` 检查菜单栏和浏览器。若通过 Finder 启动时检测不到终端中安装的 Codex，在 Codey 的运行环境页面填入 Codex 的绝对路径。面向外部用户分发时，在应用包和图标定稿后执行 Developer ID 签名和 Apple 公证。[Apple 应用包结构](https://developer.apple.com/library/archive/documentation/CoreFoundation/Conceptual/CFBundles/BundleTypes/BundleTypes.html) · [Apple 公证文档](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)

## Linux

安装稳定版 Rust 和桌面库的开发包。以下以项目 CI 使用的 Ubuntu 22.04 为例，其他发行版使用各自的软件包名称。

```sh
sudo apt-get update &&
sudo apt-get install -y build-essential pkg-config libgtk-3-dev libayatana-appindicator3-dev libxdo-dev &&
cargo build --locked --release --bin codey &&
mkdir -p dist &&
tar -czf "dist/codey-linux-$(uname -m).tar.gz" \
  -C "$PWD/target/release" codey -C "$PWD" README.md LICENSE
```

在目标系统解压并运行 `./codey`。目标机需要 GTK 3、Ayatana AppIndicator 等运行库，以及支持托盘的桌面会话。用 `ldd ./codey` 检查链接库，再实际启动确认托盘可用。面向多个发行版分发时，在计划支持的最老发行版上构建，并在目标发行版上逐一验证，以检查 glibc 和桌面库兼容性。[项目 CI](../.github/workflows/ci.yml) · [tray-icon 文档](https://docs.rs/tray-icon/0.26.1/tray_icon/)

如果需要应用菜单入口，安装可执行文件后，将 `codey.desktop` 放到 `~/.local/share/applications/`，并将 `Exec` 指向安装位置，例如 `/usr/local/bin/codey`。系统级安装可通过发行包部署到对应的系统目录。

```ini
[Desktop Entry]
Type=Application
Name=Codey
Exec=/usr/local/bin/codey
Icon=codey
Terminal=false
```

## 图标文件

| 显示位置 | 当前来源 | 更换或添加方式 |
| --- | --- | --- |
| 浏览器标签页 | [`web/favicon.svg`](../web/favicon.svg)，由 [`src/server.rs`](../src/server.rs) 编译时嵌入 | 更新 SVG 并重新编译 |
| 系统托盘或 macOS 菜单栏 | [`src/tray.rs`](../src/tray.rs) 的 `icon()` 生成 32×32 RGBA 像素 | 修改绘制代码，或从同一母版导出透明 RGBA 像素，在编译时嵌入后传给 `Icon::from_rgba` |
| Windows 资源管理器 | 若需文件图标，需增加 PE 图标资源 | 将母版导出为多尺寸 `.ico`（例如 16、32、48、256 像素），在 Windows 构建中用 `build.rs` 调用资源编译步骤，把 `ICON` 资源链接进 `codey.exe` |
| macOS Finder | 由 `.app` 的图标配置提供 | 从母版导出 16、32、128、256、512 像素的透明 PNG 和相应的两倍尺寸。将它们按 `icon_16x16.png`、`icon_16x16@2x.png` 等规则放入 `Codey.iconset/`，用 `iconutil -c icns Codey.iconset -o Codey.icns` 生成应用包资源 |
| Linux 应用菜单 | 由 `.desktop` 的 `Icon` 字段指定 | 复用母版 SVG，安装为 `~/.local/share/icons/hicolor/scalable/apps/codey.svg`。如需兼容 PNG，可另装 `hicolor/48x48/apps/codey.png` |

可以用现有 `web/favicon.svg` 作为图形母版，保留转换结果的透明背景，并在小尺寸下检查可辨识度。Windows 文件图标、macOS 应用包图标和 Linux 应用菜单图标由各平台各自读取，托盘和浏览器分别读取表中的来源。[Microsoft ICON 资源](https://learn.microsoft.com/en-us/windows/win32/menurc/icon-resource) · [Apple CFBundleIconFile](https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleiconfile) · [freedesktop 图标主题规范](https://specifications.freedesktop.org/icon-theme/latest/) · [Desktop Entry 规范](https://specifications.freedesktop.org/desktop-entry/latest/recognized-keys.html)

## 一键打包

[现有 GitHub Actions 工作流](../.github/workflows/ci.yml)已在 Windows、Ubuntu 和 macOS 上分别构建并上传产物。将源码推送到 GitHub 后，可在 Actions 中手动运行 `Codey`，从该次运行下载三个平台的产物。覆盖更多 CPU 架构时，需要在矩阵中明确加入目标和运行环境。[GitHub 手动运行工作流](https://docs.github.com/en/actions/managing-workflow-runs-and-deployments/managing-workflow-runs/manually-running-a-workflow)
