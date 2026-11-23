//! Interface text in every language the shell speaks.
//!
//! All wording the shell shows is a field of [`Strings`]; no other module and no
//! `.slint` file holds interface text. A language is one constant of that type,
//! so a missing translation does not compile, and switching language is just
//! handing the UI a different constant. Names of the editors (Writer, Sheets,
//! ...) are product names and are not translated.
//!
//! Text with a variable part marks it as `{name}`; [`fill`] puts the value in.

use crate::stats::ImeState;
use crate::tabs::DocKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    ZhHans,
    ZhHant,
    Ja,
    En,
    Fr,
    De,
    Es,
    Ko,
}

impl Language {
    /// In the order the language menu lists them.
    pub const ALL: [Language; 8] = [
        Language::ZhHans,
        Language::ZhHant,
        Language::Ja,
        Language::En,
        Language::Fr,
        Language::De,
        Language::Es,
        Language::Ko,
    ];

    /// Stable index, used to hand the language to the UI layer and back.
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|language| *language == self)
            .expect("ALL lists every language")
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    /// BCP 47 tag.
    pub fn tag(self) -> &'static str {
        match self {
            Language::ZhHans => "zh-Hans",
            Language::ZhHant => "zh-Hant",
            Language::Ja => "ja",
            Language::En => "en",
            Language::Fr => "fr",
            Language::De => "de",
            Language::Es => "es",
            Language::Ko => "ko",
        }
    }

    /// The language's own name, shown in the language menu whatever the
    /// current language is.
    pub fn native_name(self) -> &'static str {
        match self {
            Language::ZhHans => "简体中文",
            Language::ZhHant => "繁體中文",
            Language::Ja => "日本語",
            Language::En => "English",
            Language::Fr => "Français",
            Language::De => "Deutsch",
            Language::Es => "Español",
            Language::Ko => "한국어",
        }
    }

    /// Pick a language from a tag such as `ja`, `ja-JP`, `zh_CN.UTF-8`,
    /// `zh-Hant-HK` or `en-US`. Chinese is Traditional when the tag names the
    /// Traditional script or a region that writes it, Simplified otherwise.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let tag = tag.to_ascii_lowercase();
        let mut parts = tag.split(['-', '_', '.']);
        let primary = parts.next().unwrap_or_default();
        match primary {
            "zh" => {
                // An explicit script wins over the region: zh-Hans-HK is Simplified.
                let parts: Vec<&str> = parts.collect();
                let traditional = if parts.contains(&"hans") {
                    false
                } else {
                    parts
                        .iter()
                        .any(|part| matches!(*part, "hant" | "tw" | "hk" | "mo"))
                };
                Some(if traditional {
                    Language::ZhHant
                } else {
                    Language::ZhHans
                })
            }
            "ja" => Some(Language::Ja),
            "en" => Some(Language::En),
            "fr" => Some(Language::Fr),
            "de" => Some(Language::De),
            "es" => Some(Language::Es),
            "ko" => Some(Language::Ko),
            _ => None,
        }
    }

    pub fn strings(self) -> &'static Strings {
        match self {
            Language::ZhHans => &ZH_HANS,
            Language::ZhHant => &ZH_HANT,
            Language::Ja => &JA,
            Language::En => &EN,
            Language::Fr => &FR,
            Language::De => &DE,
            Language::Es => &ES,
            Language::Ko => &KO,
        }
    }
}

/// Replace `{name}` in a template with a value.
pub fn fill(template: &str, name: &str, value: &str) -> String {
    template.replace(&format!("{{{name}}}"), value)
}

macro_rules! strings {
    ($($(#[$doc:meta])* $field:ident,)+) => {
        /// Every piece of interface text, in one language.
        #[derive(Debug, PartialEq, Eq)]
        pub struct Strings {
            $($(#[$doc])* pub $field: &'static str,)+
        }

        impl Strings {
            /// Field names with their text, for checks that must cover all of them.
            pub fn fields(&self) -> Vec<(&'static str, &'static str)> {
                vec![$((stringify!($field), self.$field),)+]
            }
        }
    };
}

strings! {
    /// Under the product name on the start page.
    tagline,
    /// Heading over the editor tiles and in the new-document menu.
    new_document,
    recent,
    files,
    /// Marks an editor that only opens a placeholder.
    placeholder,
    placeholder_view,
    search_files,
    /// What the status bar calls the start page.
    start,
    language,

    writer_label,
    sheets_label,
    slides_label,
    markdown_label,
    pdf_label,

    writer_tagline,
    sheets_tagline,
    slides_tagline,
    markdown_tagline,
    pdf_tagline,

    /// Titles of new documents; a number follows.
    writer_untitled,
    sheets_untitled,
    slides_untitled,
    markdown_untitled,
    pdf_untitled,

    /// `{name}` is the editor.
    placeholder_title,
    placeholder_body,
    /// `{name}` is the editor, `{phase}` the roadmap phase.
    planned_phase,
    pdf_undecided,

    /// Says that a Writer tab is plain text, not the word processor.
    plain_text_note,
    /// `{count}` is a number.
    word_count,
    char_count,
    ime_enabled,
    ime_composing,
    ime_disabled,

    /// Values shown in toolbar fields.
    style_body,
    number_general,

    ai_title,
    ai_body,
    ai_point_tools,
    ai_point_approval,
    ai_point_keys,
    ai_input,

    /// What a new Writer tab starts with.
    writer_sample,

    /// Menu bar. On macOS these are the system menu; elsewhere a bar in the window.
    menu_file,
    menu_edit,
    menu_new_markdown,
    menu_open,
    menu_save,
    menu_save_as,
    menu_close_tab,
    menu_quit,
    menu_undo,
    menu_redo,
    menu_cut,
    menu_copy,
    menu_paste,
    menu_select_all,
    /// The button on the start page.
    open_file,

    /// Asked before closing a tab with unsaved changes. `{name}` is the document.
    unsaved_title,
    unsaved_body,
    /// Asked before quitting with unsaved changes. `{count}` is how many documents.
    quit_title,
    quit_body,
    button_save,
    button_discard,
    button_cancel,
    button_ok,
    button_quit_anyway,

    /// Headings of error messages. `{name}` is the file.
    open_failed,
    save_failed,
    not_utf8,
    is_utf16,

    /// Says that a Markdown tab edits text but does not understand Markdown yet.
    markdown_note,
    /// `{style}` is LF, CRLF or CR.
    mixed_line_endings,
    /// Name of the file-type filter in the open and save dialogs.
    file_type_text,
    /// Titles of the open and save dialogs.
    open_dialog,
    save_dialog,

    /// The question at start-up about documents a crash left unsaved.
    recovery_title,
    recovery_body,
    recovery_restore,
    recovery_discard,
    recovery_restore_all,
    recovery_later,
    /// Shown where a recovered document's file would be.
    recovery_new_document,
    /// When a record was last written. `{count}` is a number.
    recovery_just_now,
    recovery_minutes,
    recovery_hours,
    recovery_days,
    /// What became of a recovered document's file.
    recovery_disk_changed,
    recovery_disk_missing,
    /// `{reason}` is the system's message.
    recovery_disk_unreadable,
    recovery_unreadable,
    /// Status bar notices on a recovered document.
    notice_disk_changed,
    notice_disk_missing,
    /// `{reason}` is the system's message.
    notice_record_failed,
    /// A link in the preview that is not a web or e-mail link was clicked.
    notice_link_refused,
    /// A file is being read. `{name}` is its name.
    notice_opening,
    /// In the preview pane of a document too large to preview. `{size}` is
    /// the limit, such as "4 MB".
    preview_too_large,
    /// Find and replace: the Edit menu, the find bar, and what it reports.
    menu_find,
    menu_find_next,
    menu_find_previous,
    menu_find_selection,
    menu_replace,
    find_placeholder,
    replace_placeholder,
    find_match_case,
    find_whole_word,
    find_regex,
    find_close,
    replace_one,
    replace_all,
    /// `{index}` is the current match, `{count}` how many there are.
    find_position,
    find_none,
    find_counting,
    find_bad_pattern,
    /// More matches than are kept. `{count}` is that number.
    find_too_many,
    /// `{count}` is how many places were replaced.
    notice_replaced,
    notice_replacing,
    /// Files and folders: the File menu, the sidebar and the start page.
    menu_open_folder,
    menu_clear_recent,
    /// Title of the folder dialog.
    open_folder_dialog,
    /// Shows an entry in Finder on macOS, in the file manager elsewhere.
    reveal_finder,
    reveal_other,
    recent_remove,
    recent_empty,
    /// After a recent file that is no longer there.
    recent_missing,
    /// `{name}` is the file, `{path}` the folder it was in.
    recent_missing_title,
    recent_missing_body,
    /// In the sidebar while no folder is open.
    no_folder,
    refresh,
    /// Lines in the folder tree that are not files.
    tree_reading,
    tree_empty,
    /// `{count}` is a number.
    tree_omitted,
    tree_omitted_at_least,
    tree_missing,
    tree_denied,
    tree_not_folder,
    /// `{reason}` is the system's message.
    tree_failed,
    /// After a file the tree shows but cannot open yet.
    unsupported,
    /// `{name}` is the file.
    unsupported_title,
    unsupported_body,
    /// Over the window while files are dragged onto it.
    drop_to_open,
    /// `{name}` is a link to a folder above it.
    loop_title,
    loop_body,
    /// `{name}` is the file or folder.
    reveal_failed,
    /// The sidebar's filter matches nothing.
    no_matches,
    /// When a recent file was last opened. `{time}` is hours and minutes;
    /// `{year}`, `{month}` and `{day}` are numbers.
    opened_today,
    opened_yesterday,
    opened_this_year,
    opened_other_year,
    /// Titles of GitHub's callouts (`> [!NOTE]`) in the preview.
    callout_note,
    callout_tip,
    callout_important,
    callout_warning,
    callout_caution,
    /// Filters of the open dialog.
    file_type_supported,
    file_type_spreadsheet,
    file_type_presentation,
    /// Status bar of Sheets and Slides tabs, which only show the file.
    read_only_note,
    /// `{count}` is a number.
    cell_count,
    /// `{index}` and `{count}` are numbers.
    slide_position,
    calc_running,
    /// `{count}` is how many formulas.
    calc_done,
    /// `{reason}` is the engine's message.
    calc_failed,
    /// Over the grid of a sheet that holds a chart.
    chart_sheet,
    no_slides,
    /// In the status bar on a slide that is skipped in a show.
    slide_hidden,
    /// On the boxes where slide content is not drawn yet.
    shape_picture,
    shape_table,
    shape_chart,
    shape_graphic,
    /// Why an Excel or PowerPoint file did not open.
    office_legacy,
    office_not_office,
    office_wrong_kind,
    office_too_large,
    /// `{reason}` names the damaged part.
    office_broken,
    /// The paragraph style field of a Markdown tab. `{level}` is 1 to 6.
    heading_level,
    /// The Format menu: Markdown syntax the editor writes for the user.
    menu_format,
    format_bold,
    format_italic,
    format_strike,
    format_code,
    format_bullet,
    format_numbered,
    format_task,
    format_quote,
    format_link,
    format_image,
    format_table,
}

impl Strings {
    /// What the status bar calls a kind of document.
    pub fn kind_label(&self, kind: DocKind) -> &'static str {
        match kind {
            DocKind::Writer => self.writer_label,
            DocKind::Sheets => self.sheets_label,
            DocKind::Slides => self.slides_label,
            DocKind::Markdown => self.markdown_label,
            DocKind::Pdf => self.pdf_label,
        }
    }

    /// One line under the editor's name on the start page.
    pub fn kind_tagline(&self, kind: DocKind) -> &'static str {
        match kind {
            DocKind::Writer => self.writer_tagline,
            DocKind::Sheets => self.sheets_tagline,
            DocKind::Slides => self.slides_tagline,
            DocKind::Markdown => self.markdown_tagline,
            DocKind::Pdf => self.pdf_tagline,
        }
    }

    pub fn untitled(&self, kind: DocKind) -> &'static str {
        match kind {
            DocKind::Writer => self.writer_untitled,
            DocKind::Sheets => self.sheets_untitled,
            DocKind::Slides => self.slides_untitled,
            DocKind::Markdown => self.markdown_untitled,
            DocKind::Pdf => self.pdf_untitled,
        }
    }

    /// When the roadmap delivers an editor.
    pub fn plan(&self, kind: DocKind) -> String {
        match kind.planned_phase() {
            Some(phase) => fill(
                &fill(self.planned_phase, "name", kind.name()),
                "phase",
                &phase.to_string(),
            ),
            None => self.pdf_undecided.to_owned(),
        }
    }

    /// Empty while nothing is known about the input method.
    pub fn ime(&self, state: ImeState) -> &'static str {
        match state {
            ImeState::Unknown => "",
            ImeState::Enabled => self.ime_enabled,
            ImeState::Composing => self.ime_composing,
            ImeState::Disabled => self.ime_disabled,
        }
    }
}

