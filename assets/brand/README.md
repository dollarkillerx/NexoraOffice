# Aster Office 品牌资源

标志由三个办公文档的形状拼成：左边一张纸页，右上一组表格单元格，右下一块幻灯片。只有幻灯片块使用强调色。

## 文件

| 文件 | 用途 |
| --- | --- |
| `logo/aster-icon.svg` | 应用图标，深色底。默认使用这一个 |
| `logo/aster-icon-light.svg` | 应用图标，浅色底 |
| `logo/aster-icon-violet.svg` | 应用图标，强调色底，标志全白 |
| `logo/aster-mark.svg` | 标志本体，透明底，用在浅色背景上 |
| `logo/aster-mark-on-dark.svg` | 标志本体，透明底，用在深色背景上 |
| `logo/aster-mark-mono.svg` | 单色标志，用在只能印一种颜色的场合 |
| `logo/aster-lockup.svg` | 标志加文字的横排组合 |
| `logo/png/aster-icon-<尺寸>.png` | 深色底图标的位图，16 到 1024 像素，透明圆角 |
| `logo/aster.icns`、`logo/aster.ico` | macOS 和 Windows 的图标文件，由 `logo/make-icons.sh` 从上面的 PNG 生成 |
| `logo-concepts/` | 定稿前的四套概念稿和 C 方案的配色对比，仅作记录 |

## 颜色

| 名称 | 色值 | 说明 |
| --- | --- | --- |
| 强调色，紫罗兰 | `#7C5CE0` | 白底对比度 4.70 : 1，近黑底 4.18 : 1。同时是界面的唯一强调色 |
| 近黑 | `#0b0b0b` | 文字与深色底 |
| 暖白 | `#f7f7f6` | 深色底上的标志 |
| 白 | `#ffffff` | 浅色底 |

## 几何

画布 1024×1024，按 64 为一格分成 16×16 的网格。标志占中间 10×10 格，四周各留 3 格，所有间距都是 1 格。缩到 16 像素时一格正好是一个像素，每条边都落在像素边界上。

- 纸页：4 格宽、10 格高，右上角切去 1 格。
- 单元格：4 个，各 2×2 格。
- 幻灯片：5 格宽、4 格高。

改动图形时保持所有坐标是 64 的整数倍，否则小尺寸下会发虚。

## 导出位图

仓库里的 PNG 是在 macOS 上用 `logo/export-png.swift` 导出的：

```sh
swift assets/brand/logo/export-png.swift assets/brand/logo/aster-icon.svg out.png 256
```

这个脚本只能在 macOS 上运行。改动图标后，先重新导出各尺寸的 PNG，再运行 `logo/make-icons.sh` 重新生成 `icns` 和 `ico`。Linux 直接使用 PNG。

## 尚未完成

- 横排组合里的文字用的是系统字体，还不是定稿。需要选定字体并把文字转成轮廓。
- 没有做商标检索。“纸页加方格”的构图与一些仪表盘、布局类图标相近，正式使用前需要查重。
- macOS 的应用图标有自己的留白规范，打包时要按规范另出一版。
- 概念稿由 OpenAI Codex 生成，C 方案的几何修正和配色由后续人工调整完成。
