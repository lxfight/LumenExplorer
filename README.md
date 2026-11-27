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
- **长期运行的资源控制**：快速滚动丢弃图标任务时同步释放待处理记录，避免持续刷新；每个标签页最多保留 256 条导航历史。目录切换复用滚动和入场动画状态，避免旧状态随导航次数累积。回收站还原在后台执行，等待系统或磁盘响应时界面仍可操作；还原期间暂缓其他撤销和重做。

### 文件操作与 Shell 集成
- **Fluent 2 自绘右键菜单**：集成顶部快捷图标栏、基于注册表 `ShellNew` 的「新建」子菜单，以及按需懒加载的完整 Windows 经典 Shell 菜单（`Shift+右键` / `Shift+F10`）。
- **选择新建类型**：点击顶部「新建」或打开文件夹空白处的右键「新建」子菜单，可选择文件夹、文本文档和本机注册的文档模板。支持读取 WPS / Office 的嵌套 `ShellNew` 注册项；Word、Excel、PowerPoint 等类型是否出现，取决于本机的模板注册与模板文件。
- **复制、剪切与粘贴**：`Ctrl+C` / `Ctrl+X` / `Ctrl+V` 支持复制和移动文件，即使剪贴板只有文件、没有文本，也能粘贴到当前目录。同目录复制会生成副本，剪切粘贴成功后清除文件剪贴板；截图可粘贴为 PNG。地址栏、搜索框和重命名框保留文本粘贴功能。
- **后台文件操作与撤销**：`IFileOperation` 运行在独立后台 STA 线程并实时显示进度，支持多级撤销、多选批量重命名，以及重命名和移动操作的重做。按 `Delete` 或 `Ctrl+D` 将选中项目移到回收站后，可用 `Ctrl+Z` 按原路径还原；回收站中的其他同名项目不会被一并还原。还原失败时保留撤销记录，便于重试。
- **内置压缩与解压**：调用系统内置 `tar.exe` 异步压缩为 ZIP 或解压 `.zip` / `.7z` / `.rar` / `.tar` / `.gz` / `.zst` 归档。
- **OLE 拖放与实时监听**：支持将选中文件以 Windows 标准文件列表拖到其他应用，支持同盘移动 / 跨盘复制与修饰键反馈；通过 `ReadDirectoryChangesW` 自动去抖并合并目录变更刷新。

### 多后端搜索
- **Everything IPC v2**：自动检测运行中的 Everything，按当前目录作用域实现毫秒级索引检索。
- **Windows Search**：支持在设置中切换优先查询系统 `SystemIndex`。
- **内置并行搜索**：对未索引目录自动回退至多线程递归搜索，支持 `ext:`、`size:`、`date:`、`type:`、`name:` 与 `*` / `?` 通配符查询语法。

## 常用快捷键

### 文件操作

| 快捷键 | 功能 |
| --- | --- |
| `Ctrl+C` 或 `Ctrl+Insert` | 复制选中项目 |
| `Ctrl+X` | 剪切选中项目 |
| `Ctrl+V` 或 `Shift+Insert` | 粘贴文件；剪贴板图片保存为 PNG |
| `Ctrl+Shift+C` | 复制选中项目的路径 |
| `Ctrl+Shift+N` | 新建文件夹 |
| `F2` | 重命名；多选时批量重命名 |
| `Delete` 或 `Ctrl+D` | 将选中项目移至回收站 |
| `Shift+Delete` | 永久删除选中项目 |
| `Ctrl+Z` | 撤销，包括还原本窗口删除到回收站的项目 |
| `Ctrl+Y` 或 `Ctrl+Shift+Z` | 重做重命名、移动操作 |
| `Ctrl+A` | 全选 |
| `Ctrl+Shift+I` | 反选 |
| `Alt+Enter` 或 `Alt+I` | 查看属性 |
| `Shift+右键` 或 `Shift+F10` | 打开经典 Windows Shell 右键菜单 |

光标在地址栏、搜索框或重命名框中时，复制、剪切和粘贴快捷键用于文本编辑。

### 导航与标签页