pub const ZH_HANS: Strings = Strings {
    tagline: "原生、离线优先的开源办公套件",
    new_document: "新建",
    recent: "最近",
    files: "文件",
    placeholder: "占位",
    placeholder_view: "占位视图",
    search_files: "搜索文件",
    start: "开始",
    language: "语言",

    writer_label: "文字处理",
    sheets_label: "电子表格",
    slides_label: "演示文稿",
    markdown_label: "Markdown",
    pdf_label: "PDF",

    writer_tagline: "文档、报告、信函",
    sheets_tagline: "表格、公式、图表",
    slides_tagline: "幻灯片与演示",
    markdown_tagline: "笔记与纯文本写作",
    pdf_tagline: "阅读与批注",

    writer_untitled: "未命名文档",
    sheets_untitled: "未命名表格",
    slides_untitled: "未命名演示",
    markdown_untitled: "未命名笔记",
    pdf_untitled: "PDF 文档",

    placeholder_title: "{name} 编辑器还没有实现",
    placeholder_body: "这个标签页只用来看外壳的布局：标签、工具栏和状态栏会随文档类型切换，但这里没有任何编辑功能，工具栏按钮全部不可用。背后的线框是布局示意，不是界面。",
    planned_phase: "按路线图，{name} 在 Phase {phase} 交付。",
    pdf_undecided: "PDF 阅读器是否进入第一版还没有决定。",

    plain_text_note: "纯文本原型",
    word_count: "{count} 字",
    char_count: "{count} 字符",
    ime_enabled: "输入法已启用",
    ime_composing: "正在组字",
    ime_disabled: "输入法未启用",

    style_body: "正文",
    number_general: "常规",

    ai_title: "AI 助手",
    ai_body: "内置 AI 和 MCP 服务都还没有实现，这个面板只是为它们留的位置。计划中的做法：",
    ai_point_tools: "内置助手和外部 MCP 客户端使用同一份文档工具目录。",
    ai_point_approval: "会改动文档的操作先经你确认，每次调用都留下记录。",
    ai_point_keys: "服务商和密钥由你自己配置，密钥只保存在本机。",
    ai_input: "向 AI 提问（尚未实现）",

    writer_sample: "\
Aster Office 外壳原型

这一页不是界面框架的文本控件。文字由 Parley 排版、vello_cpu 渲染，再作为一张图贴进窗口，输入法事件直接取自窗口系统。

现在能做的：输入中文、日文和英文，移动光标，选择，滚动，缩放。
还不能做的：格式、撤销、打开和保存。工具栏上置灰的按钮都还没有接上功能。

日本語の入力も試してください。English works too.

",

    menu_file: "文件",
    menu_edit: "编辑",
    menu_new_markdown: "新建 Markdown 文档",
    menu_open: "打开…",
    menu_save: "保存",
    menu_save_as: "另存为…",
    menu_close_tab: "关闭标签页",
    menu_quit: "退出 Aster Office",
    menu_undo: "撤销",
    menu_redo: "重做",
    menu_cut: "剪切",
    menu_copy: "复制",
    menu_paste: "粘贴",
    menu_select_all: "全选",
    open_file: "打开文件…",
    unsaved_title: "要保存对“{name}”的修改吗？",
    unsaved_body: "不保存的话，这些修改会丢失。",
    quit_title: "有 {count} 个文档尚未保存",
    quit_body: "现在退出，这些文档里未保存的修改会丢失。",
    button_save: "保存",
    button_discard: "不保存",
    button_cancel: "取消",
    button_ok: "好",
    button_quit_anyway: "放弃修改并退出",
    open_failed: "无法打开“{name}”",
    save_failed: "无法保存“{name}”",
    not_utf8: "这个文件不是 UTF-8 编码。Aster Office 目前只读写 UTF-8 文本；文件没有被改动。",
    is_utf16: "这个文件是 UTF-16 编码。Aster Office 目前只读写 UTF-8 文本；文件没有被改动。",
    markdown_note: "语法高亮和预览；图片只显示占位框",
    mixed_line_endings: "换行符混用，保存时统一为 {style}",
    file_type_text: "Markdown 与纯文本",
    open_dialog: "打开",
    save_dialog: "另存为",
    recovery_title: "恢复未保存的文档",
    recovery_body: "Aster Office 上次没有正常退出。下面这些文档有尚未保存的修改，选择要恢复的：",
    recovery_restore: "恢复",
    recovery_discard: "丢弃",
    recovery_restore_all: "全部恢复",
    recovery_later: "稍后决定",
    recovery_new_document: "尚未保存过的新文档",
    recovery_just_now: "刚刚",
    recovery_minutes: "{count} 分钟前",
    recovery_hours: "{count} 小时前",
    recovery_days: "{count} 天前",
    recovery_disk_changed: "原文件在此期间被其他程序修改过",
    recovery_disk_missing: "原文件已不存在",
    recovery_disk_unreadable: "无法读取原文件：{reason}",
    recovery_unreadable: "这条记录无法读取：{reason}",
    notice_disk_changed: "原文件在恢复前已被其他程序修改；保存时会先问你存到哪里",
    notice_disk_missing: "原文件已不存在；保存时会先问你存到哪里",
    notice_record_failed: "恢复记录无法写入（{reason}），崩溃后这个文档可能无法找回",
    notice_link_refused: "只会打开网页和邮件链接，这个链接没有打开",
    notice_opening: "正在打开 {name}…",
    preview_too_large: "文档超过 {size}，预览已关闭",
    menu_find: "查找…",
    menu_find_next: "查找下一个",
    menu_find_previous: "查找上一个",
    menu_find_selection: "用所选内容查找",
    menu_replace: "替换…",
    find_placeholder: "查找",
    replace_placeholder: "替换为",
    find_match_case: "区分大小写",
    find_whole_word: "全字匹配",
    find_regex: "正则表达式",
    find_close: "关闭查找",
    replace_one: "替换",
    replace_all: "全部替换",
    find_position: "{index} / {count}",
    find_none: "无结果",
    find_counting: "正在统计…",
    find_bad_pattern: "正则表达式有误",
    find_too_many: "{count} 个以上",
    notice_replaced: "已替换 {count} 处",
    notice_replacing: "正在全部替换…",
    menu_open_folder: "打开文件夹…",
    menu_clear_recent: "清除最近列表",
    open_folder_dialog: "打开文件夹",
    reveal_finder: "在访达中显示",
    reveal_other: "在文件管理器中显示",
    recent_remove: "从最近列表中移除",
    recent_empty: "还没有打开过文件。打开或保存的文件会列在这里。",
    recent_missing: "不可用",
    recent_missing_title: "找不到“{name}”",
    recent_missing_body: "它原来在 {path}，可能已被移动、改名或删除。",
    no_folder: "打开一个文件夹，它里面的文档会列在这里。",
    refresh: "刷新",
    tree_reading: "正在读取…",
    tree_empty: "没有可显示的文件",
    tree_omitted: "还有 {count} 项没有列出",
    tree_omitted_at_least: "还有至少 {count} 项没有列出",
    tree_missing: "文件夹不在了",
    tree_denied: "没有读取权限",
    tree_not_folder: "这不是文件夹",
    tree_failed: "无法读取：{reason}",
    unsupported: "暂不支持",
    unsupported_title: "暂不支持打开“{name}”",
    unsupported_body: "这一版能打开 Markdown 和纯文本文件（.md、.markdown、.txt），以及 Excel（.xlsx）和 PowerPoint（.pptx）文件，后两种只能查看。Word 和 PDF 文件会在以后的版本中支持。",
    drop_to_open: "松开即可打开",
    loop_title: "“{name}”不能展开",
    loop_body: "它是一个链接，指向它所在的文件夹或更上层的文件夹，展开它会无穷无尽地重复下去。",
    reveal_failed: "无法在文件管理器中显示“{name}”",
    no_matches: "没有匹配的文件",
    opened_today: "今天 {time}",
    opened_yesterday: "昨天 {time}",
    opened_this_year: "{month} 月 {day} 日",
    opened_other_year: "{year} 年 {month} 月 {day} 日",
    callout_note: "说明",
    callout_tip: "提示",
    callout_important: "重要",
    callout_warning: "警告",
    callout_caution: "当心",
    file_type_supported: "所有支持的文件",
    file_type_spreadsheet: "Excel 工作簿",
    file_type_presentation: "PowerPoint 演示文稿",
    read_only_note: "只读",
    cell_count: "{count} 个单元格",
    slide_position: "第 {index} 张，共 {count} 张",
    calc_running: "正在计算公式…",
    calc_done: "已计算 {count} 个公式",
    calc_failed: "公式没有算完，显示的是文件里保存的结果：{reason}",
    chart_sheet: "这是图表工作表。图表还不能显示。",
    no_slides: "这个演示文稿里没有幻灯片。",
    slide_hidden: "隐藏",
    shape_picture: "图片",
    shape_table: "表格",
    shape_chart: "图表",
    shape_graphic: "图形",
    office_legacy: "这是 Office 97–2003 格式的文件，或者设了打开密码。目前只能打开没有加密的 .xlsx 和 .pptx 文件。",
    office_not_office: "这不是 Excel 或 PowerPoint 文件，或者文件已经损坏。",
    office_wrong_kind: "文件的内容和扩展名不符：比如一个演示文稿被命名成了 .xlsx。",
    office_too_large: "这个文件超出了安全限制（部件太多、太大，或压缩得异常），没有打开。",
    office_broken: "文件已损坏，读不出来：{reason}",
    heading_level: "标题 {level}",
    menu_format: "格式",
    format_bold: "粗体",
    format_italic: "斜体",
    format_strike: "删除线",
    format_code: "行内代码",
    format_bullet: "无序列表",
    format_numbered: "有序列表",
    format_task: "任务列表",
    format_quote: "引用",
    format_link: "链接",
    format_image: "图片",
    format_table: "表格",
};

