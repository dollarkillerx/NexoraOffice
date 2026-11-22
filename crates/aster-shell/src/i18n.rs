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
    /// Marks lists that hold fixed sample entries.
    sample,
    sample_data,
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
    /// What a document opened from the sample list holds. `{title}` is its name.
    sample_document,
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
    sample: "示例",
    sample_data: "示例数据",
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
    sample_document: "{title}\n\n这是示例文档，不是从磁盘读取的文件。最近文件列表目前是固定的示例数据，打开和保存还没有实现。\n\n",
};

pub const JA: Strings = Strings {
    tagline: "ネイティブで、オフライン優先の、オープンソースのオフィススイート",
    new_document: "新規作成",
    recent: "最近使ったファイル",
    files: "ファイル",
    sample: "サンプル",
    sample_data: "サンプルデータ",
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
    sample_document: "{title}\n\nこれはサンプル文書で、ディスクから読み込んだファイルではありません。最近使ったファイルの一覧は固定のサンプルデータで、開く機能と保存機能はまだ実装されていません。\n\n",
};

pub const EN: Strings = Strings {
    tagline: "A native, offline-first, open-source office suite",
    new_document: "New",
    recent: "Recent",
    files: "Files",
    sample: "Sample",
    sample_data: "Sample data",
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
    sample_document: "{title}\n\nThis is a sample document, not a file read from disk. The list of recent files is fixed sample data for now; opening and saving are not built yet.\n\n",
};

pub const ZH_HANT: Strings = Strings {
    tagline: "原生、離線優先的開源辦公套件",
    new_document: "新增",
    recent: "最近",
    files: "檔案",
    sample: "範例",
    sample_data: "範例資料",
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
    sample_document: "{title}\n\n這是範例文件，不是從磁碟讀取的檔案。最近檔案清單目前是固定的範例資料，開啟和儲存尚未實作。\n\n",
};

pub const FR: Strings = Strings {
    tagline: "Une suite bureautique native, libre et pensée pour le hors-ligne",
    new_document: "Nouveau",
    recent: "Récents",
    files: "Fichiers",
    sample: "Exemple",
    sample_data: "Données d’exemple",
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
    sample_document: "{title}\n\nCeci est un document d’exemple, pas un fichier lu sur le disque. La liste des fichiers récents contient pour l’instant des données d’exemple fixes ; l’ouverture et l’enregistrement ne sont pas encore disponibles.\n\n",
};

pub const DE: Strings = Strings {
    tagline: "Eine native, quelloffene Office-Suite, die zuerst offline funktioniert",
    new_document: "Neu",
    recent: "Zuletzt verwendet",
    files: "Dateien",
    sample: "Beispiel",
    sample_data: "Beispieldaten",
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
    sample_document: "{title}\n\nDies ist ein Beispieldokument, keine von der Festplatte gelesene Datei. Die Liste der zuletzt verwendeten Dateien enthält vorerst feste Beispieldaten; Öffnen und Speichern sind noch nicht gebaut.\n\n",
};

pub const ES: Strings = Strings {
    tagline: "Una suite ofimática nativa, de código abierto y pensada para trabajar sin conexión",
    new_document: "Nuevo",
    recent: "Recientes",
    files: "Archivos",
    sample: "Ejemplo",
    sample_data: "Datos de ejemplo",
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
    sample_document: "{title}\n\nEste es un documento de ejemplo, no un archivo leído del disco. La lista de archivos recientes contiene por ahora datos de ejemplo fijos; abrir y guardar aún no están hechos.\n\n",
};

pub const KO: Strings = Strings {
    tagline: "네이티브, 오프라인 우선, 오픈 소스 오피스 제품군",
    new_document: "새로 만들기",
    recent: "최근 항목",
    files: "파일",
    sample: "예시",
    sample_data: "예시 데이터",
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
    sample_document: "{title}\n\n이 문서는 예시이며 디스크에서 읽은 파일이 아닙니다. 최근 파일 목록은 지금은 고정된 예시 데이터이고, 열기와 저장은 아직 구현되지 않았습니다.\n\n",
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
        let shared = ["markdown_label", "pdf_label"];
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
    fn sample_text_says_it_is_a_sample() {
        for language in Language::ALL {
            let strings = language.strings();
            let text = fill(strings.sample_document, "title", "报告");
            assert!(text.starts_with("报告\n"));
            assert!(strings.writer_sample.ends_with("\n\n"));
        }
    }
}