| 快捷键 | 功能 |
| --- | --- |
| `Ctrl+L`、`Alt+D` 或 `F4` | 聚焦地址栏 |
| `Ctrl+F`、`Ctrl+E` 或 `F3` | 聚焦搜索框 |
| `Alt+←` | 后退 |
| `Alt+→` | 前进 |
| `Alt+↑` | 返回上级目录 |
| `Backspace` | 默认返回上级目录，可在设置中改为后退 |
| `Ctrl+Shift+G` | 跳转到剪贴板中的本地路径 |
| `Ctrl+T` | 新建标签页 |
| `Ctrl+W` | 关闭当前标签页 |
| `Ctrl+Shift+T` | 恢复最近关闭的标签页 |
| `Ctrl+1` 至 `Ctrl+8` | 切换到对应序号的标签页 |
| `Ctrl+9` | 切换到最后一个标签页 |
| `Ctrl+Tab` | 切换到下一个标签页 |
| `Ctrl+Shift+Tab` | 切换到上一个标签页 |
| `Ctrl+N` | 新建窗口 |
| `Ctrl+Shift+O` | 将已打开的系统资源管理器窗口吸收为标签页 |

### 视图与设置

| 快捷键 | 功能 |
| --- | --- |
| `Space` | 快速预览（Quick Look） |
| `Ctrl+Shift+E` | 切换双栏分屏视图 |
| `F5` | 刷新 |
| `Ctrl+H` | 显示或隐藏隐藏文件 |
| `Ctrl+Shift+D` | 切换主题模式 |
| `Ctrl+,` | 打开设置中心 |
| `Ctrl+Alt+E` | 全局呼出窗口（需在设置中开启） |

## 常用文件操作

- **新建文档**：进入本地文件夹，点击顶部「新建」后选择类型，或在空白处右键 →「新建」→ 选择类型。创建后可直接重命名；`Ctrl+Shift+N` 仍用于快速新建文件夹。「新建」「查看」「排序方式」的子菜单会根据窗口空间向右或向左展开。
- **粘贴文件**：选中项目后按 `Ctrl+C` 或 `Ctrl+X`，进入目标文件夹再按 `Ctrl+V`。光标在地址栏、搜索框或重命名框中时，快捷键用于文本编辑；点击文件列表可返回文件操作。
- **拖到其他应用**：按住文件行、名称或图标拖到目标应用的文件接收区域；多选后可一起拖出。目标应用需支持 Windows 文件拖放，文件是否发送由目标应用决定。
- **撤销删除**：在同一窗口中按 `Delete` 或 `Ctrl+D` 删除项目，操作完成后按 `Ctrl+Z` 还原到原目录。撤销记录保存在当前窗口内存中，关闭窗口后不会保留。`Shift+Delete` 永久删除、清空回收站，以及未进入回收站的删除操作不支持此方式还原。

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

### 压力测试

压力测试需手动运行，普通 `cargo test` 和 CI 会跳过。测试会在 `target/stress-runs/<时间戳>/` 创建 30,000 个测试文件，执行大目录枚举、搜索取消、500 次复制/重命名/移动/回收站删除/还原操作，再持续运行 5 分钟的导航、四种视图切换和缩略图请求负载。文件操作仅针对测试目录中的文件，不保存用户配置。

```powershell
cargo test --locked stress_soak -- --ignored --exact app::stress::stress_soak --test-threads=1 --nocapture

# 可选：将持续负载延长至 1 小时（另有数据准备和恢复阶段）
$env:LUMEN_STRESS_SECONDS = '3600'
cargo test --locked stress_soak -- --ignored --exact app::stress::stress_soak --test-threads=1 --nocapture
Remove-Item Env:LUMEN_STRESS_SECONDS
```

最近一次报告目录记录在 `target/stress-runs/latest.txt`。目录中的 `report.json` 包含耗时、内存、句柄、GDI 对象和界面状态数量；测试数据保留供复查。帧耗时测量应用 UI 处理和 CPU 网格生成，不包含 GPU 绘制，不能当作实际窗口帧率。短时测试也不能代替数天运行验证。

本机测试结果见 [2026-11-27 压力测试报告](STRESS_TEST_REPORT.md)。

### 常用命令行参数

- `--new-window`：强制打开新窗口（绕过单实例转发）
- `--background`：静默启动到系统托盘
- `/select,<path>`：打开目录并定位选中指定文件
- `/e,<path>` / `/root,<path>`：兼容 Windows 资源管理器路径参数

用户配置与窗口状态默认保存于 `%APPDATA%\Lumen\`。

## License

本项目基于 [MIT](LICENSE) 协议开源。