pub const JA: Strings = Strings {
    tagline: "ネイティブで、オフライン優先の、オープンソースのオフィススイート",
    new_document: "新規作成",
    recent: "最近使ったファイル",
    files: "ファイル",
    placeholder: "未実装",
    placeholder_view: "プレースホルダー",
    search_files: "ファイルを検索",
    start: "スタート",
    language: "言語",

    writer_label: "文書",
    sheets_label: "スプレッドシート",
    slides_label: "プレゼンテーション",
    markdown_label: "Markdown",
    pdf_label: "PDF",

    writer_tagline: "文書、レポート、手紙",
    sheets_tagline: "表、数式、グラフ",
    slides_tagline: "スライドと発表",
    markdown_tagline: "メモとプレーンテキスト",
    pdf_tagline: "閲覧と注釈",

    writer_untitled: "無題の文書",
    sheets_untitled: "無題のシート",
    slides_untitled: "無題のスライド",
    markdown_untitled: "無題のメモ",
    pdf_untitled: "PDF 文書",

    placeholder_title: "{name} エディターはまだ実装されていません",
    placeholder_body: "このタブはシェルのレイアウトを確認するためのものです。タブ、ツールバー、ステータスバーは文書の種類に応じて切り替わりますが、編集機能はなく、ツールバーのボタンはすべて無効です。背後の線画はレイアウトの見取り図で、実際の画面ではありません。",
    planned_phase: "ロードマップでは、{name} は Phase {phase} で提供する予定です。",
    pdf_undecided: "PDF ビューアーを最初の版に含めるかどうかは未定です。",

    plain_text_note: "プレーンテキストの試作",
    word_count: "{count} 語",
    char_count: "{count} 文字",
    ime_enabled: "入力メソッド有効",
    ime_composing: "変換中",
    ime_disabled: "入力メソッド無効",

    style_body: "本文",
    number_general: "標準",

    ai_title: "AI アシスタント",
    ai_body: "内蔵 AI と MCP サーバーはまだ実装されていません。このパネルはそのための場所を確保しているだけです。予定している方針：",
    ai_point_tools: "内蔵アシスタントと外部の MCP クライアントは、同じ文書ツール一覧を使います。",
    ai_point_approval: "文書を変更する操作は実行前に確認を求め、呼び出しはすべて記録します。",
    ai_point_keys: "プロバイダーと API キーは自分で設定し、キーはこの端末にだけ保存します。",
    ai_input: "AI に質問（未実装）",

    writer_sample: "\
Aster Office シェルの試作

このページは UI フレームワークのテキスト部品ではありません。文字は Parley で組版し、vello_cpu で描画して、一枚の画像としてウィンドウに貼っています。入力メソッドのイベントはウィンドウシステムから直接受け取ります。

できること：日本語、中国語、英語の入力、カーソル移動、選択、スクロール、拡大縮小。
まだできないこと：書式、元に戻す、開く、保存。ツールバーの灰色のボタンには機能がつながっていません。

中文输入也请试一下。English works too.

",

    menu_file: "ファイル",
    menu_edit: "編集",
    menu_new_markdown: "新規 Markdown 文書",
    menu_open: "開く…",
    menu_save: "保存",
    menu_save_as: "名前を付けて保存…",
    menu_close_tab: "タブを閉じる",
    menu_quit: "Aster Office を終了",
    menu_undo: "元に戻す",
    menu_redo: "やり直す",
    menu_cut: "切り取り",
    menu_copy: "コピー",
    menu_paste: "貼り付け",
    menu_select_all: "すべて選択",
    open_file: "ファイルを開く…",
    unsaved_title: "「{name}」への変更を保存しますか？",
    unsaved_body: "保存しない場合、変更内容は失われます。",
    quit_title: "未保存の文書が {count} 件あります",
    quit_body: "このまま終了すると、これらの文書の未保存の変更は失われます。",
    button_save: "保存",
    button_discard: "保存しない",
    button_cancel: "キャンセル",
    button_ok: "OK",
    button_quit_anyway: "変更を破棄して終了",
    open_failed: "「{name}」を開けません",
    save_failed: "「{name}」を保存できません",
    not_utf8: "このファイルは UTF-8 ではありません。Aster Office は現在 UTF-8 のテキストだけを読み書きします。ファイルは変更されていません。",
    is_utf16: "このファイルは UTF-16 です。Aster Office は現在 UTF-8 のテキストだけを読み書きします。ファイルは変更されていません。",
    markdown_note: "構文ハイライトとプレビュー。画像は枠だけ表示します",
    mixed_line_endings: "改行コードが混在しています。保存時に {style} に統一されます",
    file_type_text: "Markdown とプレーンテキスト",
    open_dialog: "開く",
    save_dialog: "名前を付けて保存",
    recovery_title: "保存されていない文書の復元",
    recovery_body: "前回 Aster Office は正常に終了しませんでした。次の文書には保存されていない変更があります。復元するものを選んでください：",
    recovery_restore: "復元",
    recovery_discard: "破棄",
    recovery_restore_all: "すべて復元",
    recovery_later: "あとで決める",
    recovery_new_document: "まだ保存されていない新規文書",
    recovery_just_now: "たった今",
    recovery_minutes: "{count} 分前",
    recovery_hours: "{count} 時間前",
    recovery_days: "{count} 日前",
    recovery_disk_changed: "元のファイルはその間に別のプログラムで変更されました",
    recovery_disk_missing: "元のファイルはもうありません",
    recovery_disk_unreadable: "元のファイルを読めません：{reason}",
    recovery_unreadable: "この記録は読めません：{reason}",
    notice_disk_changed: "元のファイルは復元前に別のプログラムで変更されています。保存するときは保存先を確認します",
    notice_disk_missing: "元のファイルはもうありません。保存するときは保存先を確認します",
    notice_record_failed: "復元用の記録を書き込めません（{reason}）。異常終了するとこの文書は取り戻せないかもしれません",
    notice_link_refused: "開けるのはウェブとメールのリンクだけです。このリンクは開きませんでした",
    notice_opening: "{name} を開いています…",
    preview_too_large: "{size} を超える文書のため、プレビューはオフです",
    menu_find: "検索…",
    menu_find_next: "次を検索",
    menu_find_previous: "前を検索",
    menu_find_selection: "選択範囲で検索",
    menu_replace: "置換…",
    find_placeholder: "検索",
    replace_placeholder: "置換後の文字列",
    find_match_case: "大文字と小文字を区別",
    find_whole_word: "単語単位",
    find_regex: "正規表現",
    find_close: "検索を閉じる",
    replace_one: "置換",
    replace_all: "すべて置換",
    find_position: "{index} / {count}",
    find_none: "見つかりません",
    find_counting: "数えています…",
    find_bad_pattern: "正規表現に誤りがあります",
    find_too_many: "{count} 件以上",
    notice_replaced: "{count} 件置換しました",
    notice_replacing: "すべて置換しています…",
    menu_open_folder: "フォルダを開く…",
    menu_clear_recent: "最近使った項目を消去",
    open_folder_dialog: "フォルダを開く",
    reveal_finder: "Finder に表示",
    reveal_other: "ファイルマネージャで表示",
    recent_remove: "最近使った項目から削除",
    recent_empty: "まだファイルを開いていません。開いたファイルや保存したファイルがここに並びます。",
    recent_missing: "見つかりません",
    recent_missing_title: "「{name}」が見つかりません",
    recent_missing_body: "以前は {path} にありました。移動、名前の変更、または削除された可能性があります。",
    no_folder: "フォルダを開くと、その中の書類がここに並びます。",
    refresh: "再読み込み",
    tree_reading: "読み込み中…",
    tree_empty: "表示できるファイルはありません",
    tree_omitted: "ほかに {count} 項目あります（表示していません）",
    tree_omitted_at_least: "ほかに {count} 項目以上あります（表示していません）",
    tree_missing: "フォルダが見つかりません",
    tree_denied: "読み取る権限がありません",
    tree_not_folder: "フォルダではありません",
    tree_failed: "読み込めません：{reason}",
    unsupported: "未対応",
    unsupported_title: "「{name}」はまだ開けません",
    unsupported_body: "このバージョンで開けるのは Markdown とテキストファイル（.md、.markdown、.txt）、Excel（.xlsx）と PowerPoint（.pptx）のファイルで、Excel と PowerPoint は表示のみです。Word と PDF のファイルは今後のバージョンで対応します。",
    drop_to_open: "ドロップして開く",
    loop_title: "「{name}」は展開できません",
    loop_body: "これは自分のあるフォルダか、その上位のフォルダを指すリンクです。展開すると同じフォルダが際限なく繰り返されます。",
    reveal_failed: "「{name}」をファイルマネージャで表示できません",
    no_matches: "一致するファイルはありません",
    opened_today: "今日 {time}",
    opened_yesterday: "昨日 {time}",
    opened_this_year: "{month}月{day}日",
    opened_other_year: "{year}年{month}月{day}日",
    callout_note: "注記",
    callout_tip: "ヒント",
    callout_important: "重要",
    callout_warning: "警告",
    callout_caution: "注意",
    file_type_supported: "対応しているすべてのファイル",
    file_type_spreadsheet: "Excel ブック",
    file_type_presentation: "PowerPoint プレゼンテーション",
    read_only_note: "読み取り専用",
    cell_count: "{count} 個のセル",
    slide_position: "{index} / {count} 枚目",
    calc_running: "数式を計算しています…",
    calc_done: "{count} 個の数式を計算しました",
    calc_failed: "数式を計算しきれませんでした。ファイルに保存された結果を表示しています：{reason}",
    chart_sheet: "グラフシートです。グラフはまだ表示できません。",
    no_slides: "このプレゼンテーションにはスライドがありません。",
    slide_hidden: "非表示",
    shape_picture: "画像",
    shape_table: "表",
    shape_chart: "グラフ",
    shape_graphic: "図形",
    office_legacy: "Office 97–2003 形式のファイルか、パスワードで保護されたファイルです。現在開けるのは暗号化されていない .xlsx と .pptx だけです。",
    office_not_office: "Excel や PowerPoint のファイルではないか、ファイルが壊れています。",
    office_wrong_kind: "ファイルの内容が拡張子と合いません（たとえば .xlsx という名前のプレゼンテーション）。",
    office_too_large: "このファイルは安全上の制限（部品の数、大きさ、異常な圧縮率）を超えているため、開きませんでした。",
    office_broken: "ファイルが壊れていて読み取れません：{reason}",
    heading_level: "見出し {level}",
    menu_format: "書式",
    format_bold: "太字",
    format_italic: "斜体",
    format_strike: "取り消し線",
    format_code: "インラインコード",
    format_bullet: "箇条書き",
    format_numbered: "番号付きリスト",
    format_task: "タスクリスト",
    format_quote: "引用",
    format_link: "リンク",
    format_image: "画像",
    format_table: "表",
};

pub const EN: Strings = Strings {
    tagline: "A native, offline-first, open-source office suite",
    new_document: "New",
    recent: "Recent",
    files: "Files",
    placeholder: "Not built",
    placeholder_view: "Placeholder",
    search_files: "Search files",
    start: "Start",
    language: "Language",

    writer_label: "Document",
    sheets_label: "Spreadsheet",
    slides_label: "Presentation",
    markdown_label: "Markdown",
    pdf_label: "PDF",

    writer_tagline: "Documents, reports, letters",
    sheets_tagline: "Tables, formulas, charts",
    slides_tagline: "Slides and presenting",
    markdown_tagline: "Notes and plain-text writing",
    pdf_tagline: "Reading and annotating",

    writer_untitled: "Untitled document",
    sheets_untitled: "Untitled spreadsheet",
    slides_untitled: "Untitled presentation",
    markdown_untitled: "Untitled note",
    pdf_untitled: "PDF document",

    placeholder_title: "The {name} editor is not built yet",
    placeholder_body: "This tab only shows how the shell is laid out. Tabs, toolbar and status bar change with the kind of document, but nothing here can be edited and every toolbar button is disabled. The wireframe behind this card is a sketch of the layout, not a working view.",
    planned_phase: "On the roadmap, {name} arrives in Phase {phase}.",
    pdf_undecided: "Whether a PDF reader ships in the first version has not been decided.",

    plain_text_note: "Plain-text prototype",
    word_count: "{count} words",
    char_count: "{count} characters",
    ime_enabled: "Input method on",
    ime_composing: "Composing",
    ime_disabled: "Input method off",

    style_body: "Body",
    number_general: "General",

    ai_title: "AI assistant",
    ai_body: "Neither the built-in AI nor the MCP server exists yet. This panel only holds their place. What is planned:",
    ai_point_tools: "The built-in assistant and outside MCP clients use one and the same catalogue of document tools.",
    ai_point_approval: "Anything that changes a document asks you first, and every call is recorded.",
    ai_point_keys: "You choose the provider and supply the key; keys stay on this machine.",
    ai_input: "Ask the AI (not built yet)",

    writer_sample: "\
Aster Office shell prototype

This page is not a text widget of the UI toolkit. The text is laid out by Parley, drawn by vello_cpu and placed in the window as one image; input-method events come straight from the window system.

What works: typing in English, Chinese and Japanese, moving the caret, selecting, scrolling, zooming.
What does not: formatting, undo, opening and saving. The greyed-out toolbar buttons are not connected to anything yet.

中文输入也请试一下。日本語の入力も試してください。

",

    menu_file: "File",
    menu_edit: "Edit",
    menu_new_markdown: "New Markdown Document",
    menu_open: "Open…",
    menu_save: "Save",
    menu_save_as: "Save As…",
    menu_close_tab: "Close Tab",
    menu_quit: "Quit Aster Office",
    menu_undo: "Undo",
    menu_redo: "Redo",
    menu_cut: "Cut",
    menu_copy: "Copy",
    menu_paste: "Paste",
    menu_select_all: "Select All",
    open_file: "Open File…",
    unsaved_title: "Save the changes to “{name}”?",
    unsaved_body: "If you don’t save, your changes will be lost.",
    quit_title: "{count} documents have unsaved changes",
    quit_body: "If you quit now, the unsaved changes in these documents will be lost.",
    button_save: "Save",
    button_discard: "Don’t Save",
    button_cancel: "Cancel",
    button_ok: "OK",
    button_quit_anyway: "Discard Changes and Quit",
    open_failed: "“{name}” could not be opened",
    save_failed: "“{name}” could not be saved",
    not_utf8: "This file is not UTF-8. For now Aster Office reads and writes UTF-8 text only. The file has not been changed.",
    is_utf16: "This file is UTF-16. For now Aster Office reads and writes UTF-8 text only. The file has not been changed.",
    markdown_note: "Syntax highlighting and preview; images show as placeholders",
    mixed_line_endings: "Mixed line endings: saving will unify them to {style}",
    file_type_text: "Markdown and plain text",
    open_dialog: "Open",
    save_dialog: "Save As",
    recovery_title: "Recover Unsaved Documents",
    recovery_body: "Aster Office did not quit normally last time. These documents have changes that were not saved. Choose what to recover:",
    recovery_restore: "Recover",
    recovery_discard: "Discard",
    recovery_restore_all: "Recover All",
    recovery_later: "Decide Later",
    recovery_new_document: "A new document that was never saved",
    recovery_just_now: "just now",
    recovery_minutes: "{count} min ago",
    recovery_hours: "{count} h ago",
    recovery_days: "{count} days ago",
    recovery_disk_changed: "The original file was changed by another program in the meantime",
    recovery_disk_missing: "The original file no longer exists",
    recovery_disk_unreadable: "The original file cannot be read: {reason}",
    recovery_unreadable: "This record cannot be read: {reason}",
    notice_disk_changed: "The original file was changed by another program before recovery; saving will ask where to save",
    notice_disk_missing: "The original file no longer exists; saving will ask where to save",
    notice_record_failed: "The recovery record cannot be written ({reason}); this document may be lost if the app crashes",
    notice_link_refused: "Only web and e-mail links are opened; this link was not",
    notice_opening: "Opening {name}…",
    preview_too_large: "The preview is off for documents over {size}",
    menu_find: "Find…",
    menu_find_next: "Find Next",
    menu_find_previous: "Find Previous",
    menu_find_selection: "Use Selection for Find",
    menu_replace: "Replace…",
    find_placeholder: "Find",
    replace_placeholder: "Replace with",
    find_match_case: "Match case",
    find_whole_word: "Whole word",
    find_regex: "Regular expression",
    find_close: "Close find",
    replace_one: "Replace",
    replace_all: "Replace All",
    find_position: "{index} of {count}",
    find_none: "No results",
    find_counting: "Counting…",
    find_bad_pattern: "Invalid regular expression",
    find_too_many: "More than {count}",
    notice_replaced: "Replaced {count}",
    notice_replacing: "Replacing all…",
    menu_open_folder: "Open Folder…",
    menu_clear_recent: "Clear Recent Files",
    open_folder_dialog: "Open Folder",
    reveal_finder: "Show in Finder",
    reveal_other: "Show in File Manager",
    recent_remove: "Remove from Recent Files",
    recent_empty: "No files opened yet. Files you open or save appear here.",
    recent_missing: "Unavailable",
    recent_missing_title: "“{name}” can’t be found",
    recent_missing_body: "It was in {path}. It may have been moved, renamed or deleted.",
    no_folder: "Open a folder to list the documents in it here.",
    refresh: "Refresh",
    tree_reading: "Reading…",
    tree_empty: "No files to show",
    tree_omitted: "{count} more not listed",
    tree_omitted_at_least: "At least {count} more not listed",
    tree_missing: "The folder is gone",
    tree_denied: "No permission to read",
    tree_not_folder: "Not a folder",
    tree_failed: "Can’t read: {reason}",
    unsupported: "Not yet supported",
    unsupported_title: "“{name}” can’t be opened yet",
    unsupported_body: "This version opens Markdown and plain text files (.md, .markdown, .txt), and Excel (.xlsx) and PowerPoint (.pptx) files for viewing. Word and PDF files will be supported in a later version.",
    drop_to_open: "Drop to open",
    loop_title: "“{name}” doesn’t expand",
    loop_body: "It is a link to the folder it is in, or to a folder above it. Expanding it would repeat the same folders without end.",
    reveal_failed: "Can’t show “{name}” in the file manager",
    no_matches: "No matching files",
    opened_today: "Today {time}",
    opened_yesterday: "Yesterday {time}",
    opened_this_year: "{month}/{day}",
    opened_other_year: "{month}/{day}/{year}",
    callout_note: "Note",
    callout_tip: "Tip",
    callout_important: "Important",
    callout_warning: "Warning",
    callout_caution: "Caution",
    file_type_supported: "All supported files",
    file_type_spreadsheet: "Excel workbooks",
    file_type_presentation: "PowerPoint presentations",
    read_only_note: "Read-only",
    cell_count: "{count} cells",
    slide_position: "Slide {index} of {count}",
    calc_running: "Calculating formulas…",
    calc_done: "{count} formulas calculated",
    calc_failed: "Formulas were not calculated; showing the results saved in the file: {reason}",
    chart_sheet: "This is a chart sheet. Charts can’t be shown yet.",
    no_slides: "This presentation has no slides.",
    slide_hidden: "Hidden",
    shape_picture: "Picture",
    shape_table: "Table",
    shape_chart: "Chart",
    shape_graphic: "Graphic",
    office_legacy: "This is an Office 97–2003 file, or one protected with a password. Only unencrypted .xlsx and .pptx files can be opened for now.",
    office_not_office: "This is not an Excel or PowerPoint file, or it is damaged.",
    office_wrong_kind: "The file’s contents don’t match its extension, such as a presentation named .xlsx.",
    office_too_large: "This file is over a safety limit (too many parts, parts too large, or unusually compressed) and was not opened.",
    office_broken: "The file is damaged and can’t be read: {reason}",
    heading_level: "Heading {level}",
    menu_format: "Format",
    format_bold: "Bold",
    format_italic: "Italic",
    format_strike: "Strikethrough",
    format_code: "Inline Code",
    format_bullet: "Bulleted List",
    format_numbered: "Numbered List",
    format_task: "Task List",
    format_quote: "Quote",
    format_link: "Link",
    format_image: "Image",
    format_table: "Table",
};

