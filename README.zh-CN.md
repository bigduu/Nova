# Nova

[English](README.md) · [简体中文](README.zh-CN.md)

**让任何 MCP agent 直接操作你 Mac 或 Windows 上的真实应用。** Nova 是单个 Rust
二进制文件，不需要 Python 运行时。它为 Claude Desktop、Cursor、Codex、Claude Code
等 MCP 客户端提供 macOS 和 Windows 上的截图、系统原生 OCR、键盘鼠标输入、剪贴板、
窗口/应用控制，以及无障碍接口（Accessibility / UIA）操作。

[下载](https://github.com/bigduu/Nova/releases/latest) · [Bodhi](https://github.com/bigduu/Zenith) 组件之一 · [Bodhi 桌面版](https://github.com/bigduu/Bodhi-AI/releases/latest) · [MIT 开源](LICENSE)

- **操作你正在用的真实桌面，不是虚拟机**：支持 macOS 14+（Apple Silicon 和 Intel）
  以及 Windows x64/ARM64。
- **先看清再点击**：读取 Accessibility/UIA 控件，使用 Apple Vision 或 Windows OCR
  识别文字，支持单窗口截图和局部放大。点击坐标按截图的像素空间给出，再映射回屏幕。
- **什么文字都能输入**：键盘输入支持完整 Unicode，包括中文、日文、韩文和 emoji。
- **优先走无障碍语义的控制（v0.3.0）：** AX 优先的 `ax_read` / `ax_activate`
  （会拒绝过期快照）、上限 64 步并返回结构化失败信息的 `batch_actions`、负责权限的
  Nova.app（开发预览版），以及可选的 Chrome DevTools 旁路服务。最新发布版：**v0.3.0** —
  [Releases](https://github.com/bigduu/Nova/releases/tag/v0.3.0)。

<p align="center"><img src="docs/demos/browser-checklist.gif" alt="Nova 通过真实的浏览器 MCP 操作勾选两个演示条目并准备审阅。" width="720"></p>

[静态图片](docs/demos/browser-checklist.png) · [复现方法与 MCP 记录](docs/demos/README.md)

这段录屏展示的是 Nova 的**浏览器工具路径**（专用演示页面，导航和点击都是
真实的 MCP 调用）。它在 Linux 上录制，因此不代表 macOS/Windows 原生桌面操作，不包含模型
推理，也不是发布版二进制。复现说明里解释了 `--npx` 适配器和隔离浏览器的用法。

## 安装

| 平台 | 已发布的 v0.3.0 |
| --- | --- |
| macOS 14+（Homebrew） | `brew install bigduu/tap/nova` |
| macOS 14+（手动） | 从 [Releases](https://github.com/bigduu/Nova/releases/tag/v0.3.0) 下载 `nova-v0.3.0-universal-apple-darwin.tar.gz` |
| Windows x86_64 | `nova-v0.3.0-x86_64-pc-windows-msvc.zip` |
| Windows ARM64 | `nova-v0.3.0-aarch64-pc-windows-msvc.zip` |
| Linux | 不支持桌面操作（只能做无界面的协议检查） |

**Homebrew（macOS）：**

```sh
brew tap bigduu/tap
brew install bigduu/tap/nova
nova --version
```

这个 formula 从 release 压缩包安装命令行程序，不会帮你配置 MCP 客户端，也不会授予 macOS
权限。用 `which nova` 查看下面配置要填的路径：Apple Silicon 通常是
`/opt/homebrew/bin/nova`，Intel Mac 通常是 `/usr/local/bin/nova`。

**手动下载：**从同一个 release 下载对应的 `.sha256` 文件，校验压缩包后再解压。macOS 上：

```sh
tar -xzf nova-v*-universal-apple-darwin.tar.gz
xattr -dr com.apple.quarantine ./nova  # 仅在 Gatekeeper 拦截下载文件时需要
sudo install -m 0755 nova /usr/local/bin/nova
```

Windows 上解压与本机架构对应的压缩包，直接运行 `nova.exe`，或把所在目录加入 `PATH`。
Windows 二进制没有 Authenticode 签名，首次运行时 SmartScreen 可能会提示。macOS
二进制是 ad-hoc 签名，没有公证。

**从源码构建：**

```sh
git clone https://github.com/bigduu/Nova.git
cd Nova
cargo build --release --locked
```

macOS 上产物是 `target/release/nova`，Windows 上是 `target/release/nova.exe`。不要用
`cargo install nova`：crates.io 上的这个名字属于另一个不相关的项目。Nova 没有发布到 npm。

## 选择版本

| 途径 | 能用到什么 |
| --- | --- |
| [已发布的 v0.3.0](https://github.com/bigduu/Nova/releases/tag/v0.3.0)（Homebrew 和 release 压缩包） | 截图、`zoom_region`、OCR、编号标记与 `click_mark`、鼠标键盘输入、含 `ax_read` / `read_ui` / `ax_activate` 的 AX/UIA 操作、`inspect_app`、托管的 `nova mcp`、Nova.app 开发预览版、可选 Chrome DevTools 旁路服务、需逐页配对的 [Chrome 桥](chrome/README.md)、窗口/应用、剪贴板、`batch_actions`（64 步上限 + 结构化失败）、`wait`。 |
| `master`（开发前沿） | v0.3.0 之后尚未发版的改动 — 需要从源码构建。 |

Nova.app 仍是**开发预览版**（ad-hoc 签名，未公证）。菜单栏状态和虚拟光标标为**预览**，真机验收尚未完成（[#34](https://github.com/bigduu/Nova/issues/34)、[#70](https://github.com/bigduu/Nova/issues/70)）。[审计记录](docs/readme-audit.md)。

## 在 MCP 客户端中使用

下面的配置都使用**直连 stdio**（不带参数），v0.3.0 发布版和 `master` 构建都适用。把路径
换成你自己 `which nova` 的输出、解压出的二进制或 `target/release/nova`。图形界面客户端不一定
继承你 shell 的 `PATH`，所以请写绝对路径。

**Claude Desktop**：macOS 上编辑
`~/Library/Application Support/Claude/claude_desktop_config.json`，Windows 上编辑
`%APPDATA%\Claude\claude_desktop_config.json`。
**Cursor**：编辑 `~/.cursor/mcp.json`（对所有项目生效），或某个项目里的 `.cursor/mcp.json`。

```json
{
  "mcpServers": {
    "nova": { "command": "/opt/homebrew/bin/nova", "args": [] }
  }
}
```

**Claude Code：**

```sh
claude mcp add --scope user nova -- /opt/homebrew/bin/nova
```

**Codex**：写进 `~/.codex/config.toml`，或者运行
`codex mcp add nova -- /opt/homebrew/bin/nova`：

```toml
[mcp_servers.nova]
command = "/opt/homebrew/bin/nova"
args = []
```

Windows 上请写 `nova.exe` 的完整路径：JSON 里要转义（`"C:\\Tools\\nova\\nova.exe"`），
TOML 里可以用字面量字符串（`'C:\Tools\nova\nova.exe'`）。

**macOS 权限**：直连 stdio 模式下，macOS 通常把 Nova 算在启动它的应用名下。请给 Claude
Desktop、Cursor，或运行 Claude Code / Codex 的终端或 IDE 授予**辅助功能**（输入和界面控制）
和**屏幕录制**（`screenshot`、`ocr`、`list_windows`）权限。如果不起作用，再把 `nova`
二进制本身加进去。授权后重启或重新连接 MCP 服务。也可以改由
[Nova.app](#novaapp-开发预览版) 持有权限，见[托管模式](#通过-novaapp-的托管模式)。

试试对 agent 说：*“用 Nova 列出我打开的窗口，给最前面的窗口截个图，告诉我能看到哪些按钮。”*

### 通过 Nova.app 的托管模式

`nova mcp` 是 Bamboo 插件使用的跨平台托管入口（v0.3.0+）：

```json
{
  "mcpServers": {
    "nova": { "command": "/absolute/path/to/nova", "args": ["mcp"] }
  }
}
```

Windows 和 Linux 无界面构建提供普通的 stdio MCP。macOS 上 `mcp` 只会连接独立的 Nova.app，
需要时通过 LaunchServices 启动它。请单独安装这个应用，打开一次，并给 Nova 授予**辅助功能**
权限；截图、OCR 和 `list_windows` 需要**屏幕录制**权限（只有你在应用菜单里明确选择时才会
请求）。应用内附带的可执行文件也可以直接当连接器用：

```json
{
  "mcpServers": {
    "nova": {
      "command": "/Applications/Nova.app/Contents/MacOS/nova",
      "args": ["mcp"]
    }
  }
}
```

显式的 `--connect` 命令仍然支持，和 macOS 上的 `mcp` 用同一种传输方式：通过私有的、按用户
隔离的 Unix socket 转发 MCP 字节。连接器本身不调用桌面 API，也不请求 macOS 权限；由应用进程
负责 MCP 处理和 TCC 权限归属。socket 位于 `/tmp/nova-app-<uid>/`，目录权限 0700、socket
权限 0600，并校验对端是同一 UID。

如果 Nova.app 不可用，托管命令会退出，并给出安装和重连提示。它绝不会退回到在 MCP 宿主进程里
直接做桌面操作。`NOVA_APP_SOCKET` 只用于隔离的开发和测试；设置后会禁用自动启动应用。正常
安装使用时请不要设置。不在应用包内的 `nova` 不带参数运行时，仍然是旧的直连 stdio 模式。

当应用服务关闭连接时，CLI 连接器会转发完剩余响应后退出，即使宿主仍保持 stdin 管道打开。
宿主正常关闭 stdin 时，仍会半关闭请求流，并把服务最后的响应（包括最后缓冲的字节）读完。
标准输出的背压照常存在：宿主必须持续读取响应。替换或重启服务后，只需重连 Nova 这一个 MCP
服务，Bodhi 可以保持打开。被中断的请求不会重放，也不会自动建立新的 MCP 会话。

这种退出行为只针对会终止的 CLI 连接器进程。转发完成后它专用的运行时会被释放，进程退出也会
回收尚未完成的阻塞 stdin 读取。它并不会让 `connect_stdio` 库函数在常驻或嵌入式运行时中的
stdin 变得可取消。

### Chrome DevTools MCP sidecar

如果需要更高级的 Chrome 页面自动化和调试，Nova 可以在桌面
服务旁边启动官方的 [Chrome DevTools MCP](https://github.com/ChromeDevTools/chrome-devtools-mcp)。
它是一个透明的 stdio 旁路服务，不是 Nova 内部的第二套浏览器实现。需要 npm/`npx`、Node.js
`^20.19.0`、`^22.12.0` 或 `>=23`，以及当前稳定版（或更新）的 Chrome。Nova 把审阅过的上游包
固定为 `chrome-devtools-mcp@1.8.0`。

应用级集成应接受应用选择器（名称或 bundle ID），并在内部使用 `inspect_app` 发现机制。仅靠
发现并不会授予 CDP 控制权；自动把应用路由到对应 provider 是另一项单独的集成工作。复制
endpoint 和下面单独的 MCP 配置属于高级传输兼容用法。macOS 上：

```json
{
  "mcpServers": {
    "nova": {
      "command": "/Applications/Nova.app/Contents/MacOS/nova",
      "args": ["--connect"]
    },
    "nova-chrome-devtools": {
      "command": "/Applications/Nova.app/Contents/MacOS/nova",
      "args": ["chrome-devtools"]
    }
  }
}
```

如果是从当前源码构建的独立二进制，就写它的绝对路径和 `["chrome-devtools"]`。已发布的
如果图形界面客户端找不到 `npx`，在子命令后面加上
`"--npx", "/absolute/path/to/npx"`。

默认会启动一个全新的、临时的隔离 Chrome 配置文件。默认关闭使用统计、包更新检查和 CrUX URL
查询，并屏蔽敏感的网络请求头。已附加的 DevTools 目标发出的请求可以用重复的
`--allowed-url-pattern` 加以限制，例如：

```json
"args": [
  "chrome-devtools",
  "--allowed-url-pattern", "https://example.com/*",
  "--allowed-url-pattern", "https://*.example.net/*"
]
```

URL 允许规则需要 Chrome 149+。它只在 MCP 服务已附加时对 DevTools 目标生效，并不是完整的
网络沙箱；如果需要完全的网络隔离，请按[上游安全策略](https://github.com/ChromeDevTools/chrome-devtools-mcp/security/policy)
使用操作系统或虚拟机沙箱。

如果要改用一个已经在运行、已登录的 Chrome 配置文件，先在 Chrome 里打开
`chrome://inspect/#remote-debugging` 并启用远程调试，然后配置：

```json
"args": ["chrome-devtools", "--profile", "existing"]
```

自动连接需要 Chrome 144+。如果同时有多个 Chrome 配置文件在运行，Chrome 会选择它认为的默认
配置文件；操作前请先选定并确认已连接的页面。

> [!WARNING]
> 现有配置文件模式可以查看并控制所选 Chrome 配置文件中所有打开的窗口，包括已登录的页面。
> 只对可信的本地 MCP 客户端启用，用完后请关闭远程调试。

内部调用方或高级用户可以连接一个明确选定、已经在运行的浏览器，而不依赖稳定版 Chrome 的默认
配置文件：

```sh
nova chrome-devtools --browser-url http://127.0.0.1:9222
nova chrome-devtools --ws-endpoint 'ws://[::1]:9222/devtools/browser/<id>'
```

只能使用一个 endpoint，且不能同时带 `--profile`（包括显式的 `isolated`）或 `--headless`。
`--browser-url` 接受指向浏览器根路径的 HTTP(S) 地址，末尾 `/` 可选；`--ws-endpoint` 接受
`/devtools/browser/<id>` 形式的 WS(S) 地址，其中 ID 只能包含字母、数字、连字符或下划线。两者
都要求原始的字面量回环 IP 和显式端口 1–65535。主机名、缩写/整数/十六进制 IP、凭据、查询
字符串和片段都会在启动 npx 之前被拒绝。Nova 原样转发所选地址，不额外添加 Chrome 启动参数，
也没有自动连接回退。HTTPS/WSS 使用上游的证书校验。

这些输入是**可信的本地 endpoint**，并不是网络隔离。固定版本的
[Puppeteer HTTP 发现](https://github.com/puppeteer/puppeteer/blob/puppeteer-v25.8.0/packages/puppeteer-core/src/common/BrowserConnector.ts#L180-L195)
会使用返回的 `webSocketDebuggerUrl`，其
[WebSocket 传输会跟随重定向](https://github.com/puppeteer/puppeteer/blob/puppeteer-v25.8.0/packages/puppeteer-core/src/node/NodeWebSocketTransport.ts#L19-L29)。
因此本地服务可以把连接引到其他地址。固定版本的
[选项](https://github.com/ChromeDevTools/chrome-devtools-mcp/blob/chrome-devtools-mcp-v1.8.0/src/config/mcp-options.ts)
和[归属清理逻辑](https://github.com/ChromeDevTools/chrome-devtools-mcp/blob/chrome-devtools-mcp-v1.8.0/src/browser.ts#L276-L300)
区分了“附加”和“启动”：断开连接、stdin EOF 或 SIGTERM 时只会从附加的浏览器上分离，而上游
自己启动的浏览器会被关闭。

连接后先调用 `list_pages`，按预期的标题/URL 选择页面。每个针对页面的读取或操作都显式传入返回的
`pageId`。例如有两个窗口，目标页面 ID 分别是 7 和 12：先 `take_snapshot({pageId: 7})`，再
`fill({pageId: 7, uid: "<该快照里的输入框>", value: "example"})`；然后
`take_snapshot({pageId: 12})`，再 `click({pageId: 12, uid: "<该快照里的按钮>"})`。页面关闭或
变化后要重新获取页面 ID 和元素 UID；不要依赖当前聚焦的是哪个窗口。

endpoint 必须提供**浏览器级 CDP**，覆盖应用的所有页面/窗口。WS 路径检查会排除仅渲染进程的
`/devtools/page/...` socket，以及 Node/V8 主进程的 inspector WebSocket URL。仅凭 HTTP 根路径
语法无法确认对外提供服务的到底是谁。上游官方支持的是 Chrome / Chrome for Testing；附加到
Electron/CEF 属于实验性用法，在该运行时自己的窗口里实测读取和操作之前都视为未验证。MCP
`tools/list` 成功本身并不能证明应用兼容。如果设置或连接失败，请检查所选应用支持的调试方式，
并刷新其发现元数据后再重试这种传输。Bodhi 可以保持打开；Nova 的原生 AX 能力和单独配对的
扩展能力仍然可用。

使用 `--enable-webmcp` 可以暴露上游实验性的 WebMCP 工具。在隔离模式下，Nova 会添加 Chrome
必需的 `--enable-features=WebMCP` 启动参数；对于现有配置文件或 endpoint，浏览器必须已经带着
这个特性启动。WebMCP 需要 Chrome 150+。`--expose-network-headers` 和
`--enable-performance-crux` 是需要显式开启的隐私选项。固定的 1.8.0 版本不支持
`--disable-javascript-evaluation` 选项，所以 Nova 不会宣称支持或传递它。

这个旁路服务和 Nova 可选的[安全 Chrome 桥](chrome/README.md)适用于不同的信任模型：DevTools
MCP 功能全面，适合常规浏览器自动化、DOM/网络检查和性能调试；安全 Chrome 桥要求逐页显式配对，
在需要按页面最小授权时更合适。浏览器自身界面、原生对话框和非网页界面仍然使用 Nova 的桌面工具。

请使用解压出的 release 二进制或源码构建产物的绝对路径。Windows 上使用转义后的可执行文件路径，
例如 `"C:\\absolute\\path\\nova.exe"`。下面的 AX 优先流程请使用源码构建产物。如果所在目录
已经在 `PATH` 中，命令可以直接写 `"nova"`。

在客户端里重连/重新加载 Nova MCP 服务即可，Bodhi 主窗口可以保持打开。直连 stdio 和开发版
二进制的情况，见[权限与代码签名](#权限与代码签名macos)。

**HTTP 客户端**：把 Nova 作为服务运行，通过 Streamable HTTP 连接：

```sh
nova --http                       # 127.0.0.1:3100/mcp
nova --http --addr 127.0.0.1:8080 # 自定义回环端口
```

HTTP 模式目前只是本地传输：保留 rmcp 默认的回环 Host 白名单，不配置远程访问认证。绑定所有
网卡不属于支持的局域网用法。

**第一次调用（当前源码构建）。**先调用 `ax_read`（可选
`ax_read(window="<name>", mode="all")`）获取语义内容和控件，再对确切的可操作节点调用
`ax_activate(snapshot_id, node_id)`。操作后再运行一次 `ax_read` 验证语义状态。如果 AX/UIA
覆盖缺失或不完整，用聚焦窗口的 `ocr` 读取渲染出的文字；只有确实需要像素时（布局、图标、颜色、
图片、画布或视觉验证）才用 `screenshot(window=...)` / `zoom_region`。所有指针工具都使用最近
一次截图的像素空间；`cursor_position` 则返回操作系统全局逻辑坐标。

语义读取（`ax_read` 及其别名 `read_ui`）会包含受支持的控件状态，并在 `all`/`content` 模式下
用 `scrollable=true` 标出滚动容器，包括没有名称的容器。容器本身不会获得操作标记。目标信息
包含窗口标题；如果所选 macOS 窗口提供 `AXDocument`/`AXURL`，还会包含可选的 `url`。不支持的
URL/状态属性会省略；Windows 使用受支持的缓存 UIA 状态和 ScrollPattern 元数据。

Windows 读取会保留缓存的 ValuePattern 文本（包括 Unicode 和多行内容），前提是控件提供这个
pattern。空值、不支持、读取失败或非字符串的值会省略；密码框和未知密码状态始终脱敏。只提供
TextPattern 的控件没有值的回退。

原生文本写入（`type_text`、`write_clipboard`、批量输入和 `ax_set_value`）不会在日志和确认信息
里包含提交的文字。写入诊断只保留字符数/UTF-8 字节数和原生操作细节，不回显内容。显式读取，
例如 `read_clipboard` 和 `ax_read`，仍会返回请求的内容。

## 系统要求

- **Linux**：源码可以构建一个无界面的 MCP 服务，用于协议检查；原生桌面操作会返回
  unsupported/headless 错误。这不是 Linux 桌面自动化。
- **macOS 14+**：macOS 桌面后端的最低版本。release 压缩包是 universal 版本，Apple Silicon
  和 Intel Mac 都能运行。
- **Windows x86_64 或 ARM64**：Windows 桌面后端。GitHub Releases 为两种架构分别提供原生
  压缩包。
- Windows 上的 `ocr` 使用系统已安装的 Windows OCR 语言包。可以用 `nova --ocr-langs` 查看
  可用语言；如果识别时提示不可用，请安装对应语言包。
- 在 macOS 上构建需要 macOS 15 SDK / Xcode 16+，原因是间接依赖 `apple-metal`；这只是构建
  时的要求，不是 Nova 运行所需的最低 macOS 版本。
- macOS 上，`screenshot`、`ocr` 和 `list_windows` 需要**屏幕录制**权限；`ax_read`、语义
  激活和输入需要**辅助功能**权限。

> macOS 会把这些权限授予它认定为对 Nova “负责”的那个进程。托管的
> `nova mcp` 入口和 Bamboo 插件在 macOS 上使用独立的 Nova.app。直连 stdio/HTTP
> 可能以宿主应用、终端或直接启动的二进制作为权限主体。见
> [权限与代码签名](#权限与代码签名macos)。

## 工具


| 工具 | 作用 |
| --- | --- |
| `ax_read` | 标准的 `ax:read`：通过 macOS Accessibility 或 Windows UIA 读取语义标签、文本、值、角色、可执行动作、状态和可选的边界框，不需要截图。返回临时的快照/节点协议，以及明确的覆盖范围和状态。 |
| `read_ui` | 兼容别名，底层使用同一套 `ax_read` 遍历和缓存代际。 |
| `ax_activate` | 激活最新一次 `ax_read` 中某个确切的可操作节点；拒绝过期的快照 ID，并报告 `route=ax\|uia\|web_dom\|element_center`。每次尝试都会在分发给 provider 之前消耗掉对应代际。 |
| `screenshot` | 截取整个显示器或单个 `window=`：用于布局、图标、颜色、图片、画布，以及在语义/OCR 路径之后做视觉验证。 |
| `zoom_region` | 以原生分辨率放大上一张截图中的一个矩形区域，用于在没有无障碍树的界面上看清小目标。 |
| `ocr` | macOS 上用 Apple Vision、Windows 上用 Windows Media OCR 识别屏幕文字。`mode=auto` 先用 Fast，置信度不够时回退到 Accurate；`mode=fast\|accurate` 强制使用其中一种。可选的严格 `roi={x,y,width,height}` 会通过原生区域路径重新截取当前图像中的一个矩形。返回每一行文字及其可点击的中心点。 |
| `click_mark` | 针对最新编号标记的兼容操作；推荐使用代际安全的 `ax_activate`。 |
| `left_click` / `right_click` / `double_click` / `mouse_move` / `scroll` | 在上一张截图像素空间中的指针输入。 |
| `cursor_position` | 读取鼠标在操作系统全局逻辑坐标中的位置；不会换算到上一张截图的像素空间。 |
| `type_text` / `key_combo` | 键盘输入（完整 Unicode，包括中日韩文字和 emoji）。 |
| `list_windows` / `list_applications` / `open_application` | 窗口和应用信息查询与启动。 |
| `inspect_app` | 可选的 macOS 应用能力发现。接受应用名称/bundle ID；省略时会发现正在运行的 Chromium 类候选应用。不需要调用方提供端口，也不会弹出权限请求。 |
| `read_clipboard` / `write_clipboard` | 剪贴板读写。 |
| `ax_click` / `ax_set_value` / `ax_focus` | 按无障碍角色/标签操作控件。 |
| `dump_ax` | 读取原始 AX/UIA 树，用于诊断和排查覆盖问题。 |
| `batch_actions` | 一次调用中执行一系列输入操作。 |
| `wait` | 暂停指定的秒数。 |

`batch_actions` 最多按顺序执行 64 个操作，遇到第一个失败就停止，不重试也不
回滚。成功时保留按行分隔的状态信息。失败时设置 MCP `isError=true`，并在文本内容和
`structuredContent` 中返回同样的 JSON：`completed: [{index, result}]`、`failed_index`、
`reason` 和 `not_executed: {start, end_exclusive}`。索引从 0 开始；这个左闭右开区间只包含失败
步骤之后的操作，而失败的那一步可能已经产生了部分副作用。超过 64 个操作会在执行任何操作之前
被拒绝，此时 `failed_index: null`，整批都标记为未执行。每条确认或原因最长 512 个 Unicode
字符，超长时保留首尾、中间用 `…` 省略。输入的文字只保留字符数/UTF-8 字节数，不保留明文。
决定重试什么之前，请先查看进度并重新 `ax_read` 获取最新状态；整批重放可能会重复已完成步骤的
副作用。

## 运行

```sh
cargo run                      # stdio 传输（默认）
cargo run -- mcp                # 托管 MCP：macOS 上走 Nova.app，其他平台走 stdio
cargo run -- --http            # Streamable HTTP，监听 127.0.0.1:3100
cargo run -- --http --addr 127.0.0.1:8080
```

> ScreenCaptureKit 链接的 Swift 运行时通过 `build.rs` 写入的 `LC_RPATH` 定位，所以
> `cargo run`/`cargo test` 和独立二进制都不需要设置 `DYLD_*` 环境变量。

## Nova.app 开发预览版

从包含应用打包流程的版本切出的 release 还会附带：

`nova-v<version>-universal-apple-darwin-development-app.zip`

压缩包里是一个 universal 的 `Nova.app`，它在没有 Dock 图标的情况下运行 Nova 的按用户应用
服务。这样屏幕录制和辅助功能权限归属于 Nova 自己的应用身份，而不是让 MCP 宿主（例如 Bodhi）
成为权限主体。安装并启动：

```sh
shasum -a 256 -c nova-v*-universal-apple-darwin-development-app.zip.sha256
unzip nova-v*-universal-apple-darwin-development-app.zip
ditto Nova.app /Applications/Nova.app
open -gj -b com.zenith.nova
```

请独立于 Bodhi 和插件下载的 CLI 安装这个应用。把它放在 `/Applications/Nova.app`（或
`~/Applications/Nova.app`），不要放进 Bodhi.app 或插件目录。插件仍会下载 CLI 压缩包，在 macOS
上只把它当作连接器使用；安装或更新插件不会安装或更新 Nova.app。CLI 和应用请使用同一个当前
版本构建。

按上文[在 MCP 客户端中使用](#在-mcp-客户端中使用)的说明，用 `nova mcp` 配置 stdio MCP 客户端。
如果当前代码还没有发布应用压缩包，请分别构建两个 macOS 架构，合并成 universal 二进制，再用
[`package-development-app.sh`](packaging/macos/package-development-app.sh) 组装应用；该脚本
需要 universal 二进制和对应的 Cargo 版本号作为参数。

> [!WARNING]
> 这个应用压缩包**仅供开发使用**。它是 ad-hoc 签名，没有 Developer ID 签名，没有公证，也没有
> staple。Gatekeeper 可能会拦截它；换成签名不同的构建后，可能需要重新授予 TCC 权限。
> 现有的 universal CLI `.tar.gz` 仍然是 Homebrew 和 Bamboo 使用的受支持制品；应用 `.zip`
> 不会取代它。

## 权限与代码签名（macOS）

### 检查应用可用的交互方式

在设置某个应用或检查有哪些交互路径可用时，可以使用
`inspect_app`。它是可选的；普通的原生交互仍然从 `ax_read` 开始。

```json
{"app": "Slack"}
```

选择器接受正在运行的应用名称或 bundle identifier。完全匹配优先于部分匹配。省略 `app` 时，
会发现正在运行的 Electron、Chromium 和 CEF 候选应用，包括没有发现调试连接的应用。仅凭名称
不能确认运行时：Nova 会检查已知的框架目录及其可执行文件证据。未知或无法读取的应用包保持为
unknown。

默认结果包含应用身份、运行时、检查状态、当前可用的原生路径以及下一步建议。Nova 在内部查找
进程拥有的本地连接候选，调用方不需要查找或提供端口。仅用于诊断时可以使用：

```json
{"app": "com.example.application", "details": true}
```

详细输出包括应用包/运行时证据、进程启动身份、endpoint 来源和元数据校验。Nova 会检查所选应用
拥有的监听端口、能识别的调试参数；只有当 `--user-data-dir` 参数证明了配置文件位置时，才会
检查确切的 `DevToolsActivePort` 文件。它不会扫描配置文件内容，也不会返回完整的参数或环境变量。
即使操作系统参数列表里没有相应参数，以编程方式启用的端口也可以通过监听归属被发现。

`browser_endpoint_available` 表示只读元数据的浏览器握手成功；它**并不**附加浏览器工具、授予
授权，也不验证完整的 Chrome DevTools MCP 工具集。原生 `ax_read` 仍然使用辅助功能权限，结果会
在需要该权限时注明。Node inspector endpoint、不兼容的 endpoint、过期证据和不完整的检查都会
分别标明。没发现端口不能证明调试已关闭。某个应用能否启用调试、能否支持基于重启的改动，在针对
该应用验证之前都是未知的。

发现过程不会启动、聚焦、退出或重启应用，不会请求权限，不会修改应用包或参数，也不会开启调试
服务。网络请求只发往经过验证、由进程拥有的回环 socket，且只有 `/json/version`、
`Browser.getVersion` 和 `Target.getBrowserContexts`。不会枚举页面、执行脚本、发送输入或调用
`Browser.close`。HTTP 代理和重定向都被禁用；对外公布的 WebSocket 必须保持在同一个归属地址和
端口上。探测前后都会检查归属和启动身份。

一次调查总共允许 8 秒、16 个结果应用、每个应用 32 个进程、4 代辅助进程、每个应用 8 次
endpoint 探测、2 个有证据的配置文件。每次元数据探测的截止时间是 900 毫秒；HTTP 正文和
WebSocket 消息最大 32 KiB，WebSocket 交换总量最大 128 KiB、每次回复最多 16 帧。框架查找最多
检查应用 `Contents/Frameworks` 中的 64 个条目，以及每个已识别框架中最多 4 个版本目录。达到
限制或证据不可用时会报告为不完整，而不是悄悄声称应用不支持调试。并发调用会收到 busy 结果。
Windows/Linux 返回明确的 unsupported 结果；它们已有的原生工具不受影响。

在 macOS 上，常驻的桌面传输会保持进程主 run loop 运行，因此第一次检查之后启动或退出的应用会
出现或消失，无需重启 Nova 或其 MCP 宿主。重新启动某个应用会触发新的进程/启动时间和 endpoint
归属检查。这种清单刷新不需要屏幕录制或辅助功能权限。`mcp` 和 `--connect` 字节代理会在这个
桌面事件循环和初始化之前返回。

自动化测试使用伪造的应用包、进程记录和回环服务。被忽略的
`own_listener_and_process_start_identity_match` 测试只检查它自己的进程/监听端口。被忽略的
`e2e_app_inspection` 验收测试需要明确准备一个 bundle identifier 为 `dev.nova.acceptance.*` 的
应用，并设置 `NOVA_TEST_APP_BUNDLE_ID`；它永远不会默认去检查用户正在运行的应用。

`cargo test --test e2e_resident_app_inspection` 会运行一个单独的 macOS 回归测试，其测试二进制
拥有真正的进程主线程。在同一个常驻进程中，它先建立发现基线，启动一个唯一的临时 AppKit 应用，
检查其出现，退出它，检查其消失，再在重新启动时检查新的进程身份。随后它用一个内部随机分配的
监听端口模拟狭窄的 CDP 握手，再重复一遍；这个测试替身不是 Chromium，会被报告为未知运行时。
它不创建窗口，也不请求权限。真实 Electron/Chromium 生命周期的验收仍然是另一项检查。仅用于
测试的 `--without-main-loop` 参数是一个负面对照，用来复现旧的清单过期问题，预期会失败。

### 权限归属

*Nova.app 菜单在 v0.3.0 中为**预览**；真机验收记录在 [#34](https://github.com/bigduu/Nova/issues/34)。*
打包的 macOS 应用有一个 **Nova** 菜单栏入口。它把本地服务的 **Starting**、**Ready** 或
**Failed** 状态与**辅助功能**和**屏幕录制**权限分开显示。Ready 表示本地服务正在监听，并不代表
任一权限已授予或 Chrome 已配对。如果启动失败，菜单仍然可用并显示失败状态。重复启动会直接退出，
保留已有的服务进程。

启动和 **Refresh Status** 只检查当前权限，不会截图、打开设置、请求权限，也不会等待 Chrome
配对队列。**Request Accessibility…** 只请求读取和操作原生应用控件的权限。**Request Screen
Recording…** 只请求截图和屏幕文字识别的权限。每一部分还有各自的 **Open … Settings** 操作。
“Not granted” 可能表示还没有请求过权限，它无法区分这种情况和被拒绝。

修改权限后，选择 **Refresh Status** 并重试 Nova 工具；Bodhi 可以保持打开。截图会在下一次截图
请求时继续使用现有的辅助进程及其权限变更恢复机制；刷新本身既不会启动也不会重启截图辅助进程。
**Quit Nova** 会结束服务及其连接。需要时重新打开 Nova，并在客户端里只重连 Nova 这一个 MCP
服务。不会自动重放请求，也没有重启控制。

这个菜单只在 macOS 应用服务中提供。直连 stdio/HTTP 和连接器进程保持原有的传输方式，没有状态
菜单。这个界面并不保证替换/重新签名 Nova 或系统更新后权限仍然有效；那些仍属于单独的安装和
发布检查。

独立应用传输是推荐的权限模型：把**屏幕录制**和**辅助功能**授予 `Nova.app`，然后使用
`nova mcp`（或显式的 `nova --connect`）。连接器从不初始化 CoreGraphics 或 Accessibility，
因此 Bamboo、Claude Desktop 和终端都不再需要 Nova 的桌面权限。

升级 Bodhi 时，请保持 Nova.app 独立安装且不做改动。新的 Bodhi/插件连接器连接的是同一个由
应用持有的服务，所以连接器自己的构建/签名身份不会成为 Nova 的权限主体。这是关于桌面调用在哪里
执行的架构保证；签名安装和真实的 TCC 升级验收仍是单独的发布关卡。替换 Nova.app 本身、改变它的
签名，或者操作系统的权限决定，仍然可能需要重新授权。开发预览版是 ad-hoc 签名。

在系统设置中给 Nova 授权后，重试工具即可。如果 macOS 要求重启才能生效，退出并重新打开
**Nova.app**，然后在客户端里只重连 **Nova MCP 服务**，Bodhi 主窗口保持打开。连接器不会重放
被中断的请求，也不会在 Nova 退出后自动恢复 MCP 会话。不要为了修复这条托管的 Nova 路径去删除或
重新添加 Bodhi 的授权。

对于直连 stdio/HTTP 和源码开发模式，还有两点需要注意：

**按 Nova 的启动方式给负责的进程授权。**macOS TCC 可能把子进程算在负责它的父应用名下。旧的
直连 stdio MCP（参数列表为空）请给 Claude Desktop、Bamboo，或启动 Nova 的终端/IDE 授权。直接
启动的 CLI/HTTP 进程，macOS 则可能使用 Nova 二进制本身。如果给预期的宿主授权不起作用，请把
已安装的 `nova` 二进制（或 `target/release/nova`）作为后备，添加到*系统设置 → 隐私与安全性 →
屏幕录制*和*辅助功能*中。

**保持接受授权的那个进程的身份稳定。**如果 Nova 本身是权限主体，`cargo build` 产出的是 ad-hoc、
*由链接器签名*的二进制，其代码签名身份是内容哈希（`nova-<hash>`）。每次构建都会变化，所以直接
给二进制的授权会失效。在这种模式下开发时，请用一个稳定的自签名身份给 Nova 签名：

```sh
cargo build --release
./scripts/dev-codesign.sh --release   # 每次构建后都要重新签名
```

第一次运行会在你的登录钥匙串中创建 `Zenith Nova Code Signing` 身份（如果 codesign 弹窗，点一次
**始终允许**），并用固定的标识符（`com.zenith.nova`）给二进制签名。之后用同一证书重新签名的
构建，都能沿用直接给 Nova 的授权。给宿主应用的授权同样取决于宿主保持稳定的签名身份。

> **故障排查：`screenshot` 报 “wedged” / “busy” 截图错误。**
> 所有截图（以及窗口枚举）都在**同一个**按用户的守护进程中运行
> （`nova --capture-daemon`，通过 flock 选举，socket 为 `/tmp/nova-capture-<uid>-<hash>.sock`），
> 因为 `replayd` 是按**可执行文件路径**区分客户端的：两个同一二进制的 ScreenCaptureKit 客户端
> 会互相挤掉对方的 XPC 身份，导致每次新建流都卡死。截图超过 8 秒看门狗时守护进程会自行退出，
> 客户端会自动恢复：先杀掉并重启守护进程，第二次失败时 SIGKILL 所有 nova 截图进程并执行
> `killall -9 replayd`，卡死会自愈，无需手动处理。如果没有恢复：运行 `nova --selftest`（先在
> 一个牺牲子进程里探测 ScreenCaptureKit，再测试守护进程路径），并查看
> `/tmp/nova-capture-worker.log`（步骤跟踪）和 `/tmp/nova-capture-daemon.log`（守护进程
> stderr）。手动处理方法是杀掉持有流的进程（`pkill -f -- --capture-daemon`），而不是 replayd：
> 普通的 `killall replayd` 没有作用（replayd 会忽略 SIGTERM），而只要还有持有流的客户端存活，
> 即使 `killall -9 replayd` 也治不好卡死，客户端会重新连接并让新的 replayd 再次卡住。

## 坐标定位

*下面的虚拟光标属于 v0.3.0 中 Nova.app 的**预览**功能；真机验收记录在 [#70](https://github.com/bigduu/Nova/issues/70)。*在 macOS **Nova.app** 中，基于坐标的鼠标
移动、点击和滚动还会在给定的逻辑坐标处显示一个紫色虚拟箭头。点击光圈和滚动方向提示会在 400
毫秒后淡出；无操作 1.2 秒后箭头消失。这个面板会让鼠标输入穿透，也无法获得键盘焦点。前台输入
仍会移动真实指针；按 PID/后台投递保持原有行为。批量操作使用同一个原生后端。元素中心回退后的
内部指针还原，会让提示停留在那次点击处。这个反馈只表示进行了一次输入尝试，不代表目标确实接受
了输入。纯语义的 AX 操作和浏览器 DOM 操作不会推断光标位置；不带界面的直连传输和纯连接器不会
创建这个覆盖层。

只有当保留的光标窗口的所有者解析为同一个 Nova 可执行文件时，显示器和区域截图才会把它排除，
包括对已预热截图流的更新。其他应用的单窗口截图保持原有过滤。渲染、点击穿透、焦点/指针行为、
批量操作、冷/热截图排除和退出清理，仍需要在受控的真实桌面上验收；自动化测试无法证明这些图形
界面结果。多显示器和全屏行为也需要在实际设备上验证。

通用 LLM 根据缩小后的截图判断像素坐标，是误点的主要来源，所以主路径完全避开像素：

- **先 `ax_read`（不用图片）**：按确定的树顺序返回可操作的控件和不可操作的可读内容。macOS 上
  成功读取需要辅助功能权限，但不会接触 ScreenCaptureKit。`permission_denied` 表示需要修复这项
  授权，而不是让你去截图。
- **用新鲜的语义结果操作**：用返回的快照 ID 和节点 ID 调用 `ax_activate`。会先尝试原生 AX/UIA
  和浏览器 DOM 桥，再尝试重新验证过的元素中心点击。过期的代际会直接失败；每次激活尝试都会在
  分发给 provider 之前消耗代际，所以无论结果如何都请重新读取。
- **其次是 OCR**：当覆盖缺失或不完整、而缺的信息是渲染出的文字时，用聚焦窗口的 OCR，并用返回的
  文字中心点调用 `left_click(..., source="ocr_center")`。
- **最后才是截图/放大**：只对纯视觉状态或没有语义/文字表示的界面使用像素；坐标点击会报告
  `route=visual_coordinate`。截图标记和 `click_mark` 仍为兼容而保留。

当*确实*需要截图时，**所有点击/移动/滚动工具都使用上一张截图的像素空间**：服务会记住那一帧，
并把点击映射回真实屏幕，所以模型只需要“点它看到的地方”：
- `screenshot(window: "<name>")`：只截取单个窗口（匹配标题或应用名的子串），而不是整个显示器。
  图片更小更清晰，占用的上下文更少、缩放更少，精度更高。之后的点击会映射到这个窗口。
- `zoom_region(x, y, w, h)`：以原生分辨率放大上一张截图中的一个矩形（只截取这个矩形）。用于在
  不提供无障碍树的界面（画布、游戏、自定义视图）上看清小目标，这些地方只能靠坐标。图上会叠加
  带刻度的坐标网格，模型可以直接从坐标轴读出位置。

## 测试

测试分为快速、无副作用的测试（默认运行），以及有副作用的端到端测试（需手动开启，标记为
`#[ignore]`）。

### 默认：单元测试和无副作用的集成测试

```sh
cargo test
```

运行所有没有副作用、也不需要特殊权限的测试：

- 坐标缩放、按键/字符映射、组合键解析、批量操作（反）序列化和 MCP 工具注册的单元测试；
- `tests/e2e_interaction.rs`：截图到逻辑坐标的映射（通过 `CGDisplay`，不需要权限），以及一次
  **不破坏数据**的剪贴板往返测试（先保存再恢复剪贴板）。

CI 中的 macOS `test` 任务运行的就是这些（见 `.github/workflows/ci.yml`；该工作流还有 Windows
交叉检查和 Linux 无界面任务）。

### 端到端测试（`#[ignore]`）

这些测试要么会发送**真实的输入事件**（移动光标、点击、滚动，或向当前聚焦的窗口输入文字），要么
需要**屏幕录制**权限，所以不包含在 `cargo test` 中，必须手动开启。请在可以接受这些操作的桌面
会话中运行：

```sh
# 全部运行
cargo test -- --include-ignored

# 或者只运行一个
cargo test --test e2e_input mouse_move_roundtrips_through_cursor_position -- --ignored
```

| 测试（文件） | 作用 | 需要 |
| --- | --- | --- |
| `semantic_snapshot_reads…`（`e2e_ax_read`） | 通过 AX/UIA 解析并读取当前聚焦或 `NOVA_AX_WINDOW` 指定的应用，不截取像素 | 辅助功能 / 已登录的 UIA 桌面 |
| `mouse_move_roundtrips…`（`e2e_input`） | 移动光标，再通过 `cursor_position` 读回并断言位置；会还原光标 | 辅助功能 |
| `click_events_post…`（`e2e_input`） | 在空白的桌面角落左键/右键/双击（用 Esc 关闭菜单） | 辅助功能 |
| `scroll_events_post…`（`e2e_input`） | 发送垂直滚动事件 | 辅助功能 |
| `type_text_posts…`（`e2e_input`） | **向当前聚焦的窗口输入文字** | 辅助功能 |
| `open_application_launches…`（`e2e_input`） | 启动/聚焦“系统设置” | — |
| `list_windows_returns…`（`e2e_input`） | 枚举屏幕上的窗口 | 屏幕录制 |
| `e2e_capture_display_returns_valid_jpeg`（`e2e_screenshot`） | 截取显示器并检查 JPEG | 屏幕录制 |
| `e2e_capture_dims_match_target_dims_contract`（`e2e_screenshot`） | 断言截图尺寸与点击坐标映射一致 | 屏幕录制 |
| `e2e_window_screenshot_produces_view_frame`（`e2e_screenshot`） | 截取一个窗口并校验其 view-frame 元数据 | 屏幕录制 |
| `ocr_recognizes_text_on_the_display`（`e2e_ocr`） | 对实时截图运行 Apple Vision OCR，断言识别到文字且行中心点在范围内 | 屏幕录制 |
| `daemon_*` / `client_*` / `concurrent_*`（`e2e_capture_worker`） | 共享截图守护进程：截图、杀掉后重启恢复、多客户端并发、出错后仍能存活 | 屏幕录制 |
| `legacy_pipe_protocol_still_served`（`e2e_worker`） | 旧的 `--capture-worker` 管道协议，经代理转入守护进程 | 屏幕录制 |
| `stdio_server_completes_handshake_and_lists_tools`（`e2e_stdio`） | 端到端测试 stdio（JSON-RPC）传输 | — |
| `safari_opens_google_and_nova_reads_the_homepage`（`e2e_safari_google`） | 启动 Safari、打开 Google，并通过 Nova 读取页面 | 网络 + 屏幕录制 + 辅助功能 |

> `mouse_move_roundtrips…` 证明 macOS 指针事件发送和光标读回在逻辑坐标下能往返一致。未被忽略
> 的交互测试覆盖截图到逻辑坐标的计算，实时截图测试则覆盖截图尺寸约定。
>
> `e2e_capture_worker` 请**单线程**运行（`-- --ignored --test-threads=1`）：这些测试共用一个
> 守护进程/socket。

`list_applications_returns_app_bundles`（在 `e2e_input` 中）**没有**被忽略：它只读取 Spotlight，
在没有 Spotlight 的 CI 主机上也能容错。

### 代码检查与格式化

```sh
cargo fmt --all -- --check
cargo clippy --all-targets
```

## 发布（维护者）

版本 tag 通过 `.github/workflows/release.yml` 驱动整个流程。工作流只解析一次 tag，核对它与
`Cargo.toml` 和触发事件的提交是否一致，并让每个从源码构建的任务都检出这个不可变的提交。它构建
并冒烟测试 universal macOS CLI 和仅供开发的 Nova.app，用这些资产创建 Release，然后按顺序由
后续任务附加 Windows x86_64/ARM64 原生压缩包和 Bamboo 插件包。CLI `.tar.gz` 的文件名和校验值
输出保持不变，供 Homebrew 和 Bamboo 插件清单使用。

打 tag 之前先运行无副作用的发布检查：

```sh
scripts/test-release-workflow.sh
```

当前已发布版本是 **v0.3.0**。创建下一个发布 tag 之前，请确认 tag 与版本号一致。发布 tag
必须禁止强制更新；工作流也会按 tag 串行运行，并在第一次上传前重新校验 tag。在所有正式分发关卡
完成之前，Nova.app 资产必须一直标注为**仅供开发使用（DEVELOPMENT ONLY）**：

- 先签内嵌代码、再签外层应用，使用 Developer ID Application 身份并启用 hardened runtime；
- 把分发制品提交给 Apple 公证服务，并验证返回的公证票据；
- 把票据 staple 到应用上，并用 `codesign` 和 `spctl` 验证；
- 用 macOS audit token 和指定的代码要求来认证本地 MCP 和 Chrome 桥的对端，而不是只依赖同 UID
  的 socket；
- 在真实安装的 Chrome 上运行打包好的原生宿主和扩展，覆盖配对、导航撤销、过期快照和断开连接；
- 在把发布工作流视为正式的供应链边界之前，把第三方 GitHub Actions 固定到完整的提交 SHA；
- 在干净的 Apple Silicon 和 Intel macOS 14+ 机器上冒烟测试启动、升级、`nova --connect`、
  屏幕录制和辅助功能授权。

不要把 ad-hoc 签名的应用预览版描述为可用于生产的 macOS 应用。

## 许可证

[MIT](LICENSE) © bigduu
