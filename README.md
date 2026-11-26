# Aster Office

用 Rust 编写的原生、离线优先的开源办公套件，面向 macOS、Windows 和 Linux。目前处于 Phase 0：用一组小型验证工程回答“这条技术路线走不走得通”，还没有可用的产品。`crates/aster-shell` 是应用外壳的原型，可以打开看界面；Markdown 标签能打开、编辑、保存真实的文本文件（100 MB 也可以），源码有语法高亮，可以分屏预览（预览按中日文禁则断行，能选中复制），可以查找替换；侧边栏列出最近打开的文件和一个打开的文件夹。Excel（.xlsx）和 PowerPoint（.pptx）文件可以打开查看：工作簿显示网格、数字格式、单元格样式、合并单元格、冻结窗格和图片，公式由 formualizer 重新计算；演示文稿按自己的背景、主题颜色和字体显示幻灯片，包括图片、表格和演讲者备注，也可以编辑（文字和格式、形状和图片、幻灯片的增删和排序、备注、放映）并保存成 .pptx，同样只改动变了的部分。工作簿可以编辑（输入、选区、复制粘贴、撤销重做、增量重算、排序和筛选、批注、下拉列表、复制工作表）并保存成 .xlsx，保存时只改动变了的单元格；开始页的 Sheets 和 Slides 卡片新建空白文档。PDF 文件可以在标签里查看（翻页、缩放、缩略图），解析和渲染在一个受限的子进程里进行：限内存、限时间，macOS 上还在沙盒里，打开恶意文件也不会拖垮程序。Word 文档（.docx）可以打开、分页显示、编辑并保存（正文、表格、页眉页脚、脚注），保存时只改动变了的段落；可以插入表格、图片、链接、脚注和页码，增删表格的行列、合并拆分单元格、拖线改列宽行高，改图片的环绕方式并拖动，显示修订和批注并接受或拒绝修订，导出成文字可选的 PDF；开始页的 Writer 卡片新建空白文档。

目前做到了哪里、哪些分支在进行中、哪些还没验证，见 [docs/progress.md](docs/progress.md)。

## 仓库结构

| 路径 | 内容 |
| --- | --- |
| `crates/aster-shell` | 应用外壳原型：窗口、标签、工具栏、侧边栏、设计令牌、界面语言。说明里写明了哪些是占位 |
| `crates/aster-surface` | 文档表面：自绘的纯文本表面、只排可见部分的大文件文本视图、Markdown 预览的排版和绘制、工作表网格视图和幻灯片视图，以及接入窗口的胶水，G1 关卡也用它 |
| `crates/aster-text` | 文字排版：G3 的中日文断行器（禁则、行尾标点、挤压、中西文间距），在 Parley 整形之上排段落，命中测试和选区；预览用它 |
| `crates/aster-markdown` | Markdown 编辑器的文档模型：文本、选区、编辑命令、撤销历史、文件编码与换行符；增量语法高亮和预览的块结构（用 `pulldown-cmark` 解析） |
| `crates/aster-storage` | 原子写入和崩溃恢复（命令日志、检查点、恢复记录），来自 G6 关卡 |
| `crates/aster-platform` | 系统的打开、保存和选择文件夹对话框，打开网页和邮件链接，在访达或文件管理器里显示文件，应用数据目录 |
| `crates/aster-ooxml` | Office Open XML：我们自己的包层（按需解压、安全限额、内容类型、关系，ADR 0001），XLSX、PPTX 和 DOCX 的读取器和保存（未改动的部件原样拷贝，只重写变了的单元格、形状和段落） |
| `crates/aster-sheets` | Sheets 的模型：工作簿、稀疏单元格、行高列宽、合并单元格、数字格式、单元格样式、冻结窗格、图片锚点、共享公式展开，以及 `CalcEngine` 接口 |
| `crates/aster-calc` | 计算引擎的适配器：formualizer 接在 `CalcEngine` 后面（ADR 0002），只从我们的模型取输入 |
| `crates/aster-slides` | Slides 的模型：幻灯片、形状、文字框、填充和线条、图片、表格、背景、演讲者备注；母版、版式和主题由读取器解析好 |
| `crates/aster-writer` | Writer 的模型：段落、表格、分节、样式和列表，格式都已算好；编辑操作和撤销历史；位置是段落路径加字节偏移 |
| `crates/aster-layout` | Writer 文档的分页排版：行、表格、页面、页眉页脚和脚注，以及文字上每个位置在页面上的坐标 |
| `crates/aster-image` | 文档里的图片：PNG 和 JPEG 解码，有像素数上限，大图缩小 |
| `crates/aster-workspace` | 文档周围的文件：最近文件列表、侧边栏的文件夹树（一次读一个文件夹）、重启后记住的文件夹 |
| `spikes/` | Phase 0 的验证工程，每道关卡一个，各自带说明和结果 |
| `assets/brand/` | Logo、图标和品牌说明 |
| `docs/decisions/` | 选型决定的记录，一项一份 |
| `packaging/` | 三个平台的打包脚本和许可证检查 |
| `.github/workflows/` | 持续集成的工作流，开发阶段停用 |
| `scripts/ci.sh` | 在本机运行同样的检查 |
| `deny.toml` | 依赖与许可证策略 |