pub const ZH_HANT: Strings = Strings {
    tagline: "原生、離線優先的開源辦公套件",
    new_document: "新增",
    recent: "最近",
    files: "檔案",
    placeholder: "未實作",
    placeholder_view: "預留畫面",
    search_files: "搜尋檔案",
    start: "開始",
    language: "語言",
    writer_label: "文書處理",
    sheets_label: "試算表",
    slides_label: "簡報",
    markdown_label: "Markdown",
    pdf_label: "PDF",
    writer_tagline: "文件、報告、信函",
    sheets_tagline: "表格、公式、圖表",
    slides_tagline: "投影片與簡報",
    markdown_tagline: "筆記與純文字寫作",
    pdf_tagline: "閱讀與註解",
    writer_untitled: "未命名文件",
    sheets_untitled: "未命名試算表",
    slides_untitled: "未命名簡報",
    markdown_untitled: "未命名筆記",
    pdf_untitled: "PDF 文件",
    placeholder_title: "{name} 編輯器尚未實作",
    placeholder_body: "這個分頁只用來檢視外殼的版面：分頁、工具列和狀態列會隨文件類型切換，但這裡沒有任何編輯功能，工具列按鈕全部無法使用。背後的線框是版面示意，不是實際介面。",
    planned_phase: "依照路線圖，{name} 會在 Phase {phase} 交付。",
    pdf_undecided: "PDF 閱讀器是否納入第一版尚未決定。",
    plain_text_note: "純文字原型",
    word_count: "{count} 字",
    char_count: "{count} 字元",
    ime_enabled: "輸入法已啟用",
    ime_composing: "正在組字",
    ime_disabled: "輸入法未啟用",
    style_body: "內文",
    number_general: "通用格式",
    ai_title: "AI 助理",
    ai_body: "內建 AI 和 MCP 服務都尚未實作，這個面板只是為它們預留的位置。規劃中的做法：",
    ai_point_tools: "內建助理和外部 MCP 用戶端使用同一份文件工具目錄。",
    ai_point_approval: "會更動文件的操作會先請你確認，每次呼叫都會留下紀錄。",
    ai_point_keys: "服務供應商和金鑰由你自行設定，金鑰只儲存在本機。",
    ai_input: "向 AI 提問（尚未實作）",
    writer_sample: "Aster Office 外殼原型\n\n這一頁不是介面框架的文字元件。文字由 Parley 排版、vello_cpu 繪製，再當成一張圖貼進視窗，輸入法事件直接取自視窗系統。\n\n現在能做的：輸入中文、日文和英文，移動游標，選取，捲動，縮放。\n還不能做的：格式、復原、開啟和儲存。工具列上變灰的按鈕都還沒有接上功能。\n\n日本語の入力も試してください。English works too.\n\n",

    menu_file: "檔案",
    menu_edit: "編輯",
    menu_new_markdown: "新增 Markdown 文件",
    menu_open: "開啟…",
    menu_save: "儲存",
    menu_save_as: "另存新檔…",
    menu_close_tab: "關閉分頁",
    menu_quit: "結束 Aster Office",
    menu_undo: "復原",
    menu_redo: "重做",
    menu_cut: "剪下",
    menu_copy: "複製",
    menu_paste: "貼上",
    menu_select_all: "全選",
    open_file: "開啟檔案…",
    unsaved_title: "要儲存對「{name}」的變更嗎？",
    unsaved_body: "如果不儲存，這些變更將會遺失。",
    quit_title: "有 {count} 份文件尚未儲存",
    quit_body: "現在結束，這些文件中未儲存的變更將會遺失。",
    button_save: "儲存",
    button_discard: "不儲存",
    button_cancel: "取消",
    button_ok: "好",
    button_quit_anyway: "捨棄變更並結束",
    open_failed: "無法開啟「{name}」",
    save_failed: "無法儲存「{name}」",
    not_utf8: "這個檔案不是 UTF-8 編碼。Aster Office 目前只讀寫 UTF-8 文字；檔案沒有被更動。",
    is_utf16: "這個檔案是 UTF-16 編碼。Aster Office 目前只讀寫 UTF-8 文字；檔案沒有被更動。",
    markdown_note: "語法標示和預覽；圖片只顯示預留框",
    mixed_line_endings: "換行字元混用，儲存時統一為 {style}",
    file_type_text: "Markdown 與純文字",
    open_dialog: "開啟",
    save_dialog: "另存新檔",
    recovery_title: "復原未儲存的文件",
    recovery_body: "Aster Office 上次沒有正常結束。下列文件有尚未儲存的變更，請選擇要復原的：",
    recovery_restore: "復原",
    recovery_discard: "捨棄",
    recovery_restore_all: "全部復原",
    recovery_later: "稍後決定",
    recovery_new_document: "尚未儲存過的新文件",
    recovery_just_now: "剛剛",
    recovery_minutes: "{count} 分鐘前",
    recovery_hours: "{count} 小時前",
    recovery_days: "{count} 天前",
    recovery_disk_changed: "原檔案在這段期間被其他程式修改過",
    recovery_disk_missing: "原檔案已不存在",
    recovery_disk_unreadable: "無法讀取原檔案：{reason}",
    recovery_unreadable: "這筆記錄無法讀取：{reason}",
    notice_disk_changed: "原檔案在復原前已被其他程式修改；儲存時會先詢問你要存到哪裡",
    notice_disk_missing: "原檔案已不存在；儲存時會先詢問你要存到哪裡",
    notice_record_failed: "無法寫入復原記錄（{reason}），當機後這份文件可能無法找回",
    notice_link_refused: "只會開啟網頁和郵件連結，這個連結沒有開啟",
    notice_opening: "正在開啟 {name}…",
    preview_too_large: "文件超過 {size}，預覽已關閉",
    menu_find: "尋找…",
    menu_find_next: "尋找下一個",
    menu_find_previous: "尋找上一個",
    menu_find_selection: "用所選內容尋找",
    menu_replace: "取代…",
    find_placeholder: "尋找",
    replace_placeholder: "取代為",
    find_match_case: "區分大小寫",
    find_whole_word: "全字相符",
    find_regex: "規則運算式",
    find_close: "關閉尋找",
    replace_one: "取代",
    replace_all: "全部取代",
    find_position: "{index} / {count}",
    find_none: "沒有結果",
    find_counting: "正在統計…",
    find_bad_pattern: "規則運算式有誤",
    find_too_many: "{count} 個以上",
    notice_replaced: "已取代 {count} 處",
    notice_replacing: "正在全部取代…",
    menu_open_folder: "開啟資料夾…",
    menu_clear_recent: "清除最近清單",
    open_folder_dialog: "開啟資料夾",
    reveal_finder: "在 Finder 中顯示",
    reveal_other: "在檔案管理員中顯示",
    recent_remove: "從最近清單中移除",
    recent_empty: "還沒有開啟過檔案。開啟或儲存的檔案會列在這裡。",
    recent_missing: "無法使用",
    recent_missing_title: "找不到「{name}」",
    recent_missing_body: "它原本在 {path}，可能已被移動、重新命名或刪除。",
    no_folder: "開啟一個資料夾，其中的文件會列在這裡。",
    refresh: "重新整理",
    tree_reading: "正在讀取…",
    tree_empty: "沒有可顯示的檔案",
    tree_omitted: "還有 {count} 項沒有列出",
    tree_omitted_at_least: "還有至少 {count} 項沒有列出",
    tree_missing: "資料夾已不存在",
    tree_denied: "沒有讀取權限",
    tree_not_folder: "這不是資料夾",
    tree_failed: "無法讀取：{reason}",
    unsupported: "尚不支援",
    unsupported_title: "尚不支援開啟「{name}」",
    unsupported_body: "這一版能開啟 Markdown 和純文字檔（.md、.markdown、.txt），以及 Excel（.xlsx）和 PowerPoint（.pptx）檔案，後兩種只能檢視。Word 和 PDF 檔案會在以後的版本中支援。",
    drop_to_open: "放開即可開啟",
    loop_title: "「{name}」無法展開",
    loop_body: "它是一個連結，指向它所在的資料夾或更上層的資料夾，展開它會無止盡地重複下去。",
    reveal_failed: "無法在檔案管理員中顯示「{name}」",
    no_matches: "沒有相符的檔案",
    opened_today: "今天 {time}",
    opened_yesterday: "昨天 {time}",
    opened_this_year: "{month} 月 {day} 日",
    opened_other_year: "{year} 年 {month} 月 {day} 日",
    callout_note: "說明",
    callout_tip: "提示",
    callout_important: "重要",
    callout_warning: "警告",
    callout_caution: "當心",
    file_type_supported: "所有支援的檔案",
    file_type_spreadsheet: "Excel 活頁簿",
    file_type_presentation: "PowerPoint 簡報",
    read_only_note: "唯讀",
    cell_count: "{count} 個儲存格",
    slide_position: "第 {index} 張，共 {count} 張",
    calc_running: "正在計算公式…",
    calc_done: "已計算 {count} 個公式",
    calc_failed: "公式沒有算完，顯示的是檔案裡儲存的結果：{reason}",
    chart_sheet: "這是圖表工作表。圖表還不能顯示。",
    no_slides: "這份簡報裡沒有投影片。",
    slide_hidden: "隱藏",
    shape_picture: "圖片",
    shape_table: "表格",
    shape_chart: "圖表",
    shape_graphic: "圖形",
    office_legacy: "這是 Office 97–2003 格式的檔案，或者設了開啟密碼。目前只能開啟沒有加密的 .xlsx 和 .pptx 檔案。",
    office_not_office: "這不是 Excel 或 PowerPoint 檔案，或者檔案已經損壞。",
    office_wrong_kind: "檔案的內容和副檔名不符：例如一份簡報被命名成了 .xlsx。",
    office_too_large: "這個檔案超出了安全限制（部件太多、太大，或壓縮得異常），沒有開啟。",
    office_broken: "檔案已損壞，讀不出來：{reason}",
    heading_level: "標題 {level}",
    menu_format: "格式",
    format_bold: "粗體",
    format_italic: "斜體",
    format_strike: "刪除線",
    format_code: "行內程式碼",
    format_bullet: "項目符號清單",
    format_numbered: "編號清單",
    format_task: "待辦清單",
    format_quote: "引用",
    format_link: "連結",
    format_image: "圖片",
    format_table: "表格",
};

