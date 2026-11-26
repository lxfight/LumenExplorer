# LumenExplorer

基于 Rust 构建的 Windows 11 现代文件资源管理器。依托真实 Windows Shell 命名空间与 COM 接口（`IShellFolder`、`IFileOperation`、`IContextMenu`、`OLE` 拖放与 `ReadDirectoryChangesW`），搭配完全自绘的 Fluent 2 界面与全异步后台架构，兼顾原生兼容性与流畅性能。

## 核心特性

### 界面与原生体验
- **Fluent 2 自绘设计**：支持亮色 / 暗色主题（跟随系统或手动切换）、可选 Windows 11 Mica 背景材质与分层物理动效。
- **原生窗口集成**：无边框自绘标题栏，保留原生 `WM_NCHITTEST`（支持 Snap Layouts / Aero Snap）、高 DPI 逻辑坐标持久化与多屏边界校正。
- **零注册表侵入**：不篡改 `HKCU\Software\Classes` 文件关联、不安装低级键盘钩子，与系统资源管理器安全共存。
- **单实例与系统托盘**：支持单实例 IPC 毫秒级转发、托盘常驻与标准全局快捷键呼出（`Ctrl+Alt+E`），支持一键吸收已打开的系统资源管理器窗口（`Ctrl+Shift+O`）。

### 导航与工作流
- **多标签与双栏分屏**：支持标签页拖拽排序、中键关闭、恢复已关闭标签（`Ctrl+Shift+T`）、会话持久化，以及左右独立标签页的双栏分屏视图（`Ctrl+Shift+E`）。
- **智能地址栏与路径跳转**：面包屑支持子目录快速下拉，地址栏支持 `~`、环境变量展开、子目录异步补全，以及直接输入 `cmd` / `powershell` / `pwsh` / `wt` 启动终端；复制本地路径后支持 `Ctrl+Shift+G` 一键直达。
- **完整导航窗格**：涵盖主页、最近使用、OneDrive、快速访问（支持拖放固定）、此电脑（含容量条与移动设备弹出）、WSL (Linux)、网络与回收站。

### 视图与快速预览
- **多种视图模式**：详细信息、列表、平铺、大图标，支持表头排序（Windows 原生区域拼音 + 自然数字排序）、可折叠分组、每文件夹独立记忆视图与列宽。
- **Quick Look 快速预览**：选中文件按 `Space` 即可异步预览高清图片或自动解码的文本文件（UTF-8 / UTF-16 / GBK），预览时可直接用 `↑` / `↓` 切换项目。
- **详细信息窗格**：支持宽度拖拽调整、缩略图展示、文件属性查看及异步计算文件夹大小。
- **按需渲染与低占用**：空闲无动画/后台任务时不重绘；图标与缩略图基于视口优先队列提取，采用 LRU 内存预算缓存；系统字体以内存映射共享加载。

### 文件操作与 Shell 集成
- **Fluent 2 自绘右键菜单**：集成顶部快捷图标栏、基于注册表 `ShellNew` 的「新建」子菜单，以及按需懒加载的完整 Windows 经典 Shell 菜单（`Shift+右键` / `Shift+F10`）。
- **后台文件操作与多级撤销**：`IFileOperation` 运行在独立后台 STA 线程并实时显示进度，支持多级撤销 / 重做（含回收站还原）、同目录副本粘贴、剪贴板截图保存为 PNG 与多选批量重命名。
- **内置压缩与解压**：调用系统内置 `tar.exe` 异步压缩为 ZIP 或解压 `.zip` / `.7z` / `.rar` / `.tar` / `.gz` / `.zst` 归档。
- **完整 OLE 拖放与实时监听**：支持同盘移动 / 跨盘复制与修饰键反馈；通过 `ReadDirectoryChangesW` 自动去抖并合并目录变更刷新。

### 多后端搜索
- **Everything IPC v2**：自动检测运行中的 Everything，按当前目录作用域实现毫秒级索引检索。
- **Windows Search**：支持在设置中切换优先查询系统 `SystemIndex`。
- **内置并行搜索**：对未索引目录自动回退至多线程递归搜索，支持 `ext:`、`size:`、`date:`、`type:`、`name:` 与 `*` / `?` 通配符查询语法。

## 常用快捷键

| 快捷键 | 功能 |
| --- | --- |
| `Ctrl+Alt+E` | 全局呼出窗口 |
| `Ctrl+Shift+O` | 吸收已打开的系统资源管理器窗口为标签页 |
| `Ctrl+Shift+G` | 跳转到剪贴板中的本地路径 |
| `Ctrl+Shift+E` | 切换双栏分屏视图 |
| `Space` | 快速预览（Quick Look） |
| `Ctrl+,` | 打开设置中心 |
| `Ctrl+T` / `Ctrl+W` / `Ctrl+Shift+T` | 新建标签 / 关闭标签 / 恢复已关闭标签 |
| `Ctrl+1..9` / `Ctrl+N` | 切换标签页 / 新建窗口 |
| `Ctrl+L` / `Alt+D` / `F4` | 聚焦地址栏 |
| `Ctrl+F` / `Ctrl+E` / `F3` | 聚焦搜索框 |
| `Alt+←` / `→` / `↑` / `Backspace` | 后退 / 前进 / 返回上级目录 |
| `F2` / `Delete` / `Shift+Delete` | 重命名（多选批量重命名）/ 移至回收站 / 永久删除 |
| `Ctrl+Z` / `Ctrl+Y` / `Ctrl+Shift+Z` | 撤销 / 重做 |
| `Ctrl+Shift+C` / `Ctrl+Shift+N` | 复制选中路径 / 新建文件夹 |
| `Shift+右键` / `Shift+F10` | 打开经典 Windows Shell 右键菜单 |
| `F5` / `Ctrl+H` / `Ctrl+Shift+D` | 刷新 / 切换隐藏文件显示 / 切换明暗主题 |

## 目录结构

```text
src/
├── main.rs          # 程序入口、命令行解析、单实例 IPC 与窗口初始化
├── platform.rs      # Win32 窗口集成、DWM/Mica、系统托盘、全局快捷键与 IME 状态
├── theme.rs         # Fluent 2 设计令牌、系统字体加载与样式配置
├── app/             # 应用状态机（状态、命令分发、输入处理与后台任务协调）
├── core/            # 数据模型、排序分组、标签页、设置与窗口几何持久化
├── shell/           # Windows Shell 集成（枚举、文件操作、图标、剪贴板、拖放、搜索、监听等）
└── ui/              # 界面组件（标题栏、导航窗格、虚拟化文件列表、右键菜单、快速预览、设置等）
assets/              # 应用图标与 Windows 资源清单
```

## 构建与运行

### 环境要求
- Windows 11 (x86_64)
- Rust stable（支持 Edition 2024）
- Visual Studio MSVC 工具链与 Windows 11 SDK

### 编译与检查

```powershell
# 构建 Release 版本
cargo build --release

# 运行
.\target\release\lumen.exe

# 运行测试、格式检查与 Lint
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

### 常用命令行参数

- `--new-window`：强制打开新窗口（绕过单实例转发）
- `--background`：静默启动到系统托盘
- `/select,<path>`：打开目录并定位选中指定文件
- `/e,<path>` / `/root,<path>`：兼容 Windows 资源管理器路径参数

用户配置与窗口状态默认保存于 `%APPDATA%\Lumen\`。

## License

本项目基于 [MIT](LICENSE) 协议开源。
