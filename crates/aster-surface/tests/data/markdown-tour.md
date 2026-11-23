# Markdown 预览巡览

这份文件把**每一种** Markdown 元素都放了一遍，用来检查*高亮*和预览（*italic* text）。混排 English、日本語のかな、한국어 和 `inline code`。还有~~删掉的字~~和 ***又粗又斜***。

## 链接

- 行内链接：[Aster Office](https://example.com/aster "标题")
- 引用式链接：[参考文档][docs]
- 尖括号：<https://example.org/path?q=1>
- 裸地址：https://example.net/路径 和 www.example.com。
- 邮件：<someone@example.com>
- 不该打开的：[本地文件](file:///etc/hosts) 和 [相对路径](notes.md)

[docs]: https://example.com/docs

### 列表

1. 第一项
2. 第二项
   - 嵌套的无序项
   - 另一项
3. 第三项

- [ ] 还没做的任务
- [x] 已经完成的任务

> 引用的一段话。
>
> > 引用里的引用，**加粗**也行。

#### 代码

```rust
fn main() {
    // 注释里的 *星号* 不是强调
    println!("你好，世界");
}
```

    缩进代码块
    第二行

##### 表格

| 名称 | 数量 | 说明 |
|:-----|-----:|:----:|
| 苹果 | 3 | 红色 |
| 香蕉 | 12 | **黄色** |
| 很长很长的一个单元格内容 | 1 | `代码` |

###### 其他

---

![一张示意图](images/diagram.png)

行内图片 ![小图标](icon.png) 放在文字中间。

数学 $E = mc^2$ 和脚注[^note]。

<div align="center">HTML 块</div>

[^note]: 这是脚注的内容。