pub const FR: Strings = Strings {
    tagline: "Une suite bureautique native, libre et pensée pour le hors-ligne",
    new_document: "Nouveau",
    recent: "Récents",
    files: "Fichiers",
    placeholder: "À venir",
    placeholder_view: "Vue provisoire",
    search_files: "Rechercher des fichiers",
    start: "Accueil",
    language: "Langue",
    writer_label: "Traitement de texte",
    sheets_label: "Feuille de calcul",
    slides_label: "Présentation",
    markdown_label: "Markdown",
    pdf_label: "PDF",
    writer_tagline: "Documents, rapports, lettres",
    sheets_tagline: "Tableaux, formules, graphiques",
    slides_tagline: "Diapositives et exposés",
    markdown_tagline: "Notes et écriture en texte brut",
    pdf_tagline: "Lecture et annotation",
    writer_untitled: "Document sans titre",
    sheets_untitled: "Feuille de calcul sans titre",
    slides_untitled: "Présentation sans titre",
    markdown_untitled: "Note sans titre",
    pdf_untitled: "Document PDF",
    placeholder_title: "L’éditeur {name} n’existe pas encore",
    placeholder_body: "Cet onglet montre seulement la disposition de l’interface. Les onglets, la barre d’outils et la barre d’état changent selon le type de document, mais rien ici ne peut être modifié et tous les boutons de la barre d’outils sont désactivés. Le tracé à l’arrière-plan est une esquisse de la disposition, pas une vue fonctionnelle.",
    planned_phase: "D’après la feuille de route, {name} arrive en Phase {phase}.",
    pdf_undecided: "La présence d’un lecteur PDF dans la première version n’est pas encore décidée.",
    plain_text_note: "Prototype en texte brut",
    word_count: "{count} mots",
    char_count: "{count} caractères",
    ime_enabled: "Méthode de saisie active",
    ime_composing: "Composition en cours",
    ime_disabled: "Méthode de saisie inactive",
    style_body: "Corps de texte",
    number_general: "Standard",
    ai_title: "Assistant IA",
    ai_body: "Ni l’IA intégrée ni le serveur MCP n’existent encore. Ce panneau leur réserve seulement une place. Ce qui est prévu :",
    ai_point_tools: "L’assistant intégré et les clients MCP externes utilisent un seul et même catalogue d’outils de document.",
    ai_point_approval: "Toute action qui modifie un document demande d’abord votre accord, et chaque appel est consigné.",
    ai_point_keys: "Vous choisissez le fournisseur et fournissez la clé ; les clés restent sur cet ordinateur.",
    ai_input: "Interroger l’IA (pas encore disponible)",
    writer_sample: "Prototype de l’interface d’Aster Office\n\nCette page n’est pas un champ de texte de la bibliothèque d’interface. Le texte est composé par Parley, dessiné par vello_cpu et placé dans la fenêtre sous forme d’image ; les événements de la méthode de saisie viennent directement du système de fenêtrage.\n\nCe qui fonctionne : saisir du français, du chinois et du japonais, déplacer le curseur, sélectionner, faire défiler, zoomer.\nCe qui ne fonctionne pas : la mise en forme, l’annulation, l’ouverture et l’enregistrement. Les boutons grisés de la barre d’outils ne sont reliés à rien pour l’instant.\n\n中文输入也请试一下。日本語の入力も試してください。\n\n",

    menu_file: "Fichier",
    menu_edit: "Édition",
    menu_new_markdown: "Nouveau document Markdown",
    menu_open: "Ouvrir…",
    menu_save: "Enregistrer",
    menu_save_as: "Enregistrer sous…",
    menu_close_tab: "Fermer l’onglet",
    menu_quit: "Quitter Aster Office",
    menu_undo: "Annuler la saisie",
    menu_redo: "Rétablir",
    menu_cut: "Couper",
    menu_copy: "Copier",
    menu_paste: "Coller",
    menu_select_all: "Tout sélectionner",
    open_file: "Ouvrir un fichier…",
    unsaved_title: "Enregistrer les modifications de « {name} » ?",
    unsaved_body: "Si vous n’enregistrez pas, vos modifications seront perdues.",
    quit_title: "{count} documents ont des modifications non enregistrées",
    quit_body: "Si vous quittez maintenant, les modifications non enregistrées de ces documents seront perdues.",
    button_save: "Enregistrer",
    button_discard: "Ne pas enregistrer",
    button_cancel: "Annuler",
    button_ok: "OK",
    button_quit_anyway: "Abandonner les modifications et quitter",
    open_failed: "Impossible d’ouvrir « {name} »",
    save_failed: "Impossible d’enregistrer « {name} »",
    not_utf8: "Ce fichier n’est pas en UTF-8. Pour l’instant, Aster Office ne lit et n’écrit que du texte UTF-8. Le fichier n’a pas été modifié.",
    is_utf16: "Ce fichier est en UTF-16. Pour l’instant, Aster Office ne lit et n’écrit que du texte UTF-8. Le fichier n’a pas été modifié.",
    markdown_note: "Coloration syntaxique et aperçu ; les images restent des cadres vides",
    mixed_line_endings: "Fins de ligne mélangées : l’enregistrement les unifiera en {style}",
    file_type_text: "Markdown et texte brut",
    open_dialog: "Ouvrir",
    save_dialog: "Enregistrer sous",
    recovery_title: "Récupérer les documents non enregistrés",
    recovery_body: "Aster Office ne s’est pas fermé normalement la dernière fois. Ces documents contiennent des modifications non enregistrées. Choisissez ce qu’il faut récupérer :",
    recovery_restore: "Récupérer",
    recovery_discard: "Abandonner",
    recovery_restore_all: "Tout récupérer",
    recovery_later: "Décider plus tard",
    recovery_new_document: "Un nouveau document jamais enregistré",
    recovery_just_now: "à l’instant",
    recovery_minutes: "il y a {count} min",
    recovery_hours: "il y a {count} h",
    recovery_days: "il y a {count} jours",
    recovery_disk_changed: "Le fichier d’origine a été modifié par un autre programme entre-temps",
    recovery_disk_missing: "Le fichier d’origine n’existe plus",
    recovery_disk_unreadable: "Impossible de lire le fichier d’origine : {reason}",
    recovery_unreadable: "Impossible de lire cet enregistrement : {reason}",
    notice_disk_changed: "Le fichier d’origine a été modifié par un autre programme avant la récupération ; l’enregistrement demandera où enregistrer",
    notice_disk_missing: "Le fichier d’origine n’existe plus ; l’enregistrement demandera où enregistrer",
    notice_record_failed: "Impossible d’écrire l’enregistrement de récupération ({reason}) ; ce document pourrait être perdu en cas de plantage",
    notice_link_refused: "Seuls les liens web et e-mail s’ouvrent ; ce lien n’a pas été ouvert",
    notice_opening: "Ouverture de {name}…",
    preview_too_large: "L’aperçu est désactivé au-delà de {size}",
    menu_find: "Rechercher…",
    menu_find_next: "Rechercher le suivant",
    menu_find_previous: "Rechercher le précédent",
    menu_find_selection: "Rechercher la sélection",
    menu_replace: "Remplacer…",
    find_placeholder: "Rechercher",
    replace_placeholder: "Remplacer par",
    find_match_case: "Respecter la casse",
    find_whole_word: "Mot entier",
    find_regex: "Expression régulière",
    find_close: "Fermer la recherche",
    replace_one: "Remplacer",
    replace_all: "Tout remplacer",
    find_position: "{index} sur {count}",
    find_none: "Aucun résultat",
    find_counting: "Décompte…",
    find_bad_pattern: "Expression régulière invalide",
    find_too_many: "Plus de {count}",
    notice_replaced: "{count} remplacement(s)",
    notice_replacing: "Remplacement en cours…",
    menu_open_folder: "Ouvrir un dossier…",
    menu_clear_recent: "Effacer les fichiers récents",
    open_folder_dialog: "Ouvrir un dossier",
    reveal_finder: "Afficher dans le Finder",
    reveal_other: "Afficher dans le gestionnaire de fichiers",
    recent_remove: "Retirer des fichiers récents",
    recent_empty: "Aucun fichier ouvert pour l’instant. Les fichiers ouverts ou enregistrés apparaissent ici.",
    recent_missing: "Indisponible",
    recent_missing_title: "« {name} » est introuvable",
    recent_missing_body: "Il se trouvait dans {path}. Il a peut-être été déplacé, renommé ou supprimé.",
    no_folder: "Ouvrez un dossier pour afficher ici les documents qu’il contient.",
    refresh: "Actualiser",
    tree_reading: "Lecture…",
    tree_empty: "Aucun fichier à afficher",
    tree_omitted: "{count} autres non affichés",
    tree_omitted_at_least: "Au moins {count} autres non affichés",
    tree_missing: "Le dossier n’existe plus",
    tree_denied: "Lecture non autorisée",
    tree_not_folder: "Ce n’est pas un dossier",
    tree_failed: "Lecture impossible : {reason}",
    unsupported: "Pas encore pris en charge",
    unsupported_title: "Impossible d’ouvrir « {name} » pour l’instant",
    unsupported_body: "Cette version ouvre les fichiers Markdown et texte (.md, .markdown, .txt), ainsi que les fichiers Excel (.xlsx) et PowerPoint (.pptx) en lecture seule. Les fichiers Word et PDF seront pris en charge dans une version ultérieure.",
    drop_to_open: "Déposez pour ouvrir",
    loop_title: "« {name} » ne se déplie pas",
    loop_body: "C’est un lien vers le dossier qui le contient ou vers un dossier parent. Le déplier répéterait les mêmes dossiers sans fin.",
    reveal_failed: "Impossible d’afficher « {name} » dans le gestionnaire de fichiers",
    no_matches: "Aucun fichier correspondant",
    opened_today: "Aujourd’hui {time}",
    opened_yesterday: "Hier {time}",
    opened_this_year: "{day}/{month}",
    opened_other_year: "{day}/{month}/{year}",
    callout_note: "Remarque",
    callout_tip: "Astuce",
    callout_important: "Important",
    callout_warning: "Avertissement",
    callout_caution: "Attention",
    file_type_supported: "Tous les fichiers pris en charge",
    file_type_spreadsheet: "Classeurs Excel",
    file_type_presentation: "Présentations PowerPoint",
    read_only_note: "Lecture seule",
    cell_count: "{count} cellules",
    slide_position: "Diapositive {index} sur {count}",
    calc_running: "Calcul des formules…",
    calc_done: "{count} formules calculées",
    calc_failed: "Les formules n’ont pas été calculées ; les résultats enregistrés dans le fichier sont affichés : {reason}",
    chart_sheet: "Ceci est une feuille de graphique. Les graphiques ne peuvent pas encore être affichés.",
    no_slides: "Cette présentation ne contient aucune diapositive.",
    slide_hidden: "Masquée",
    shape_picture: "Image",
    shape_table: "Tableau",
    shape_chart: "Graphique",
    shape_graphic: "Objet",
    office_legacy: "Ce fichier est au format Office 97–2003 ou protégé par un mot de passe. Seuls les fichiers .xlsx et .pptx non chiffrés peuvent être ouverts pour l’instant.",
    office_not_office: "Ce n’est pas un fichier Excel ou PowerPoint, ou il est endommagé.",
    office_wrong_kind: "Le contenu du fichier ne correspond pas à son extension, par exemple une présentation nommée .xlsx.",
    office_too_large: "Ce fichier dépasse une limite de sécurité (trop de parties, parties trop grandes ou compression anormale) et n’a pas été ouvert.",
    office_broken: "Le fichier est endommagé et illisible : {reason}",
    heading_level: "Titre {level}",
    menu_format: "Format",
    format_bold: "Gras",
    format_italic: "Italique",
    format_strike: "Barré",
    format_code: "Code en ligne",
    format_bullet: "Liste à puces",
    format_numbered: "Liste numérotée",
    format_task: "Liste de tâches",
    format_quote: "Citation",
    format_link: "Lien",
    format_image: "Image",
    format_table: "Tableau",
};

