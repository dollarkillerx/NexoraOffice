# G7：构建与打包

Phase 0 的第七道关卡。要回答的问题：三个平台上能不能构建、打包，许可证检查能不能通过。

还没有正式的应用，所以现在被打包的是 G1 的输入法 Demo。换成正式应用只需要改 `common.sh` 开头的四行和 `windows.ps1` 开头的两行。

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

Apple M6，macOS 27.0.1，2026-11-22，版本 0.0.1。

| 项目 | 结果 |
| --- | --- |
| 可执行文件，`dist` 配置 | 17.3 MB |
| 可执行文件，普通 `release` 配置 | 22.7 MB |
| `.dmg` | 9.3 MB |
| 依赖的非系统动态库 | 无 |
| 临时签名校验 | 通过 |
| `.dmg` 校验 | 通过 |
| 从挂载的 `.dmg` 启动 | 能启动 |
| 首次 `dist` 构建 | 约 52 秒 |

### 许可证，已在本机验证

六个目标平台合计 485 个第三方包，全部在允许列表内。

需要知道的三点：

- **Slint** 提供 GPL-3.0、商业许可和免版税许可三选一，选哪一项取决于本项目自己的许可证。
- **Boost 许可（BSL-1.0）** 有两个包在用。它是宽松许可，已加进允许列表。
- **Unicode-3.0** 是 ICU4X 的数据表使用的许可，27 个包。

`licenses.py` 只检查许可证声明，不读许可证全文，也不查安全公告。这两件事由持续集成里的 cargo-deny 完成。

### Windows 和 Linux，尚未验证

`linux.sh`、`windows.ps1` 和持续集成工作流都还没有运行过。它们需要在对应系统上执行，最直接的方式是推送到 GitHub 让工作流跑一遍。在那之前，这道关卡不能算通过。

## 还没有做的

- **正式签名与公证**。macOS 现在是临时签名，只能在构建它的机器上直接运行，别的 Mac 会被系统拦截。需要 Apple Developer ID 证书并提交公证。Windows 的可执行文件没有签名，下载后会被 SmartScreen 警告。
- **Windows 安装程序**。目前只有压缩包，可执行文件也没有嵌入图标。
- **macOS 图标留白**。现在用的是通用图标，没有按 macOS 的图标规范另出一版。
- **许可证全文**。随包分发的只有包名、版本和许可证名称的清单。多数许可证要求附带全文，正式发布前要补上。
- **应用标识**。`dev.aster.office.preview` 是占位符，要换成我们自己域名下的标识。
- **内置字体**。还没有打进包里，所以体积数字不含那 45 到 60 MB。
- **自动更新**和**通用二进制**（同时支持 Intel 和 Apple 芯片的 Mac）。
