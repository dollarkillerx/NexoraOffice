//! The paragraphs every command and test runs on.

pub struct Sample {
    pub name: &'static str,
    /// BCP 47 language of the content; selects line-breaking tailoring and fonts.
    pub language: &'static str,
    pub text: &'static str,
}

pub const SAMPLES: [Sample; 5] = [
    Sample {
        name: "zh-prose",
        language: "zh-Hans",
        text: "排版引擎需要处理“禁则”：句号、逗号和后引号（」』）》等）不能出现在行首，前引号（「『（《等）不能出现在行尾。连续的标点，例如“」，「”或者“）。”，中间的空白应当被挤压，否则版面会显得松散。",
    },
    Sample {
        name: "ja-prose",
        language: "ja",
        text: "日本語の組版では、行頭に句読点や閉じ括弧（」）、小書きの仮名（ゃ・ゅ・ょ・っ）、長音記号「ー」などを置くことはできません。これを「行頭禁則」と呼びます。コンピューターでチェックしましょう。",
    },
    Sample {
        name: "mixed",
        language: "zh-Hans",
        text: "Aster Office 使用 Rust 1.95 和 Slint 1.19 开发，在 macOS、Windows 11 与 Linux 上运行。打开一个 10 MB 的 DOCX 文件不应超过 1.5 秒，网址 https://example.com/docs/long-path 只能在斜杠之后断开。",
    },
    Sample {
        name: "dialogue",
        language: "zh-Hans",
        text: "他说：“今天的会议改到下午三点。”\n她问：“（真的吗？）那我们还来得及吗？”\n“来得及。”他回答，“放心吧……”",
    },
    Sample {
        name: "english",
        language: "en",
        text: "Line breaking for Latin text must keep words whole, break after hyphens in well-known compounds, and never strand punctuation at the start of a line.",
    },
];