pub const DE: Strings = Strings {
    tagline: "Eine native, quelloffene Office-Suite, die zuerst offline funktioniert",
    new_document: "Neu",
    recent: "Zuletzt verwendet",
    files: "Dateien",
    placeholder: "Geplant",
    placeholder_view: "Platzhalter",
    search_files: "Dateien suchen",
    start: "Startseite",
    language: "Sprache",
    writer_label: "Textdokument",
    sheets_label: "Tabelle",
    slides_label: "Präsentation",
    markdown_label: "Markdown",
    pdf_label: "PDF",
    writer_tagline: "Dokumente, Berichte, Briefe",
    sheets_tagline: "Tabellen, Formeln, Diagramme",
    slides_tagline: "Folien und Vorträge",
    markdown_tagline: "Notizen und reiner Text",
    pdf_tagline: "Lesen und Kommentieren",
    writer_untitled: "Unbenanntes Dokument",
    sheets_untitled: "Unbenannte Tabelle",
    slides_untitled: "Unbenannte Präsentation",
    markdown_untitled: "Unbenannte Notiz",
    pdf_untitled: "PDF-Dokument",
    placeholder_title: "Der {name}-Editor ist noch nicht gebaut",
    placeholder_body: "Dieser Tab zeigt nur den Aufbau der Oberfläche. Tabs, Symbolleiste und Statusleiste wechseln mit der Art des Dokuments, aber hier lässt sich nichts bearbeiten, und alle Schaltflächen der Symbolleiste sind deaktiviert. Die Linien im Hintergrund sind eine Skizze des Aufbaus, keine funktionierende Ansicht.",
    planned_phase: "Laut Fahrplan kommt {name} in Phase {phase}.",
    pdf_undecided: "Ob die erste Version einen PDF-Betrachter enthält, ist noch nicht entschieden.",
    plain_text_note: "Prototyp für reinen Text",
    word_count: "{count} Wörter",
    char_count: "{count} Zeichen",
    ime_enabled: "Eingabemethode aktiv",
    ime_composing: "Eingabe läuft",
    ime_disabled: "Eingabemethode inaktiv",
    style_body: "Fließtext",
    number_general: "Standard",
    ai_title: "KI-Assistent",
    ai_body: "Weder die eingebaute KI noch der MCP-Server existieren schon. Dieses Panel hält nur ihren Platz frei. Geplant ist:",
    ai_point_tools: "Der eingebaute Assistent und externe MCP-Clients nutzen denselben Katalog von Dokumentwerkzeugen.",
    ai_point_approval: "Alles, was ein Dokument verändert, fragt vorher nach, und jeder Aufruf wird protokolliert.",
    ai_point_keys: "Anbieter und Schlüssel legen Sie selbst fest; Schlüssel bleiben auf diesem Rechner.",
    ai_input: "KI fragen (noch nicht gebaut)",
    writer_sample: "Prototyp der Aster-Office-Oberfläche\n\nDiese Seite ist kein Textfeld des UI-Toolkits. Der Text wird von Parley gesetzt, von vello_cpu gezeichnet und als ein Bild ins Fenster gelegt; die Ereignisse der Eingabemethode kommen direkt vom Fenstersystem.\n\nWas geht: Deutsch, Chinesisch und Japanisch tippen, die Einfügemarke bewegen, markieren, scrollen, zoomen.\nWas nicht geht: Formatierung, Rückgängig, Öffnen und Speichern. Die ausgegrauten Schaltflächen der Symbolleiste sind noch mit nichts verbunden.\n\n中文输入也请试一下。日本語の入力も試してください。\n\n",

    menu_file: "Datei",
    menu_edit: "Bearbeiten",
    menu_new_markdown: "Neues Markdown-Dokument",
    menu_open: "Öffnen…",
    menu_save: "Speichern",
    menu_save_as: "Speichern unter…",
    menu_close_tab: "Tab schließen",
    menu_quit: "Aster Office beenden",
    menu_undo: "Rückgängig",
    menu_redo: "Wiederholen",
    menu_cut: "Ausschneiden",
    menu_copy: "Kopieren",
    menu_paste: "Einfügen",
    menu_select_all: "Alles auswählen",
    open_file: "Datei öffnen…",
    unsaved_title: "Änderungen an „{name}“ speichern?",
    unsaved_body: "Wenn Sie nicht speichern, gehen Ihre Änderungen verloren.",
    quit_title: "{count} Dokumente haben ungespeicherte Änderungen",
    quit_body: "Wenn Sie jetzt beenden, gehen die ungespeicherten Änderungen in diesen Dokumenten verloren.",
    button_save: "Speichern",
    button_discard: "Nicht speichern",
    button_cancel: "Abbrechen",
    button_ok: "OK",
    button_quit_anyway: "Änderungen verwerfen und beenden",
    open_failed: "„{name}“ konnte nicht geöffnet werden",
    save_failed: "„{name}“ konnte nicht gespeichert werden",
    not_utf8: "Diese Datei ist nicht UTF-8-kodiert. Aster Office liest und schreibt vorerst nur UTF-8-Text. Die Datei wurde nicht verändert.",
    is_utf16: "Diese Datei ist UTF-16-kodiert. Aster Office liest und schreibt vorerst nur UTF-8-Text. Die Datei wurde nicht verändert.",
    markdown_note: "Syntaxhervorhebung und Vorschau; Bilder nur als Platzhalter",
    mixed_line_endings: "Gemischte Zeilenenden: Beim Speichern werden sie zu {style} vereinheitlicht",
    file_type_text: "Markdown und reiner Text",
    open_dialog: "Öffnen",
    save_dialog: "Speichern unter",
    recovery_title: "Nicht gespeicherte Dokumente wiederherstellen",
    recovery_body: "Aster Office wurde beim letzten Mal nicht normal beendet. Diese Dokumente haben nicht gespeicherte Änderungen. Wählen Sie, was wiederhergestellt werden soll:",
    recovery_restore: "Wiederherstellen",
    recovery_discard: "Verwerfen",
    recovery_restore_all: "Alle wiederherstellen",
    recovery_later: "Später entscheiden",
    recovery_new_document: "Ein neues, nie gespeichertes Dokument",
    recovery_just_now: "gerade eben",
    recovery_minutes: "vor {count} Min.",
    recovery_hours: "vor {count} Std.",
    recovery_days: "vor {count} Tagen",
    recovery_disk_changed: "Die Originaldatei wurde inzwischen von einem anderen Programm geändert",
    recovery_disk_missing: "Die Originaldatei existiert nicht mehr",
    recovery_disk_unreadable: "Die Originaldatei kann nicht gelesen werden: {reason}",
    recovery_unreadable: "Dieser Eintrag kann nicht gelesen werden: {reason}",
    notice_disk_changed: "Die Originaldatei wurde vor der Wiederherstellung von einem anderen Programm geändert; beim Speichern wird nach dem Ort gefragt",
    notice_disk_missing: "Die Originaldatei existiert nicht mehr; beim Speichern wird nach dem Ort gefragt",
    notice_record_failed: "Der Wiederherstellungseintrag kann nicht geschrieben werden ({reason}); dieses Dokument könnte bei einem Absturz verloren gehen",
    notice_link_refused: "Nur Web- und E-Mail-Links werden geöffnet; dieser Link nicht",
    notice_opening: "{name} wird geöffnet …",
    preview_too_large: "Die Vorschau ist bei Dokumenten über {size} ausgeschaltet",
    menu_find: "Suchen …",
    menu_find_next: "Weitersuchen",
    menu_find_previous: "Rückwärts suchen",
    menu_find_selection: "Auswahl suchen",
    menu_replace: "Ersetzen …",
    find_placeholder: "Suchen",
    replace_placeholder: "Ersetzen durch",
    find_match_case: "Groß-/Kleinschreibung",
    find_whole_word: "Ganzes Wort",
    find_regex: "Regulärer Ausdruck",
    find_close: "Suche schließen",
    replace_one: "Ersetzen",
    replace_all: "Alle ersetzen",
    find_position: "{index} von {count}",
    find_none: "Keine Treffer",
    find_counting: "Wird gezählt …",
    find_bad_pattern: "Ungültiger regulärer Ausdruck",
    find_too_many: "Mehr als {count}",
    notice_replaced: "{count}-mal ersetzt",
    notice_replacing: "Alle werden ersetzt …",
    menu_open_folder: "Ordner öffnen …",
    menu_clear_recent: "Liste „Zuletzt verwendet“ leeren",
    open_folder_dialog: "Ordner öffnen",
    reveal_finder: "Im Finder zeigen",
    reveal_other: "Im Dateimanager zeigen",
    recent_remove: "Aus „Zuletzt verwendet“ entfernen",
    recent_empty: "Noch keine Dateien geöffnet. Geöffnete oder gesicherte Dateien erscheinen hier.",
    recent_missing: "Nicht verfügbar",
    recent_missing_title: "„{name}“ wurde nicht gefunden",
    recent_missing_body: "Die Datei lag in {path}. Sie wurde vielleicht verschoben, umbenannt oder gelöscht.",
    no_folder: "Öffnen Sie einen Ordner, um die Dokumente darin hier aufzulisten.",
    refresh: "Aktualisieren",
    tree_reading: "Wird gelesen …",
    tree_empty: "Keine Dateien zum Anzeigen",
    tree_omitted: "{count} weitere nicht aufgelistet",
    tree_omitted_at_least: "Mindestens {count} weitere nicht aufgelistet",
    tree_missing: "Der Ordner existiert nicht mehr",
    tree_denied: "Keine Leseberechtigung",
    tree_not_folder: "Kein Ordner",
    tree_failed: "Kann nicht gelesen werden: {reason}",
    unsupported: "Noch nicht unterstützt",
    unsupported_title: "„{name}“ kann noch nicht geöffnet werden",
    unsupported_body: "Diese Version öffnet Markdown- und Textdateien (.md, .markdown, .txt) sowie Excel- (.xlsx) und PowerPoint-Dateien (.pptx) zum Ansehen. Word- und PDF-Dateien werden in einer späteren Version unterstützt.",
    drop_to_open: "Zum Öffnen loslassen",
    loop_title: "„{name}“ wird nicht aufgeklappt",
    loop_body: "Es ist ein Link auf den Ordner, in dem er liegt, oder auf einen übergeordneten Ordner. Aufklappen würde dieselben Ordner endlos wiederholen.",
    reveal_failed: "„{name}“ kann nicht im Dateimanager gezeigt werden",
    no_matches: "Keine passenden Dateien",
    opened_today: "Heute {time}",
    opened_yesterday: "Gestern {time}",
    opened_this_year: "{day}.{month}.",
    opened_other_year: "{day}.{month}.{year}",
    callout_note: "Hinweis",
    callout_tip: "Tipp",
    callout_important: "Wichtig",
    callout_warning: "Warnung",
    callout_caution: "Vorsicht",
    file_type_supported: "Alle unterstützten Dateien",
    file_type_spreadsheet: "Excel-Arbeitsmappen",
    file_type_presentation: "PowerPoint-Präsentationen",
    read_only_note: "Schreibgeschützt",
    cell_count: "{count} Zellen",
    slide_position: "Folie {index} von {count}",
    calc_running: "Formeln werden berechnet …",
    calc_done: "{count} Formeln berechnet",
    calc_failed: "Formeln wurden nicht berechnet; angezeigt werden die in der Datei gespeicherten Ergebnisse: {reason}",
    chart_sheet: "Dies ist ein Diagrammblatt. Diagramme können noch nicht angezeigt werden.",
    no_slides: "Diese Präsentation enthält keine Folien.",
    slide_hidden: "Ausgeblendet",
    shape_picture: "Bild",
    shape_table: "Tabelle",
    shape_chart: "Diagramm",
    shape_graphic: "Grafik",
    office_legacy: "Dies ist eine Datei im Format Office 97–2003 oder eine kennwortgeschützte Datei. Derzeit lassen sich nur unverschlüsselte .xlsx- und .pptx-Dateien öffnen.",
    office_not_office: "Dies ist keine Excel- oder PowerPoint-Datei, oder sie ist beschädigt.",
    office_wrong_kind: "Der Inhalt der Datei passt nicht zu ihrer Endung, etwa eine Präsentation mit der Endung .xlsx.",
    office_too_large: "Diese Datei überschreitet eine Sicherheitsgrenze (zu viele oder zu große Teile oder ungewöhnlich stark komprimiert) und wurde nicht geöffnet.",
    office_broken: "Die Datei ist beschädigt und kann nicht gelesen werden: {reason}",
    heading_level: "Überschrift {level}",
    menu_format: "Format",
    format_bold: "Fett",
    format_italic: "Kursiv",
    format_strike: "Durchgestrichen",
    format_code: "Inline-Code",
    format_bullet: "Aufzählung",
    format_numbered: "Nummerierte Liste",
    format_task: "Aufgabenliste",
    format_quote: "Zitat",
    format_link: "Link",
    format_image: "Bild",
    format_table: "Tabelle",
};

