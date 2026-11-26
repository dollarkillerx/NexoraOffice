# G7：构建与打包

Phase 0 的第七道关卡。要回答的问题：三个平台上能不能构建、打包，许可证检查能不能通过。

现在被打包的是正式应用 `aster-shell`，外加它旁边必须有的 MCP 桥接程序 `aster-mcp`（`aster-mcp-bridge` 包，`aster_mcp::setup::bridge_path` 在应用可执行文件所在的目录里找它，找不到时 AI/MCP 设置会显示“找不到 aster-mcp 程序”）。PDF 查看器的沙箱子进程是同一个可执行文件加 `--pdf-worker` 重新启动，不需要额外文件。包名写在 `common.sh` 开头，`windows.ps1` 开头另有一份，要保持一致。

包内的可执行文件统一叫 `aster-office`（macOS 的 `Contents/MacOS/aster-office`，Linux 的 `/usr/bin/aster-office`，Windows 的 `aster-office.exe`），`aster-mcp` 与它放在同一个目录。

## 内容

| 文件 | 作用 |
| --- | --- |
| `macos.sh` | 生成 `Aster Office.app` 和 `.dmg` |
| `linux.sh` | 生成 `.tar.gz` 和 `.deb` |
| `windows.ps1` | 生成 `.zip` |
| `licenses.py` | 按 `deny.toml` 的允许列表检查每个依赖的许可证，并生成随包分发的第三方包清单 |
| `common.sh` | 产品名、版本号读取、构建配置 |

产物都在 `target/dist-packages/`，使用 `dist` 构建配置：在 release 的基础上打开 thin LTO、单代码生成单元并去掉符号。

```sh
packaging/macos.sh
packaging/linux.sh
pwsh packaging/windows.ps1
python3 packaging/licenses.py --check
```

## 结果

### macOS，已在本机验证

Apple M6，macOS 27.0.1，2026-11-26，版本 0.0.1。

| 项目 | 结果 |
| --- | --- |
| `aster-office`，`dist` 配置 | 50.1 MB（51,289,040 字节） |
| `aster-mcp`，`dist` 配置 | 364 KB |
| `.dmg` | 25.3 MB |
| 依赖的非系统动态库（两个可执行文件） | 无 |
| `Info.plist`（含文档类型片段）`plutil -lint` | 通过 |
| 在没有应用运行时从包内运行 `aster-mcp` | 打印“Aster Office is not running or MCP is off”，退出码 3 |
| 临时签名校验（先签 `aster-mcp`，再签整个包，`codesign --verify --deep --strict`） | 通过 |
| `.dmg` 校验 | 通过 |
| 从挂载的 `.dmg` 启动图形界面 | 能启动；系统发来的 Markdown、PDF、xlsx、pptx 都能打开，PDF 的沙盒子进程从包内启动 |
| 包内的 AI/MCP 设置页 | 未测（`aster-mcp` 已在包内，能运行） |
| 首次 `dist` 构建（两个包） | 约 2 分 48 秒 |

### 许可证，已在本机验证

六个目标平台合计 622 个第三方包（`licenses.py` 按整个工作区解析依赖，已包含 `aster-shell` 和 `aster-mcp-bridge`），全部在允许列表内，每一个都可以与本项目的 AGPL-3.0 许可证合并使用。

需要知道的三点：

- **Slint** 提供 GPL-3.0、商业许可和免版税许可三选一。本项目是 AGPL-3.0，只能选 GPL-3.0 这一项：免版税许可限制了使用领域，与 AGPL 不相容。GPL-3.0 与 AGPL-3.0 的代码可以链接在一起，两份许可证的第 13 条专门允许这样做。
- **Boost 许可（BSL-1.0）** 有两个包在用。它是宽松许可，已加进允许列表。
- **Unicode-3.0** 是 ICU4X 的数据表使用的许可，27 个包。

`licenses.py` 只检查许可证声明，不读许可证全文，也不查安全公告。这两件事由持续集成里的 cargo-deny 完成。

### Windows 和 Linux，尚未验证

`linux.sh`、`windows.ps1` 和持续集成工作流都还没有运行过，切换到 `aster-shell` 和 `aster-mcp` 之后的改动同样没有验证。`linux.sh` 的 `.desktop` 里加了 `MimeType=`（只列出能打开的格式，不抢默认应用）；`.deb` 的 `Depends:` 没有对照 `aster-shell` 实际链接的库核对过（Skia 渲染器可能还需要 `libegl1`/`libgl1` 一类的库），需要在 Linux 上用 `ldd` 确认。它们需要在对应系统上执行，最直接的方式是推送到 GitHub 让工作流跑一遍。在那之前，这道关卡不能算通过。

## 还没有做的

- **正式签名与公证**。macOS 现在是临时签名，只能在构建它的机器上直接运行，别的 Mac 会被系统拦截。需要 Apple Developer ID 证书并提交公证。Windows 的可执行文件没有签名，下载后会被 SmartScreen 警告。
- **Windows 安装程序**。目前只有压缩包，可执行文件也没有嵌入图标。
- **macOS 图标留白**。现在用的是通用图标，没有按 macOS 的图标规范另出一版。
- **许可证全文**。随包分发的只有包名、版本和许可证名称的清单。多数许可证要求附带全文，正式发布前要补上。
- **应用标识**。`dev.aster.office.preview` 是占位符，要换成我们自己域名下的标识。
- **内置字体**。还没有打进包里，所以体积数字不含那 45 到 60 MB。
- **自动更新**和**通用二进制**（同时支持 Intel 和 Apple 芯片的 Mac）。
