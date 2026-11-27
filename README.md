# Lumen

基于 Rust 构建的 **Windows 11 现代极速文件工作台（Modern High-Speed File Workbench）**。

Lumen 的设计理念是**与 Windows 原生资源管理器安全共存、分工互补**：随手查看单个文件夹交给系统资源管理器，高频的**查找、整理、双栏比对、快速预览与批处理工作流**交给 Lumen。依托真实 Windows Shell COM 接口（`IShellFolder`、`IFileOperation`、`IContextMenu`、`OLE` 拖放与 `ReadDirectoryChangesW`）与自绘 Fluent 2 GPU 界面，兼顾原生兼容性、现代审美与零卡顿性能。

## 安装与卸载

Lumen 通过标准 Windows 安装包分发（`LumenSetup-<version>-x64.exe`，基于 Inno Setup 6）：

- **默认每用户安装（无需管理员权限）**：安装至 `%LOCALAPPDATA%\Programs\Lumen`（也可在向导中切换为「为所有用户安装」），提供标准「设置 → 应用」卸载入口。
- **可选且 100% 可逆的桌面集成**（绝不劫持系统默认资源管理器 `Folder\shell\open` 关联）：
  - 文件夹、目录空白处与驱动器右键菜单「使用 Lumen 打开」
  - 添加到 `PATH` 环境变量（可在任意终端直接运行 `lumen .` 打开当前目录）
  - 可选开机静默启动到系统托盘（`--background`）
  - 正确绑定 `AppUserModelID`（`Lumen.Explorer`），任务栏固定与窗口分组无异常
- **内置自动更新与完整性校验**：通过 Windows 原生 WinHTTP（自动继承系统代理与 TLS 证书）静默检查 GitHub Releases；下载安装包后自动核对 `SHA256SUMS.txt` 摘要，校验通过后静默升级并自动重启（`/SILENT /RELAUNCH`），可在「设置 → 关于与更新」中手动检查或跳过版本。
- **安全无缝升级与卸载**：安装包在升级或卸载前会通过单实例 IPC（`lumen.exe --quit`）通知常驻托盘的实例保存配置并优雅退出，卸载时自动清理右键菜单、自启项与 `PATH` 记录，并询问是否保留 `%APPDATA%\Lumen` 用户配置。

> `lumen.exe` 主程序本身**零注册表写入**、**无低级键盘钩子**；所有可选系统集成均由安装程序统一安装与卸载清理。

## 核心特性

### 界面与原生体验
- **Fluent 2 自绘设计**：支持亮色 / 暗色主题（跟随系统或手动切换）、可选 Windows 11 Mica 背景材质与分层物理动效。
- **原生窗口集成**：无边框自绘标题栏，保留原生 `WM_NCHITTEST`（支持 Snap Layouts / Aero Snap）、高 DPI 逻辑坐标持久化与多屏边界校正。
- **单实例与系统托盘**：支持单实例 Named Pipe IPC 毫秒级转发、托盘常驻与标准全局快捷键呼出（`Ctrl+Alt+E`），支持一键吸收已打开的系统资源管理器窗口（`Ctrl+Shift+O`）。

### 导航与工作流
- **多标签与双栏分屏**：支持标签页拖拽排序、中键关闭、恢复已关闭标签（`Ctrl+Shift+T`）、会话持久化，以及左右独立标签页的双栏分屏视图（`Ctrl+Shift+E`）。
- **智能地址栏与路径跳转**：面包屑支持子目录快速下拉，地址栏支持 `~`、环境变量展开、子目录异步补全，以及直接输入 `cmd` / `powershell` / `pwsh` / `wt` 启动终端；复制本地路径后支持 `Ctrl+Shift+G` 一键直达。
- **完整导航窗格**：涵盖主页、最近使用、OneDrive、快速访问（支持拖放固定）、此电脑（含容量条与移动设备弹出）、WSL (Linux)、网络与回收站。

### 视图与快速预览
- **多种视图模式**：详细信息、列表、平铺、大图标，支持表头排序（Windows 原生区域拼音 + 自然数字排序）、可折叠分组、每文件夹独立记忆视图与列宽。
- **Quick Look 快速预览**：选中文件按 `Space` 即可异步预览高清图片或自动解码的文本/代码文件（UTF-8 / UTF-16 / GBK），预览时可直接用 `↑` / `↓` 切换项目。
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

## 演进路线（现代文件工作台方向）

- [x] **v0.2 安装包与工作台基建**：Inno Setup 6 标准安装/卸载体系、优雅退出 IPC（`--quit`）、单实例互斥锁与 CI/Release 自动打包
- [ ] **全盘瞬移命令面板（`Ctrl+K` / `Ctrl+P`）**：将 Everything IPC v2 升级为全局文件/目录/标签/命令毫秒级跳转面板
- [ ] **文件夹真实体积内联可视化**：列表内异步计算文件夹大小与空间占比条
- [ ] **双栏目录对比与一键同步**：高亮左右差异、过滤新增与修改、支持单向/双向同步
- [ ] **工作区（Workspaces）与文件暂存架（File Shelf）**：一键切换整套项目标签组、跨目录收集文件统一处理

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
├── platform.rs      # Win32 窗口集成、DWM/Mica、系统托盘、互斥锁、全局快捷键与 IME 状态
├── theme.rs         # Fluent 2 设计令牌、系统字体加载与样式配置
├── app/             # 应用状态机（状态、命令分发、输入处理与后台任务协调）
├── core/            # 数据模型、排序分组、标签页、设置与窗口几何持久化
├── shell/           # Windows Shell 集成（枚举、文件操作、图标、剪贴板、拖放、搜索、监听等）
└── ui/              # 界面组件（标题栏、导航窗格、虚拟化文件列表、右键菜单、快速预览、设置等）
installer/
├── lumen.iss        # Inno Setup 6 安装包脚本（每用户默认安装、可选右键/PATH/自启、卸载清理）
├── build.ps1        # 一键构建 Release 与安装包脚本
├── languages/       # 简体中文安装向导语言包
└── assets/          # 安装向导 HiDPI 视觉素材
assets/              # 应用图标与 Windows 资源清单
```

## 构建与打包

### 环境要求
- Windows 11 (x86_64)
- Rust stable（支持 Edition 2024）
- Visual Studio MSVC 工具链与 Windows 11 SDK
- [Inno Setup 6](https://jrsoftware.org/isinfo.php)（仅打包安装程序需要，可通过 `winget install --id JRSoftware.InnoSetup` 安装）

### 开发编译与检查

```powershell
# 运行测试、格式检查与 Lint
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings

# 构建 Release 可执行文件
cargo build --release
```

### 构建 Windows 安装包

```powershell
# 编译 Release 并生成 dist\LumenSetup-<version>-x64.exe 与 SHA-256 校验文件
powershell -ExecutionPolicy Bypass -File installer\build.ps1
```

### 常用命令行参数

- `lumen .` / `lumen <path>`：在已运行的 Lumen 实例中以新标签打开指定目录
- `--new-window`：强制打开新窗口（绕过单实例转发）
- `--background`：静默启动到系统托盘
- `--quit`：通知运行中的实例（含托盘常驻）保存配置并安全退出
- `/select,<path>`：打开目录并定位选中指定文件
- `/e,<path>` / `/root,<path>`：兼容 Windows 资源管理器路径参数

用户配置与窗口状态默认保存于 `%APPDATA%\Lumen\`。

## License

本项目基于 [MIT](LICENSE) 协议开源。