pub const ES: Strings = Strings {
    tagline: "Una suite ofimática nativa, de código abierto y pensada para trabajar sin conexión",
    new_document: "Nuevo",
    recent: "Recientes",
    files: "Archivos",
    placeholder: "Pendiente",
    placeholder_view: "Vista provisional",
    search_files: "Buscar archivos",
    start: "Inicio",
    language: "Idioma",
    writer_label: "Documento de texto",
    sheets_label: "Hoja de cálculo",
    slides_label: "Presentación",
    markdown_label: "Markdown",
    pdf_label: "PDF",
    writer_tagline: "Documentos, informes, cartas",
    sheets_tagline: "Tablas, fórmulas, gráficos",
    slides_tagline: "Diapositivas y exposiciones",
    markdown_tagline: "Notas y escritura en texto sin formato",
    pdf_tagline: "Lectura y anotaciones",
    writer_untitled: "Documento sin título",
    sheets_untitled: "Hoja de cálculo sin título",
    slides_untitled: "Presentación sin título",
    markdown_untitled: "Nota sin título",
    pdf_untitled: "Documento PDF",
    placeholder_title: "El editor {name} aún no está hecho",
    placeholder_body: "Esta pestaña solo muestra la disposición de la interfaz. Las pestañas, la barra de herramientas y la barra de estado cambian según el tipo de documento, pero aquí no se puede editar nada y todos los botones de la barra de herramientas están desactivados. El trazado del fondo es un boceto de la disposición, no una vista que funcione.",
    planned_phase: "Según la hoja de ruta, {name} llega en la Phase {phase}.",
    pdf_undecided: "Aún no se ha decidido si la primera versión incluirá un lector de PDF.",
    plain_text_note: "Prototipo de texto sin formato",
    word_count: "{count} palabras",
    char_count: "{count} caracteres",
    ime_enabled: "Método de entrada activo",
    ime_composing: "Componiendo",
    ime_disabled: "Método de entrada inactivo",
    style_body: "Cuerpo",
    number_general: "Estándar",
    ai_title: "Asistente de IA",
    ai_body: "Ni la IA integrada ni el servidor MCP existen todavía. Este panel solo les reserva el sitio. Lo previsto:",
    ai_point_tools: "El asistente integrado y los clientes MCP externos usan un mismo catálogo de herramientas de documento.",
    ai_point_approval: "Todo lo que modifique un documento pide antes tu confirmación, y cada llamada queda registrada.",
    ai_point_keys: "Tú eliges el proveedor y aportas la clave; las claves se quedan en este equipo.",
    ai_input: "Preguntar a la IA (aún no disponible)",
    writer_sample: "Prototipo de la interfaz de Aster Office\n\nEsta página no es un campo de texto de la biblioteca de interfaz. El texto lo compone Parley, lo dibuja vello_cpu y se coloca en la ventana como una sola imagen; los eventos del método de entrada llegan directamente del sistema de ventanas.\n\nLo que funciona: escribir en español, chino y japonés, mover el cursor, seleccionar, desplazarse, ampliar y reducir.\nLo que no: el formato, deshacer, abrir y guardar. Los botones atenuados de la barra de herramientas aún no están conectados a nada.\n\n中文输入也请试一下。日本語の入力も試してください。\n\n",

    menu_file: "Archivo",
    menu_edit: "Edición",
    menu_new_markdown: "Nuevo documento Markdown",
    menu_open: "Abrir…",
    menu_save: "Guardar",
    menu_save_as: "Guardar como…",
    menu_close_tab: "Cerrar pestaña",
    menu_quit: "Salir de Aster Office",
    menu_undo: "Deshacer",
    menu_redo: "Rehacer",
    menu_cut: "Cortar",
    menu_copy: "Copiar",
    menu_paste: "Pegar",
    menu_select_all: "Seleccionar todo",
    open_file: "Abrir archivo…",
    unsaved_title: "¿Guardar los cambios de «{name}»?",
    unsaved_body: "Si no guardas, los cambios se perderán.",
    quit_title: "{count} documentos tienen cambios sin guardar",
    quit_body: "Si sales ahora, los cambios sin guardar de estos documentos se perderán.",
    button_save: "Guardar",
    button_discard: "No guardar",
    button_cancel: "Cancelar",
    button_ok: "Aceptar",
    button_quit_anyway: "Descartar los cambios y salir",
    open_failed: "No se pudo abrir «{name}»",
    save_failed: "No se pudo guardar «{name}»",
    not_utf8: "Este archivo no está en UTF-8. Por ahora Aster Office solo lee y escribe texto UTF-8. El archivo no se ha modificado.",
    is_utf16: "Este archivo está en UTF-16. Por ahora Aster Office solo lee y escribe texto UTF-8. El archivo no se ha modificado.",
    markdown_note: "Resaltado de sintaxis y vista previa; las imágenes se muestran como marcos vacíos",
    mixed_line_endings: "Finales de línea mezclados: al guardar se unificarán a {style}",
    file_type_text: "Markdown y texto sin formato",
    open_dialog: "Abrir",
    save_dialog: "Guardar como",
    recovery_title: "Recuperar documentos sin guardar",
    recovery_body: "Aster Office no se cerró correctamente la última vez. Estos documentos tienen cambios sin guardar. Elige qué recuperar:",
    recovery_restore: "Recuperar",
    recovery_discard: "Descartar",
    recovery_restore_all: "Recuperar todo",
    recovery_later: "Decidir más tarde",
    recovery_new_document: "Un documento nuevo que nunca se guardó",
    recovery_just_now: "ahora mismo",
    recovery_minutes: "hace {count} min",
    recovery_hours: "hace {count} h",
    recovery_days: "hace {count} días",
    recovery_disk_changed: "Otro programa modificó el archivo original mientras tanto",
    recovery_disk_missing: "El archivo original ya no existe",
    recovery_disk_unreadable: "No se puede leer el archivo original: {reason}",
    recovery_unreadable: "No se puede leer este registro: {reason}",
    notice_disk_changed: "Otro programa modificó el archivo original antes de la recuperación; al guardar se te preguntará dónde",
    notice_disk_missing: "El archivo original ya no existe; al guardar se te preguntará dónde",
    notice_record_failed: "No se puede escribir el registro de recuperación ({reason}); este documento podría perderse si la aplicación falla",
    notice_link_refused: "Solo se abren enlaces web y de correo; este enlace no se abrió",
    notice_opening: "Abriendo {name}…",
    preview_too_large: "La vista previa está desactivada en documentos de más de {size}",
    menu_find: "Buscar…",
    menu_find_next: "Buscar siguiente",
    menu_find_previous: "Buscar anterior",
    menu_find_selection: "Buscar la selección",
    menu_replace: "Reemplazar…",
    find_placeholder: "Buscar",
    replace_placeholder: "Reemplazar con",
    find_match_case: "Coincidir mayúsculas",
    find_whole_word: "Palabra completa",
    find_regex: "Expresión regular",
    find_close: "Cerrar búsqueda",
    replace_one: "Reemplazar",
    replace_all: "Reemplazar todo",
    find_position: "{index} de {count}",
    find_none: "Sin resultados",
    find_counting: "Contando…",
    find_bad_pattern: "Expresión regular no válida",
    find_too_many: "Más de {count}",
    notice_replaced: "{count} reemplazos",
    notice_replacing: "Reemplazando todo…",
    menu_open_folder: "Abrir carpeta…",
    menu_clear_recent: "Borrar la lista de recientes",
    open_folder_dialog: "Abrir carpeta",
    reveal_finder: "Mostrar en el Finder",
    reveal_other: "Mostrar en el gestor de archivos",
    recent_remove: "Quitar de recientes",
    recent_empty: "Todavía no has abierto ningún archivo. Los archivos que abras o guardes aparecerán aquí.",
    recent_missing: "No disponible",
    recent_missing_title: "No se encuentra «{name}»",
    recent_missing_body: "Estaba en {path}. Puede que se haya movido, renombrado o eliminado.",
    no_folder: "Abre una carpeta para ver aquí los documentos que contiene.",
    refresh: "Actualizar",
    tree_reading: "Leyendo…",
    tree_empty: "No hay archivos que mostrar",
    tree_omitted: "{count} más sin mostrar",
    tree_omitted_at_least: "Al menos {count} más sin mostrar",
    tree_missing: "La carpeta ya no existe",
    tree_denied: "Sin permiso de lectura",
    tree_not_folder: "No es una carpeta",
    tree_failed: "No se puede leer: {reason}",
    unsupported: "Aún no compatible",
    unsupported_title: "Todavía no se puede abrir «{name}»",
    unsupported_body: "Esta versión abre archivos Markdown y de texto (.md, .markdown, .txt), y archivos de Excel (.xlsx) y PowerPoint (.pptx) para verlos. Los archivos de Word y PDF serán compatibles en una versión posterior.",
    drop_to_open: "Suelta para abrir",
    loop_title: "«{name}» no se despliega",
    loop_body: "Es un enlace a la carpeta que lo contiene o a una carpeta superior. Desplegarlo repetiría las mismas carpetas sin fin.",
    reveal_failed: "No se puede mostrar «{name}» en el gestor de archivos",
    no_matches: "Ningún archivo coincide",
    opened_today: "Hoy {time}",
    opened_yesterday: "Ayer {time}",
    opened_this_year: "{day}/{month}",
    opened_other_year: "{day}/{month}/{year}",
    callout_note: "Nota",
    callout_tip: "Consejo",
    callout_important: "Importante",
    callout_warning: "Advertencia",
    callout_caution: "Precaución",
    file_type_supported: "Todos los archivos compatibles",
    file_type_spreadsheet: "Libros de Excel",
    file_type_presentation: "Presentaciones de PowerPoint",
    read_only_note: "Solo lectura",
    cell_count: "{count} celdas",
    slide_position: "Diapositiva {index} de {count}",
    calc_running: "Calculando fórmulas…",
    calc_done: "{count} fórmulas calculadas",
    calc_failed: "Las fórmulas no se calcularon; se muestran los resultados guardados en el archivo: {reason}",
    chart_sheet: "Esta es una hoja de gráfico. Los gráficos aún no se pueden mostrar.",
    no_slides: "Esta presentación no tiene diapositivas.",
    slide_hidden: "Oculta",
    shape_picture: "Imagen",
    shape_table: "Tabla",
    shape_chart: "Gráfico",
    shape_graphic: "Objeto",
    office_legacy: "Es un archivo de Office 97–2003 o protegido con contraseña. Por ahora solo se pueden abrir archivos .xlsx y .pptx sin cifrar.",
    office_not_office: "No es un archivo de Excel o PowerPoint, o está dañado.",
    office_wrong_kind: "El contenido del archivo no coincide con su extensión, por ejemplo una presentación llamada .xlsx.",
    office_too_large: "Este archivo supera un límite de seguridad (demasiadas partes, partes demasiado grandes o una compresión inusual) y no se abrió.",
    office_broken: "El archivo está dañado y no se puede leer: {reason}",
    heading_level: "Título {level}",
    menu_format: "Formato",
    format_bold: "Negrita",
    format_italic: "Cursiva",
    format_strike: "Tachado",
    format_code: "Código en línea",
    format_bullet: "Lista con viñetas",
    format_numbered: "Lista numerada",
    format_task: "Lista de tareas",
    format_quote: "Cita",
    format_link: "Enlace",
    format_image: "Imagen",
    format_table: "Tabla",
};

