# Aster Office

用 Rust 编写的原生、离线优先的办公套件，面向 macOS、Windows 和 Linux。目前处于 Phase 0：用一组小型验证工程回答“这条技术路线走不走得通”，还没有可用的产品。

## 仓库结构

| 路径 | 内容 |
| --- | --- |
| `spikes/` | Phase 0 的验证工程，每道关卡一个，各自带说明和结果 |
| `assets/brand/` | Logo、图标和品牌说明 |
| `packaging/` | 三个平台的打包脚本和许可证检查 |
| `.github/workflows/` | 持续集成 |
| `deny.toml` | 依赖与许可证策略 |

| 关卡 | 工程 | 回答的问题 |
| --- | --- | --- |
| G1 | `spikes/g1-ime` | 自绘的文档区能否使用中文和日文输入法 |
| G2 | `spikes/g2-frame` | CPU 渲染加整帧交给界面框架，滚动和打字是否够快 |
| G3 | `spikes/g3-cjk` | 能否自己实现中日文断行规则，并在三个系统上断出相同的行 |
| G6 | `spikes/g6-storage` | 进程被杀之后，命令日志能否把文档完整带回来 |
| G7 | `packaging/` | 三个平台能否构建、打包并通过许可证检查 |

## 构建与测试

需要 Rust，版本由 `rust-toolchain.toml` 指定，安装了 rustup 就会自动使用。

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
python3 packaging/licenses.py --check
```

Linux 上还需要这些系统库：`libfontconfig1-dev libfreetype6-dev libxkbcommon-dev libwayland-dev libxcb-shape0-dev libxcb-xfixes0-dev`。

## 版本与分支

**版本号**只写在一个地方：根目录 `Cargo.toml` 的 `[workspace.package]`。所有 crate 继承它，打包脚本从这里读取并写进安装包的文件名和元数据。

- 采用语义化版本。1.0 之前，破坏兼容的改动升第二位，其余升第三位。
- 发布对应一个 `v` 开头的标签，例如 `v0.1.0`。标签只打在 `main` 上。

**分支**：

- `main` 始终可以构建并通过全部检查。不直接向它提交。
- 工作在短分支上进行，经持续集成通过后合并。验证工程用 `spike/` 前缀，功能用 `feat/`，修复用 `fix/`。

**提交说明**用英文，第一行写这次改动做了什么，正文写为什么这样做、怎样验证的、还有什么没做。

**依赖**：`Cargo.lock` 入库，所有命令带 `--locked`。Slint、Parley、vello 的版本是有意固定的，升级要单独提交，并重跑受影响的关卡测试。新增依赖的许可证必须已经在 `deny.toml` 的允许列表里；往列表里加新许可证是法务决定。

**不入库的东西**：`target/` 下的全部产物，包括安装包。安装包由持续集成生成并作为构建产物保存。