| 关卡 | 工程 | 回答的问题 |
| --- | --- | --- |
| G1 | `spikes/g1-ime` | 自绘的文档区能否使用中文和日文输入法 |
| G2 | `spikes/g2-frame` | CPU 渲染加整帧交给界面框架，滚动和打字是否够快 |
| G3 | `spikes/g3-cjk` | 能否自己实现中日文断行规则，并在三个系统上断出相同的行 |
| G4 | `spikes/g4-ooxml` | DOCX、XLSX、PPTX 打开再保存之后还剩多少，用哪个库做解析层 |
| G5 | `spikes/g5-formula` | 10 万个公式的工作簿里改一格，哪个公式引擎能在 100 毫秒内算对 |
| G6 | `spikes/g6-storage` | 进程被杀之后，命令日志能否把文档完整带回来 |
| G7 | `packaging/` | 三个平台能否构建、打包并通过许可证检查 |

## 构建与测试

需要 Rust，版本由 `rust-toolchain.toml` 指定，安装了 rustup 就会自动使用。

```sh
scripts/ci.sh             # 格式、clippy、测试、许可证，几秒到一两分钟
scripts/ci.sh --spikes    # 加上 G4 和 G5，首次要编译几分钟
scripts/ci.sh --package   # 加上本机的安装包
```

脚本做的就是下面这几条，遇到第一处失败即停止：

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
python3 packaging/licenses.py --check
```

**开发阶段的检查在本地跑。** GitHub 上的三个工作流（`CI`、`G4 OOXML round trip`、`G5 formula engines`）目前在仓库设置里停用了，推送和开 PR 都不会触发它们。合并进 `main` 之前，在本机运行 `scripts/ci.sh` 并通过。

这样做省掉了每次推送十几分钟的等待，代价是只检查了一个系统：另外两个系统上才会出现的编译错误和测试失败，本地发现不了。到需要三端验证的时候，重新启用即可，工作流文件没有改动：

```sh
gh workflow enable CI
gh workflow enable "G4 OOXML round trip"
gh workflow enable "G5 formula engines"
```

Linux 上还需要这些系统库：`libfontconfig1-dev libfreetype6-dev libxkbcommon-dev libwayland-dev libxcb-shape0-dev libxcb-xfixes0-dev`。

G4 和 G5 不在这个工作区里。它们要编译待评估的 OOXML 库和公式引擎，耗时几分钟，G4 还要占用 7 GB 以上内存，所以各是一个独立的工作区，有自己的 `Cargo.lock`。进到 `spikes/g4-ooxml` 或 `spikes/g5-formula` 里运行同样的命令即可。

## 版本与分支

**版本号**只写在一个地方：根目录 `Cargo.toml` 的 `[workspace.package]`。所有 crate 继承它，打包脚本从这里读取并写进安装包的文件名和元数据。

- 采用语义化版本。1.0 之前，破坏兼容的改动升第二位，其余升第三位。
- 发布对应一个 `v` 开头的标签，例如 `v0.1.0`。标签只打在 `main` 上。

**分支**：

- `main` 始终可以构建并通过全部检查。不直接向它提交。
- 工作在短分支上进行，`scripts/ci.sh` 在本机通过后合并。验证工程用 `spike/` 前缀，功能用 `feat/`，修复用 `fix/`。

**提交说明**用英文，第一行写这次改动做了什么，正文写为什么这样做、怎样验证的、还有什么没做。

**依赖**：`Cargo.lock` 入库，所有命令带 `--locked`。Slint、Parley、vello 的版本是有意固定的，升级要单独提交，并重跑受影响的关卡测试。新增依赖的许可证必须能与 AGPL-3.0 合并使用，并且已经在 `deny.toml` 的允许列表里；往列表里加新许可证是法务决定。

**不入库的东西**：`target/` 下的全部产物，包括安装包。安装包由打包脚本在本机生成；持续集成启用时由它生成并作为构建产物保存。

## 许可证

Aster Office 以 [GNU Affero 通用公共许可证第 3 版](LICENSE)（AGPL-3.0-only）发布。

这是一个著作权传染性的许可证：你可以自由使用、研究、修改和分发本软件，但分发修改后的版本，或者把修改后的版本作为网络服务提供给他人使用时，必须以同样的许可证公开对应的完整源代码。

界面框架 Slint 以它的 GPL-3.0 选项使用。公式引擎 formualizer 经 Apache Arrow 带进 `tiny-keccak`，它只有 CC0-1.0 许可证（公有领域贡献声明），项目负责人于 2026-11-23 决定接受，已写进 `deny.toml` 的允许列表。其余依赖都是宽松许可证，清单由 `python3 packaging/licenses.py --report <文件>` 生成。

Copyright (C) 2026 Aster Office contributors