pub const KO: Strings = Strings {
    tagline: "네이티브, 오프라인 우선, 오픈 소스 오피스 제품군",
    new_document: "새로 만들기",
    recent: "최근 항목",
    files: "파일",
    placeholder: "미구현",
    placeholder_view: "자리 표시 화면",
    search_files: "파일 검색",
    start: "시작",
    language: "언어",
    writer_label: "문서",
    sheets_label: "스프레드시트",
    slides_label: "프레젠테이션",
    markdown_label: "Markdown",
    pdf_label: "PDF",
    writer_tagline: "문서, 보고서, 편지",
    sheets_tagline: "표, 수식, 차트",
    slides_tagline: "슬라이드와 발표",
    markdown_tagline: "메모와 일반 텍스트 작성",
    pdf_tagline: "읽기와 주석",
    writer_untitled: "제목 없는 문서",
    sheets_untitled: "제목 없는 스프레드시트",
    slides_untitled: "제목 없는 프레젠테이션",
    markdown_untitled: "제목 없는 메모",
    pdf_untitled: "PDF 문서",
    placeholder_title: "{name} 편집기는 아직 구현되지 않았습니다",
    placeholder_body: "이 탭은 셸의 레이아웃을 확인하기 위한 것입니다. 탭, 도구 모음, 상태 표시줄은 문서 종류에 따라 바뀌지만 편집 기능은 없으며 도구 모음의 버튼은 모두 비활성 상태입니다. 뒤에 보이는 선 그림은 레이아웃 스케치이며 실제 화면이 아닙니다.",
    planned_phase: "로드맵에 따르면 {name}은(는) Phase {phase}에서 제공됩니다.",
    pdf_undecided: "PDF 뷰어를 첫 버전에 포함할지는 아직 정해지지 않았습니다.",
    plain_text_note: "일반 텍스트 프로토타입",
    word_count: "{count} 단어",
    char_count: "{count} 글자",
    ime_enabled: "입력기 켜짐",
    ime_composing: "조합 중",
    ime_disabled: "입력기 꺼짐",
    style_body: "본문",
    number_general: "일반",
    ai_title: "AI 도우미",
    ai_body: "내장 AI와 MCP 서버는 아직 구현되지 않았습니다. 이 패널은 자리를 잡아 둔 것일 뿐입니다. 계획은 다음과 같습니다.",
    ai_point_tools: "내장 도우미와 외부 MCP 클라이언트는 같은 문서 도구 목록을 사용합니다.",
    ai_point_approval: "문서를 바꾸는 작업은 먼저 확인을 받고, 모든 호출을 기록합니다.",
    ai_point_keys: "제공자와 API 키는 직접 설정하며, 키는 이 기기에만 저장됩니다.",
    ai_input: "AI에게 질문하기(미구현)",
    writer_sample: "Aster Office 셸 프로토타입\n\n이 페이지는 UI 프레임워크의 텍스트 컨트롤이 아닙니다. 글자는 Parley가 조판하고 vello_cpu가 그린 뒤 한 장의 이미지로 창에 올라가며, 입력기 이벤트는 창 시스템에서 직접 받습니다.\n\n되는 것: 한국어, 중국어, 일본어, 영어 입력, 커서 이동, 선택, 스크롤, 확대와 축소.\n아직 안 되는 것: 서식, 실행 취소, 열기와 저장. 도구 모음의 회색 버튼은 아직 기능에 연결되어 있지 않습니다.\n\n中文输入也请试一下。日本語の入力も試してください。\n\n",

    menu_file: "파일",
    menu_edit: "편집",
    menu_new_markdown: "새 Markdown 문서",
    menu_open: "열기…",
    menu_save: "저장",
    menu_save_as: "다른 이름으로 저장…",
    menu_close_tab: "탭 닫기",
    menu_quit: "Aster Office 종료",
    menu_undo: "실행 취소",
    menu_redo: "다시 실행",
    menu_cut: "잘라내기",
    menu_copy: "복사",
    menu_paste: "붙여넣기",
    menu_select_all: "모두 선택",
    open_file: "파일 열기…",
    unsaved_title: "“{name}”의 변경 내용을 저장할까요?",
    unsaved_body: "저장하지 않으면 변경 내용이 사라집니다.",
    quit_title: "저장하지 않은 문서가 {count}개 있습니다",
    quit_body: "지금 종료하면 이 문서들의 저장하지 않은 변경 내용이 사라집니다.",
    button_save: "저장",
    button_discard: "저장 안 함",
    button_cancel: "취소",
    button_ok: "확인",
    button_quit_anyway: "변경 내용을 버리고 종료",
    open_failed: "“{name}”을(를) 열 수 없습니다",
    save_failed: "“{name}”을(를) 저장할 수 없습니다",
    not_utf8: "이 파일은 UTF-8이 아닙니다. Aster Office는 현재 UTF-8 텍스트만 읽고 씁니다. 파일은 변경되지 않았습니다.",
    is_utf16: "이 파일은 UTF-16입니다. Aster Office는 현재 UTF-8 텍스트만 읽고 씁니다. 파일은 변경되지 않았습니다.",
    markdown_note: "구문 강조와 미리 보기. 이미지는 자리 표시만 합니다",
    mixed_line_endings: "줄 끝 문자가 섞여 있어 저장할 때 {style}(으)로 통일됩니다",
    file_type_text: "Markdown 및 일반 텍스트",
    open_dialog: "열기",
    save_dialog: "다른 이름으로 저장",
    recovery_title: "저장하지 않은 문서 복구",
    recovery_body: "지난번에 Aster Office가 정상적으로 종료되지 않았습니다. 다음 문서에 저장하지 않은 변경 내용이 있습니다. 복구할 문서를 선택하세요:",
    recovery_restore: "복구",
    recovery_discard: "버리기",
    recovery_restore_all: "모두 복구",
    recovery_later: "나중에 결정",
    recovery_new_document: "한 번도 저장하지 않은 새 문서",
    recovery_just_now: "방금",
    recovery_minutes: "{count}분 전",
    recovery_hours: "{count}시간 전",
    recovery_days: "{count}일 전",
    recovery_disk_changed: "그동안 다른 프로그램이 원본 파일을 변경했습니다",
    recovery_disk_missing: "원본 파일이 더 이상 없습니다",
    recovery_disk_unreadable: "원본 파일을 읽을 수 없습니다: {reason}",
    recovery_unreadable: "이 기록을 읽을 수 없습니다: {reason}",
    notice_disk_changed: "복구 전에 다른 프로그램이 원본 파일을 변경했습니다. 저장할 때 저장 위치를 묻습니다",
    notice_disk_missing: "원본 파일이 더 이상 없습니다. 저장할 때 저장 위치를 묻습니다",
    notice_record_failed: "복구 기록을 쓸 수 없습니다({reason}). 앱이 비정상 종료되면 이 문서를 되찾지 못할 수 있습니다",
    notice_link_refused: "웹과 이메일 링크만 엽니다. 이 링크는 열지 않았습니다",
    notice_opening: "{name} 여는 중…",
    preview_too_large: "{size}를 넘는 문서는 미리 보기를 끕니다",
    menu_find: "찾기…",
    menu_find_next: "다음 찾기",
    menu_find_previous: "이전 찾기",
    menu_find_selection: "선택 항목으로 찾기",
    menu_replace: "바꾸기…",
    find_placeholder: "찾기",
    replace_placeholder: "바꿀 내용",
    find_match_case: "대소문자 구분",
    find_whole_word: "단어 단위",
    find_regex: "정규식",
    find_close: "찾기 닫기",
    replace_one: "바꾸기",
    replace_all: "모두 바꾸기",
    find_position: "{index} / {count}",
    find_none: "결과 없음",
    find_counting: "세는 중…",
    find_bad_pattern: "정규식이 잘못되었습니다",
    find_too_many: "{count}개 이상",
    notice_replaced: "{count}곳을 바꿨습니다",
    notice_replacing: "모두 바꾸는 중…",
    menu_open_folder: "폴더 열기…",
    menu_clear_recent: "최근 항목 지우기",
    open_folder_dialog: "폴더 열기",
    reveal_finder: "Finder에서 보기",
    reveal_other: "파일 관리자에서 보기",
    recent_remove: "최근 항목에서 제거",
    recent_empty: "아직 연 파일이 없습니다. 열거나 저장한 파일이 여기에 표시됩니다.",
    recent_missing: "사용할 수 없음",
    recent_missing_title: "“{name}”을(를) 찾을 수 없습니다",
    recent_missing_body: "이 파일은 {path}에 있었습니다. 이동되었거나 이름이 바뀌었거나 삭제되었을 수 있습니다.",
    no_folder: "폴더를 열면 그 안의 문서가 여기에 표시됩니다.",
    refresh: "새로 고침",
    tree_reading: "읽는 중…",
    tree_empty: "표시할 파일이 없습니다",
    tree_omitted: "{count}개 더 있음(표시하지 않음)",
    tree_omitted_at_least: "{count}개 이상 더 있음(표시하지 않음)",
    tree_missing: "폴더가 없습니다",
    tree_denied: "읽기 권한이 없습니다",
    tree_not_folder: "폴더가 아닙니다",
    tree_failed: "읽을 수 없음: {reason}",
    unsupported: "아직 지원되지 않음",
    unsupported_title: "“{name}”은(는) 아직 열 수 없습니다",
    unsupported_body: "이 버전에서는 Markdown과 일반 텍스트 파일(.md, .markdown, .txt), 그리고 Excel(.xlsx)과 PowerPoint(.pptx) 파일을 열 수 있으며 Excel과 PowerPoint는 보기만 가능합니다. Word와 PDF 파일은 이후 버전에서 지원합니다.",
    drop_to_open: "놓으면 열립니다",
    loop_title: "“{name}”은(는) 펼칠 수 없습니다",
    loop_body: "자신이 들어 있는 폴더나 그 상위 폴더를 가리키는 링크입니다. 펼치면 같은 폴더가 끝없이 반복됩니다.",
    reveal_failed: "“{name}”을(를) 파일 관리자에서 표시할 수 없습니다",
    no_matches: "일치하는 파일이 없습니다",
    opened_today: "오늘 {time}",
    opened_yesterday: "어제 {time}",
    opened_this_year: "{month}월 {day}일",
    opened_other_year: "{year}. {month}. {day}.",
    callout_note: "참고",
    callout_tip: "팁",
    callout_important: "중요",
    callout_warning: "경고",
    callout_caution: "주의",
    file_type_supported: "지원하는 모든 파일",
    file_type_spreadsheet: "Excel 통합 문서",
    file_type_presentation: "PowerPoint 프레젠테이션",
    read_only_note: "읽기 전용",
    cell_count: "셀 {count}개",
    slide_position: "슬라이드 {index}/{count}",
    calc_running: "수식을 계산하는 중…",
    calc_done: "수식 {count}개를 계산했습니다",
    calc_failed: "수식을 계산하지 못해 파일에 저장된 결과를 표시합니다: {reason}",
    chart_sheet: "차트 시트입니다. 차트는 아직 표시할 수 없습니다.",
    no_slides: "이 프레젠테이션에는 슬라이드가 없습니다.",
    slide_hidden: "숨김",
    shape_picture: "그림",
    shape_table: "표",
    shape_chart: "차트",
    shape_graphic: "그래픽",
    office_legacy: "Office 97–2003 형식의 파일이거나 암호로 보호된 파일입니다. 지금은 암호화되지 않은 .xlsx와 .pptx 파일만 열 수 있습니다.",
    office_not_office: "Excel 또는 PowerPoint 파일이 아니거나 파일이 손상되었습니다.",
    office_wrong_kind: "파일 내용이 확장자와 맞지 않습니다. 예를 들어 .xlsx라는 이름의 프레젠테이션입니다.",
    office_too_large: "이 파일은 안전 한도(부분이 너무 많거나 크거나 비정상적으로 압축됨)를 넘어서 열지 않았습니다.",
    office_broken: "파일이 손상되어 읽을 수 없습니다: {reason}",
    heading_level: "제목 {level}",
    menu_format: "서식",
    format_bold: "굵게",
    format_italic: "기울임꼴",
    format_strike: "취소선",
    format_code: "인라인 코드",
    format_bullet: "글머리 기호 목록",
    format_numbered: "번호 매기기 목록",
    format_task: "작업 목록",
    format_quote: "인용",
    format_link: "링크",
    format_image: "이미지",
    format_table: "표",
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The `{name}` markers in a template, sorted.
    fn markers(text: &str) -> Vec<&str> {
        let mut found = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            let end = start + rest[start..].find('}').expect("unclosed marker");
            found.push(&rest[start..=end]);
            rest = &rest[end + 1..];
        }
        found.sort_unstable();
        found
    }

    #[test]
    fn no_language_has_empty_text() {
        for language in Language::ALL {
            for (name, text) in language.strings().fields() {
                assert!(!text.trim().is_empty(), "{language:?} {name} is empty");
            }
        }
    }

    #[test]
    fn every_language_uses_the_same_markers() {
        let reference = Language::ZhHans.strings().fields();
        for language in Language::ALL {
            for ((name, text), (_, expected)) in
                language.strings().fields().into_iter().zip(&reference)
            {
                assert_eq!(markers(text), markers(expected), "{language:?} {name}");
            }
        }
    }

    #[test]
    fn translations_are_not_copies_of_english() {
        // Product names and format names are the same everywhere; the rest is not.
        // "Important" and "Image" are the same words in French, "Format"
        // in French and German, "Link" in German.
        let shared = [
            "markdown_label",
            "pdf_label",
            "button_ok",
            "callout_important",
            "menu_format",
            "format_image",
            "format_link",
        ];
        let english = EN.fields();
        for language in Language::ALL {
            if language == Language::En {
                continue;
            }
            for ((name, text), (_, source)) in language.strings().fields().into_iter().zip(&english)
            {
                if !shared.contains(&name) {
                    assert_ne!(text, *source, "{language:?} {name} is not translated");
                }
            }
        }
    }

    #[test]
    fn the_two_chinese_scripts_are_kept_apart() {
        // Characters whose Simplified and Traditional forms differ, as a spot
        // check that neither table was pasted into the other.
        let simplified_only = ['档', '编', '辑', '还', '实', '现', '这', '个', '页', '启'];
        let traditional_only = ['檔', '編', '輯', '還', '實', '現', '這', '個', '頁', '啟'];
        for (name, text) in ZH_HANT.fields() {
            // The sample page ends with a line inviting Simplified input.
            let text = text.replace("中文输入也请试一下。", "");
            assert!(
                !text.contains(simplified_only),
                "zh-Hant {name} has Simplified characters"
            );
        }
        for (name, text) in ZH_HANS.fields() {
            assert!(
                !text.contains(traditional_only),
                "zh-Hans {name} has Traditional characters"
            );
        }
    }

    #[test]
    fn markers_are_filled() {
        assert_eq!(fill("{count} 字", "count", "1,234"), "1,234 字");
        assert_eq!(fill("no marker", "count", "5"), "no marker");
        for language in Language::ALL {
            let strings = language.strings();
            for kind in DocKind::ALL {
                let plan = strings.plan(kind);
                assert!(!plan.contains('{'), "{language:?} {kind:?}: {plan}");
                if kind.planned_phase().is_some() {
                    assert!(plan.contains(kind.name()), "{language:?} {kind:?}: {plan}");
                }
            }
        }
    }

    #[test]
    fn per_kind_text_is_distinct() {
        for language in Language::ALL {
            let strings = language.strings();
            let mut labels: Vec<_> = DocKind::ALL
                .into_iter()
                .map(|kind| strings.kind_label(kind))
                .collect();
            labels.sort_unstable();
            labels.dedup();
            assert_eq!(labels.len(), DocKind::ALL.len(), "{language:?} labels");
            let mut stems: Vec<_> = DocKind::ALL
                .into_iter()
                .map(|kind| strings.untitled(kind))
                .collect();
            stems.sort_unstable();
            stems.dedup();
            assert_eq!(stems.len(), DocKind::ALL.len(), "{language:?} untitled");
        }
    }

    #[test]
    fn the_input_method_label_is_empty_until_something_is_known() {
        for language in Language::ALL {
            let strings = language.strings();
            assert_eq!(strings.ime(ImeState::Unknown), "");
            for state in [ImeState::Enabled, ImeState::Composing, ImeState::Disabled] {
                assert!(!strings.ime(state).is_empty());
            }
        }
    }

    #[test]
    fn languages_are_found_by_tag() {
        for (index, language) in Language::ALL.into_iter().enumerate() {
            assert_eq!(language.index(), index);
            assert_eq!(Language::from_index(index), Some(language));
            assert_eq!(Language::from_tag(language.tag()), Some(language));
            assert!(!language.native_name().is_empty());
        }
        assert_eq!(Language::from_tag("ja-JP"), Some(Language::Ja));
        assert_eq!(Language::from_tag("EN"), Some(Language::En));
        assert_eq!(Language::from_tag("fr_FR.UTF-8"), Some(Language::Fr));
        assert_eq!(Language::from_tag("de-AT"), Some(Language::De));
        assert_eq!(Language::from_tag("es-MX"), Some(Language::Es));
        assert_eq!(Language::from_tag("ko-KR"), Some(Language::Ko));
        assert_eq!(Language::from_tag("pt"), None);
        assert_eq!(Language::from_index(Language::ALL.len()), None);
    }

    #[test]
    fn chinese_tags_pick_the_script() {
        for tag in [
            "zh",
            "zh-CN",
            "zh_CN.UTF-8",
            "zh-Hans",
            "zh-SG",
            "zh-Hans-HK",
        ] {
            assert_eq!(Language::from_tag(tag), Some(Language::ZhHans), "{tag}");
        }
        for tag in [
            "zh-TW",
            "zh_TW.UTF-8",
            "zh-Hant",
            "zh-HK",
            "zh-Hant-CN",
            "zh-MO",
        ] {
            assert_eq!(Language::from_tag(tag), Some(Language::ZhHant), "{tag}");
        }
    }

    #[test]
    fn the_writer_sample_ends_with_room_to_type() {
        for language in Language::ALL {
            assert!(language.strings().writer_sample.ends_with("\n\n"));
        }
    }
}
