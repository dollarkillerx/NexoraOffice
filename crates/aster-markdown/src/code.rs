//! Syntax colours for fenced code blocks, by the language their info string
//! names (```` ```rust ````).
//!
//! No grammar files from elsewhere: a language is a table of keywords and a
//! few switches (which quotes make strings, how comments start, whether a
//! capitalised name is a type), read by one small lexer; HTML, CSS, YAML and
//! TOML have readers of their own. The lexer reads a line at a time. What a
//! line leaves open, a block comment or a string over several lines, is
//! carried to the next in a [`LineState`], so a line is coloured knowing only
//! the state before it.
//!
//! Every reader moves forward through the line and never back, and only the
//! first [`LINE_LIMIT`] bytes of a line are read; the rest stays plain. A
//! line costs at most a few passes over 4 KB, however long it is.
//!
//! It is a hint for the eye, not a parser: a regular expression in
//! JavaScript or a here-document in a shell script can fool it, and then the
//! colours are wrong until the line where the confusion ends.

use std::ops::Range;

/// What a stretch of code is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    /// Keywords and literals like `true` and `null`; HTML tag names, CSS
    /// at-rules, C preprocessor directives.
    Keyword,
    /// Type names and lifetimes, and the names of keys: JSON, YAML and TOML
    /// keys, HTML attributes, CSS properties, shell variables.
    Type,
    /// A function or macro being called or defined; decorators and
    /// annotations.
    Function,
    String,
    /// Numbers, and constants written like them: CSS colours, HTML entities.
    Number,
    Comment,
}

impl Token {
    pub const ALL: [Token; 6] = [
        Token::Keyword,
        Token::Type,
        Token::Function,
        Token::String,
        Token::Number,
        Token::Comment,
    ];

    /// Position in [`Token::ALL`].
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// A language the lexer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Json,
    Shell,
    C,
    Cpp,
    CSharp,
    Java,
    Kotlin,
    Swift,
    Go,
    Ruby,
    Lua,
    Sql,
    Yaml,
    Toml,
    /// HTML and XML.
    Html,
    Css,
    /// SCSS and Less: CSS with `//` comments and `$variables`.
    Scss,
}

impl Language {
    pub const ALL: [Language; 21] = [
        Language::Rust,
        Language::Python,
        Language::JavaScript,
        Language::TypeScript,
        Language::Json,
        Language::Shell,
        Language::C,
        Language::Cpp,
        Language::CSharp,
        Language::Java,
        Language::Kotlin,
        Language::Swift,
        Language::Go,
        Language::Ruby,
        Language::Lua,
        Language::Sql,
        Language::Yaml,
        Language::Toml,
        Language::Html,
        Language::Css,
        Language::Scss,
    ];
}

/// The language an info string names, by its first word: `rust`,
/// `rust,ignore`, `{.python}` and `language-js` all count. `None` for a
/// language the lexer does not know, whose code then stays plain.
pub fn language(info: &str) -> Option<Language> {
    let word = info
        .split(|c: char| c.is_whitespace() || c == ',')
        .find(|word| !word.is_empty())?;
    let word = word.trim_start_matches('{').trim_start_matches('.');
    let word = word.trim_end_matches('}');
    let word = word.strip_prefix("language-").unwrap_or(word);
    if word.len() > 16 {
        return None;
    }
    let lower = word.to_ascii_lowercase();
    Some(match lower.as_str() {
        "rust" | "rs" => Language::Rust,
        "python" | "py" | "python3" | "py3" | "gyp" => Language::Python,
        "javascript" | "js" | "jsx" | "mjs" | "cjs" | "node" => Language::JavaScript,
        "typescript" | "ts" | "tsx" | "mts" | "cts" => Language::TypeScript,
        "json" | "jsonc" | "json5" | "geojson" => Language::Json,
        "bash" | "sh" | "shell" | "zsh" | "ksh" | "shellscript" => Language::Shell,
        "c" | "h" => Language::C,
        "cpp" | "c++" | "cc" | "cxx" | "hpp" | "hxx" | "h++" => Language::Cpp,
        "csharp" | "cs" | "c#" => Language::CSharp,
        "java" => Language::Java,
        "kotlin" | "kt" | "kts" => Language::Kotlin,
        "swift" => Language::Swift,
        "go" | "golang" => Language::Go,
        "ruby" | "rb" => Language::Ruby,
        "lua" => Language::Lua,
        "sql" | "mysql" | "postgresql" | "postgres" | "psql" | "pgsql" | "sqlite" | "plsql" => {
            Language::Sql
        }
        "yaml" | "yml" => Language::Yaml,
        "toml" => Language::Toml,
        "html" | "htm" | "xhtml" | "xml" | "svg" | "xsl" | "plist" | "vue" | "svelte" => {
            Language::Html
        }
        "css" => Language::Css,
        "scss" | "sass" | "less" => Language::Scss,
        _ => return None,
    })
}

/// What a line leaves open for the next one. The state before the first
/// line of a block is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LineState(Open);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum Open {
    #[default]
    Nothing,
    /// A block comment, nested this deep.
    Comment(u8),
    /// A Lua comment `--[==[`, of this level.
    LongComment(u8),
    /// A string in this quote.
    Quote(u8),
    /// A string in three of this quote.
    Triple(u8),
    /// A Rust raw string with this many `#`.
    Raw(u8),
    /// A Lua string `[==[`, of this level.
    LongString(u8),
    /// An HTML comment.
    HtmlComment,
    /// Inside an HTML tag, among its attributes.
    Tag,
}

/// How much of a line is read. Code lines are short; a longer one is
/// minified or generated, and colouring its start is enough.
pub const LINE_LIMIT: usize = 4096;

/// A line's tokens, in order and not overlapping, as byte ranges in `line`,
/// and the state it leaves for the next line. A line break at the end is
/// allowed and ignored.
pub fn tokens(
    language: Language,
    line: &str,
    state: LineState,
) -> (Vec<(Range<usize>, Token)>, LineState) {
    let mut end = line.len().min(LINE_LIMIT);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let text = line[..end].trim_end_matches(['\n', '\r']);
    let mut lexer = Lexer {
        text,
        b: text.as_bytes(),
        out: Vec::new(),
    };
    let open = match language {
        Language::Html => lexer.html(state.0),
        Language::Css => lexer.css(state.0, false),
        Language::Scss => lexer.css(state.0, true),
        Language::Yaml => lexer.yaml(),
        Language::Toml => lexer.toml(state.0),
        _ => lexer.code(spec(language), 0, state.0),
    };
    (lexer.out, LineState(open))
}

/// The tokens of text of several lines, as byte ranges in `text`, starting
/// in `state`, and the state after the last line.
pub fn highlight(
    language: Language,
    text: &str,
    state: LineState,
) -> (Vec<(Range<usize>, Token)>, LineState) {
    let mut out = Vec::new();
    let mut state = state;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let (found, after) = tokens(language, line, state);
        out.extend(
            found
                .into_iter()
                .map(|(range, token)| (at + range.start..at + range.end, token)),
        );
        state = after;
        at += line.len();
    }
    (out, state)
}

/// What sets one language apart from another, for the shared lexer.
struct Spec {
    /// Sorted, for a binary search; lowercase if `fold`.
    keywords: &'static [&'static str],
    /// Built-in type names. Sorted.
    types: &'static [&'static str],
    /// Built-in commands, shown as functions. Sorted.
    builtins: &'static [&'static str],
    /// Keywords after which the next name is a function or a type being
    /// defined: `fn`, `def`; `struct`, `class`.
    define_function: &'static [&'static str],
    define_type: &'static [&'static str],
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    nested_comments: bool,
    /// Line comments start only at the start of the line or after white
    /// space: `#` in a shell script.
    comment_after_space: bool,
    /// Quotes that make strings; of them, those without backslash escapes,
    /// those that always run over lines, and those that can be tripled.
    quotes: &'static [u8],
    raw_quotes: &'static [u8],
    long_quotes: &'static [u8],
    triple: &'static [u8],
    /// Every string may run over lines, not only those in `long_quotes`.
    multiline: bool,
    /// Names written right before a quote that belong to the string:
    /// `b"…"`, `f"…"`. Lowercase.
    prefixes: &'static [&'static str],
    /// Rust: raw strings, lifetimes and character literals, macros.
    rust: bool,
    /// Lua: long brackets `[[…]]` and comments `--[[…]]`.
    lua: bool,
    /// `#include` and other lines starting with `#`.
    preprocessor: bool,
    /// `@decorator` and `@Annotation`.
    decorators: bool,
    /// `$name` and `${…}`.
    variables: bool,
    /// A name that starts with a capital and has a small letter is a type.
    capitals: bool,
    /// Keywords and types in any case: SQL.
    fold: bool,
    /// Bytes besides letters, digits and `_` that names are made of.
    name_extra: &'static [u8],
    /// A string followed by `:` is a key: JSON.
    keys: bool,
}

const BASE: Spec = Spec {
    keywords: &[],
    types: &[],
    builtins: &[],
    define_function: &[],
    define_type: &[],
    line_comments: &[],
    block_comment: None,
    nested_comments: false,
    comment_after_space: false,
    quotes: b"\"'",
    raw_quotes: b"",
    long_quotes: b"",
    triple: b"",
    multiline: false,
    prefixes: &[],
    rust: false,
    lua: false,
    preprocessor: false,
    decorators: false,
    variables: false,
    capitals: false,
    fold: false,
    name_extra: b"",
    keys: false,
};

const C_COMMENTS: Spec = Spec {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    ..BASE
};

const RUST: Spec = Spec {
    keywords: &[
        "Self", "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
        "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
        "move", "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait",
        "true", "type", "union", "unsafe", "use", "where", "while", "yield",
    ],
    types: &[
        "bool", "char", "f32", "f64", "i128", "i16", "i32", "i64", "i8", "isize", "str", "u128",
        "u16", "u32", "u64", "u8", "usize",
    ],
    define_function: &["fn"],
    define_type: &["enum", "struct", "trait", "type", "union"],
    nested_comments: true,
    quotes: b"\"",
    multiline: true,
    prefixes: &["b", "c"],
    rust: true,
    capitals: true,
    ..C_COMMENTS
};

const PYTHON: Spec = Spec {
    keywords: &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
        "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
        "try", "while", "with", "yield",
    ],
    types: &[
        "bool",
        "bytearray",
        "bytes",
        "complex",
        "dict",
        "float",
        "frozenset",
        "int",
        "list",
        "object",
        "set",
        "str",
        "tuple",
    ],
    define_function: &["def"],
    define_type: &["class"],
    line_comments: &["#"],
    triple: b"\"'",
    prefixes: &["b", "br", "f", "fr", "r", "rb", "rf", "u"],
    decorators: true,
    capitals: true,
    ..BASE
};

const JAVASCRIPT: Spec = Spec {
    keywords: &[
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "let",
        "new",
        "null",
        "of",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    define_function: &["function"],
    define_type: &["class"],
    quotes: b"\"'`",
    long_quotes: b"`",
    decorators: true,
    capitals: true,
    name_extra: b"$",
    ..C_COMMENTS
};

const TYPESCRIPT: Spec = Spec {
    keywords: &[
        "abstract",
        "as",
        "asserts",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "declare",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "infer",
        "instanceof",
        "interface",
        "is",
        "keyof",
        "let",
        "namespace",
        "new",
        "null",
        "of",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "satisfies",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "type",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    types: &[
        "any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "unknown",
    ],
    define_type: &["class", "enum", "interface", "namespace", "type"],
    ..JAVASCRIPT
};

const JSON: Spec = Spec {
    keywords: &["false", "null", "true"],
    quotes: b"\"",
    keys: true,
    ..C_COMMENTS
};

const SHELL: Spec = Spec {
    keywords: &[
        "case", "declare", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function",
        "if", "in", "local", "readonly", "return", "select", "then", "until", "while",
    ],
    builtins: &[
        "alias", "bg", "cd", "command", "echo", "eval", "exec", "exit", "fg", "getopts", "jobs",
        "kill", "printf", "pwd", "read", "set", "shift", "source", "test", "trap", "type",
        "ulimit", "umask", "unalias", "unset", "wait",
    ],
    define_function: &["function"],
    line_comments: &["#"],
    comment_after_space: true,
    quotes: b"\"'`",
    raw_quotes: b"'",
    multiline: true,
    variables: true,
    ..BASE
};

const C: Spec = Spec {
    keywords: &[
        "NULL", "auto", "break", "case", "const", "continue", "default", "do", "else", "enum",
        "extern", "false", "for", "goto", "if", "inline", "register", "restrict", "return",
        "sizeof", "static", "struct", "switch", "true", "typedef", "union", "volatile", "while",
    ],
    types: &[
        "bool",
        "char",
        "double",
        "float",
        "int",
        "int16_t",
        "int32_t",
        "int64_t",
        "int8_t",
        "long",
        "ptrdiff_t",
        "short",
        "signed",
        "size_t",
        "ssize_t",
        "uint16_t",
        "uint32_t",
        "uint64_t",
        "uint8_t",
        "uintptr_t",
        "unsigned",
        "void",
    ],
    define_type: &["enum", "struct", "union"],
    preprocessor: true,
    ..C_COMMENTS
};

const CPP: Spec = Spec {
    keywords: &[
        "NULL",
        "alignas",
        "alignof",
        "auto",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "const_cast",
        "consteval",
        "constexpr",
        "constinit",
        "continue",
        "decltype",
        "default",
        "delete",
        "do",
        "dynamic_cast",
        "else",
        "enum",
        "explicit",
        "export",
        "extern",
        "false",
        "final",
        "for",
        "friend",
        "goto",
        "if",
        "inline",
        "mutable",
        "namespace",
        "new",
        "noexcept",
        "nullptr",
        "operator",
        "override",
        "private",
        "protected",
        "public",
        "register",
        "reinterpret_cast",
        "return",
        "sizeof",
        "static",
        "static_assert",
        "static_cast",
        "struct",
        "switch",
        "template",
        "this",
        "throw",
        "true",
        "try",
        "typedef",
        "typename",
        "union",
        "using",
        "virtual",
        "volatile",
        "while",
    ],
    types: &[
        "bool",
        "char",
        "char16_t",
        "char32_t",
        "char8_t",
        "double",
        "float",
        "int",
        "int16_t",
        "int32_t",
        "int64_t",
        "int8_t",
        "long",
        "ptrdiff_t",
        "short",
        "signed",
        "size_t",
        "string",
        "uint16_t",
        "uint32_t",
        "uint64_t",
        "uint8_t",
        "uintptr_t",
        "unsigned",
        "void",
        "wchar_t",
    ],
    define_type: &["class", "enum", "namespace", "struct", "union"],
    ..C
};

const CSHARP: Spec = Spec {
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "base",
        "break",
        "case",
        "catch",
        "checked",
        "class",
        "const",
        "continue",
        "default",
        "delegate",
        "do",
        "else",
        "enum",
        "event",
        "explicit",
        "extern",
        "false",
        "finally",
        "fixed",
        "for",
        "foreach",
        "get",
        "goto",
        "if",
        "implicit",
        "in",
        "init",
        "interface",
        "internal",
        "is",
        "lock",
        "namespace",
        "new",
        "null",
        "operator",
        "out",
        "override",
        "params",
        "private",
        "protected",
        "public",
        "readonly",
        "record",
        "ref",
        "return",
        "sealed",
        "set",
        "sizeof",
        "stackalloc",
        "static",
        "struct",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "unchecked",
        "unsafe",
        "using",
        "var",
        "virtual",
        "volatile",
        "when",
        "where",
        "while",
        "yield",
    ],
    types: &[
        "bool", "byte", "char", "decimal", "double", "dynamic", "float", "int", "long", "nint",
        "nuint", "object", "sbyte", "short", "string", "uint", "ulong", "ushort", "void",
    ],
    define_type: &["class", "enum", "interface", "record", "struct"],
    triple: b"\"",
    preprocessor: true,
    decorators: true,
    capitals: true,
    ..C_COMMENTS
};

const JAVA: Spec = Spec {
    keywords: &[
        "abstract",
        "assert",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "else",
        "enum",
        "extends",
        "false",
        "final",
        "finally",
        "for",
        "goto",
        "if",
        "implements",
        "import",
        "instanceof",
        "interface",
        "native",
        "new",
        "null",
        "package",
        "permits",
        "private",
        "protected",
        "public",
        "record",
        "return",
        "sealed",
        "static",
        "strictfp",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "transient",
        "true",
        "try",
        "var",
        "void",
        "volatile",
        "while",
        "yield",
    ],
    types: &[
        "boolean", "byte", "char", "double", "float", "int", "long", "short",
    ],
    define_type: &["class", "enum", "interface", "record"],
    triple: b"\"",
    decorators: true,
    capitals: true,
    ..C_COMMENTS
};

const KOTLIN: Spec = Spec {
    keywords: &[
        "abstract",
        "annotation",
        "as",
        "break",
        "by",
        "catch",
        "class",
        "companion",
        "const",
        "constructor",
        "continue",
        "data",
        "do",
        "else",
        "enum",
        "false",
        "final",
        "finally",
        "for",
        "fun",
        "if",
        "import",
        "in",
        "infix",
        "init",
        "inline",
        "inner",
        "interface",
        "internal",
        "is",
        "lateinit",
        "null",
        "object",
        "open",
        "operator",
        "out",
        "override",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "sealed",
        "super",
        "suspend",
        "this",
        "throw",
        "true",
        "try",
        "typealias",
        "val",
        "var",
        "vararg",
        "when",
        "where",
        "while",
    ],
    define_function: &["fun"],
    define_type: &["class", "interface", "object", "typealias"],
    nested_comments: true,
    triple: b"\"",
    decorators: true,
    capitals: true,
    ..C_COMMENTS
};

const SWIFT: Spec = Spec {
    keywords: &[
        "Self",
        "actor",
        "any",
        "as",
        "associatedtype",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "continue",
        "default",
        "defer",
        "deinit",
        "do",
        "else",
        "enum",
        "extension",
        "fallthrough",
        "false",
        "fileprivate",
        "final",
        "for",
        "func",
        "guard",
        "if",
        "import",
        "in",
        "indirect",
        "init",
        "inout",
        "internal",
        "is",
        "let",
        "mutating",
        "nil",
        "nonmutating",
        "open",
        "operator",
        "override",
        "private",
        "protocol",
        "public",
        "repeat",
        "rethrows",
        "return",
        "self",
        "some",
        "static",
        "struct",
        "subscript",
        "super",
        "switch",
        "throw",
        "throws",
        "true",
        "try",
        "typealias",
        "var",
        "weak",
        "where",
        "while",
    ],
    define_function: &["func"],
    define_type: &[
        "actor",
        "class",
        "enum",
        "extension",
        "protocol",
        "struct",
        "typealias",
    ],
    nested_comments: true,
    quotes: b"\"",
    triple: b"\"",
    preprocessor: true,
    decorators: true,
    capitals: true,
    ..C_COMMENTS
};

const GO: Spec = Spec {
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "false",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "iota",
        "map",
        "nil",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "true",
        "type",
        "var",
    ],
    types: &[
        "any",
        "bool",
        "byte",
        "complex128",
        "complex64",
        "error",
        "float32",
        "float64",
        "int",
        "int16",
        "int32",
        "int64",
        "int8",
        "rune",
        "string",
        "uint",
        "uint16",
        "uint32",
        "uint64",
        "uint8",
        "uintptr",
    ],
    define_function: &["func"],
    define_type: &["type"],
    quotes: b"\"'`",
    raw_quotes: b"`",
    long_quotes: b"`",
    ..C_COMMENTS
};

const RUBY: Spec = Spec {
    keywords: &[
        "BEGIN", "END", "__FILE__", "__LINE__", "alias", "and", "begin", "break", "case", "class",
        "def", "do", "else", "elsif", "end", "ensure", "false", "for", "if", "in", "module",
        "next", "nil", "not", "or", "redo", "rescue", "retry", "return", "self", "super", "then",
        "true", "undef", "unless", "until", "when", "while", "yield",
    ],
    define_function: &["def"],
    define_type: &["class", "module"],
    line_comments: &["#"],
    multiline: true,
    capitals: true,
    ..BASE
};

const LUA: Spec = Spec {
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if",
        "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ],
    define_function: &["function"],
    line_comments: &["--"],
    lua: true,
    ..BASE
};

const SQL: Spec = Spec {
    keywords: &[
        "add",
        "all",
        "alter",
        "and",
        "any",
        "as",
        "asc",
        "begin",
        "between",
        "by",
        "case",
        "cast",
        "check",
        "column",
        "commit",
        "constraint",
        "create",
        "cross",
        "database",
        "default",
        "delete",
        "desc",
        "distinct",
        "drop",
        "else",
        "end",
        "exists",
        "false",
        "foreign",
        "from",
        "full",
        "group",
        "having",
        "if",
        "in",
        "index",
        "inner",
        "insert",
        "intersect",
        "into",
        "is",
        "join",
        "key",
        "left",
        "like",
        "limit",
        "not",
        "null",
        "offset",
        "on",
        "or",
        "order",
        "outer",
        "primary",
        "references",
        "returning",
        "right",
        "rollback",
        "select",
        "set",
        "table",
        "then",
        "true",
        "truncate",
        "union",
        "unique",
        "update",
        "using",
        "values",
        "view",
        "when",
        "where",
        "with",
    ],
    types: &[
        "bigint",
        "bit",
        "blob",
        "boolean",
        "char",
        "date",
        "datetime",
        "decimal",
        "double",
        "float",
        "int",
        "integer",
        "interval",
        "json",
        "jsonb",
        "numeric",
        "real",
        "serial",
        "smallint",
        "text",
        "time",
        "timestamp",
        "uuid",
        "varchar",
    ],
    line_comments: &["--"],
    block_comment: Some(("/*", "*/")),
    multiline: true,
    fold: true,
    ..BASE
};

/// A YAML value, after its key.
const YAML_VALUE: Spec = Spec {
    keywords: &["false", "null", "true"],
    line_comments: &["#"],
    comment_after_space: true,
    raw_quotes: b"'",
    fold: true,
    ..BASE
};

/// A TOML value, after its key.
const TOML_VALUE: Spec = Spec {
    keywords: &["false", "inf", "nan", "true"],
    line_comments: &["#"],
    raw_quotes: b"'",
    triple: b"\"'",
    ..BASE
};

fn spec(language: Language) -> &'static Spec {
    match language {
        Language::Rust => &RUST,
        Language::Python => &PYTHON,
        Language::JavaScript => &JAVASCRIPT,
        Language::TypeScript => &TYPESCRIPT,
        Language::Json => &JSON,
        Language::Shell => &SHELL,
        Language::C => &C,
        Language::Cpp => &CPP,
        Language::CSharp => &CSHARP,
        Language::Java => &JAVA,
        Language::Kotlin => &KOTLIN,
        Language::Swift => &SWIFT,
        Language::Go => &GO,
        Language::Ruby => &RUBY,
        Language::Lua => &LUA,
        Language::Sql => &SQL,
        Language::Yaml => &YAML_VALUE,
        Language::Toml => &TOML_VALUE,
        // These have readers of their own.
        Language::Html | Language::Css | Language::Scss => &BASE,
    }
}

fn is_space(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

/// Bytes of non-ASCII characters count as letters: names may be written in
/// any script, and a token never ends inside a character.
fn is_letter(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

/// Whether `word` is in the sorted `list`, in any case if `fold`.
fn listed(list: &[&str], word: &str, fold: bool) -> bool {
    if !fold {
        return list.binary_search(&word).is_ok();
    }
    let mut buffer = [0u8; 24];
    if word.len() > buffer.len() || !word.is_ascii() {
        return false;
    }
    let lower = &mut buffer[..word.len()];
    lower.copy_from_slice(word.as_bytes());
    lower.make_ascii_lowercase();
    std::str::from_utf8(lower).is_ok_and(|lower| list.binary_search(&lower).is_ok())
}

/// One line being read.
struct Lexer<'a> {
    text: &'a str,
    b: &'a [u8],
    out: Vec<(Range<usize>, Token)>,
}

impl Lexer<'_> {
    fn push(&mut self, range: Range<usize>, token: Token) {
        if range.start < range.end {
            self.out.push((range, token));
        }
    }

    fn len(&self) -> usize {
        self.b.len()
    }

    fn at(&self, i: usize) -> Option<u8> {
        self.b.get(i).copied()
    }

    fn starts(&self, i: usize, prefix: &[u8]) -> bool {
        self.b[i.min(self.len())..].starts_with(prefix)
    }

    /// The first position from `i` that is not white space.
    fn skip_space(&self, mut i: usize) -> usize {
        while self.at(i).is_some_and(is_space) {
            i += 1;
        }
        i
    }

    /// Where a name that starts at `i` ends.
    fn name_end(&self, mut i: usize, extra: &[u8]) -> usize {
        while let Some(byte) = self.at(i) {
            if is_letter(byte) || byte.is_ascii_digit() || extra.contains(&byte) {
                i += 1;
            } else {
                break;
            }
        }
        i
    }

    /// Where a number that starts at `i` ends: digits, letters (hex digits,
    /// suffixes, units), `_`, one decimal point and a signed exponent.
    fn number_end(&self, from: usize) -> usize {
        let hex = self.starts(from, b"0x") || self.starts(from, b"0X");
        let mut i = from;
        let mut dot = false;
        while let Some(byte) = self.at(i) {
            let digit_after = |n: usize| self.at(i + n).is_some_and(|b| b.is_ascii_digit());
            if byte.is_ascii_alphanumeric() || byte == b'_' {
                if !hex
                    && matches!(byte, b'e' | b'E')
                    && matches!(self.at(i + 1), Some(b'+' | b'-'))
                    && digit_after(2)
                {
                    i += 3;
                } else {
                    i += 1;
                }
            } else if byte == b'.' && !dot && digit_after(1) {
                dot = true;
                i += 1;
            } else {
                break;
            }
        }
        i
    }

    /// Where a string in `quote` that is open at `i` closes, just after the
    /// quote; `None` if it does not close on this line.
    fn quote_end(&self, mut i: usize, quote: u8, escapes: bool) -> Option<usize> {
        while let Some(byte) = self.at(i) {
            if escapes && byte == b'\\' {
                i += 2;
            } else if byte == quote {
                return Some(i + 1);
            } else {
                i += 1;
            }
        }
        None
    }

    /// Whether the line ends in a backslash that escapes the line break.
    fn continues(&self, from: usize) -> bool {
        let tail = &self.b[from.min(self.len())..];
        tail.iter().rev().take_while(|b| **b == b'\\').count() % 2 == 1
    }

    /// Where three of `quote` open at `i` close.
    fn triple_end(&self, mut i: usize, quote: u8, escapes: bool) -> Option<usize> {
        let close = [quote; 3];
        while i < self.len() {
            if escapes && self.b[i] == b'\\' {
                i += 2;
            } else if self.starts(i, &close) {
                // A run of more quotes ends with the last three.
                let mut end = i + 3;
                while self.at(end) == Some(quote) {
                    end += 1;
                }
                return Some(end);
            } else {
                i += 1;
            }
        }
        None
    }

    /// Where a Rust raw string with `hashes` closes.
    fn raw_end(&self, mut i: usize, hashes: u8) -> Option<usize> {
        let hashes = usize::from(hashes);
        while i < self.len() {
            if self.b[i] == b'"'
                && self.b[i + 1..]
                    .iter()
                    .take(hashes)
                    .filter(|b| **b == b'#')
                    .count()
                    == hashes
            {
                return Some(i + 1 + hashes);
            }
            i += 1;
        }
        None
    }

    /// The level of a Lua long bracket `[==[` at `i`.
    fn long_open(&self, i: usize) -> Option<(u8, usize)> {
        if self.at(i) != Some(b'[') {
            return None;
        }
        let equals = self.b[i + 1..].iter().take_while(|b| **b == b'=').count();
        (self.at(i + 1 + equals) == Some(b'[') && equals < 256)
            .then_some((equals as u8, i + equals + 2))
    }

    /// Where a Lua long bracket of `level` open at `i` closes.
    fn long_end(&self, mut i: usize, level: u8) -> Option<usize> {
        let level = usize::from(level);
        while i < self.len() {
            if self.b[i] == b']'
                && self.b[i + 1..].iter().take_while(|b| **b == b'=').count() == level
                && self.at(i + 1 + level) == Some(b']')
            {
                return Some(i + level + 2);
            }
            i += 1;
        }
        None
    }

    /// Where a block comment open at `i`, `depth` deep, ends, and how deep
    /// it still is there: 0 when it closed.
    fn comment_end(&self, spec: &Spec, mut i: usize, mut depth: u8) -> (usize, u8) {
        let Some((open, close)) = spec.block_comment else {
            return (self.len(), 0);
        };
        while i < self.len() {
            if self.starts(i, close.as_bytes()) {
                i += close.len();
                depth -= 1;
                if depth == 0 {
                    return (i, 0);
                }
            } else if spec.nested_comments && self.starts(i, open.as_bytes()) {
                i += open.len();
                depth = depth.saturating_add(1);
            } else {
                i += 1;
            }
        }
        (self.len(), depth)
    }

    /// A string whose quote is at `quote_at`, its prefix starting at
    /// `start`. Returns where it ends and what it leaves open.
    fn string(&mut self, spec: &Spec, start: usize, quote_at: usize) -> (usize, Open) {
        let quote = self.b[quote_at];
        let escapes = !spec.raw_quotes.contains(&quote);
        if spec.triple.contains(&quote) && self.starts(quote_at, &[quote; 3]) {
            return match self.triple_end(quote_at + 3, quote, escapes) {
                Some(end) => {
                    self.push(start..end, Token::String);
                    (end, Open::Nothing)
                }
                None => {
                    self.push(start..self.len(), Token::String);
                    (self.len(), Open::Triple(quote))
                }
            };
        }
        match self.quote_end(quote_at + 1, quote, escapes) {
            Some(end) => {
                self.push(start..end, Token::String);
                (end, Open::Nothing)
            }
            None => {
                self.push(start..self.len(), Token::String);
                let open = spec.multiline
                    || spec.long_quotes.contains(&quote)
                    || (escapes && self.continues(quote_at + 1));
                (
                    self.len(),
                    if open {
                        Open::Quote(quote)
                    } else {
                        Open::Nothing
                    },
                )
            }
        }
    }

    /// Go on with what the line before left open. Returns where the line
    /// goes on, or what is still open at its end.
    fn resume(&mut self, spec: &Spec, from: usize, state: Open) -> Result<usize, Open> {
        let len = self.len();
        let (end, token, still) = match state {
            Open::Nothing | Open::HtmlComment | Open::Tag => return Ok(from),
            Open::Comment(depth) => {
                let (end, depth) = self.comment_end(spec, from, depth);
                (
                    end,
                    Token::Comment,
                    (depth > 0).then_some(Open::Comment(depth)),
                )
            }
            Open::LongComment(level) => match self.long_end(from, level) {
                Some(end) => (end, Token::Comment, None),
                None => (len, Token::Comment, Some(state)),
            },
            Open::LongString(level) => match self.long_end(from, level) {
                Some(end) => (end, Token::String, None),
                None => (len, Token::String, Some(state)),
            },
            Open::Raw(hashes) => match self.raw_end(from, hashes) {
                Some(end) => (end, Token::String, None),
                None => (len, Token::String, Some(state)),
            },
            Open::Triple(quote) => {
                let escapes = !spec.raw_quotes.contains(&quote);
                match self.triple_end(from, quote, escapes) {
                    Some(end) => (end, Token::String, None),
                    None => (len, Token::String, Some(state)),
                }
            }
            Open::Quote(quote) => {
                let escapes = !spec.raw_quotes.contains(&quote);
                match self.quote_end(from, quote, escapes) {
                    Some(end) => (end, Token::String, None),
                    None => {
                        let open = spec.multiline
                            || spec.long_quotes.contains(&quote)
                            || (escapes && self.continues(from));
                        (len, Token::String, open.then_some(state))
                    }
                }
            }
        };
        self.push(from..end, token);
        match still {
            Some(open) => Err(open),
            None => Ok(end),
        }
    }

    /// The shared lexer, from byte `from` of the line.
    fn code(&mut self, spec: &Spec, from: usize, state: Open) -> Open {
        let mut i = match self.resume(spec, from, state) {
            Ok(i) => i,
            Err(open) => return open,
        };
        let len = self.len();
        // What the name after a defining keyword is.
        let mut defined: Option<Token> = None;
        while i < len {
            let byte = self.b[i];
            if is_space(byte) {
                i += 1;
                continue;
            }
            let after_space = i == 0 || is_space(self.b[i - 1]);
            if spec
                .line_comments
                .iter()
                .any(|prefix| self.starts(i, prefix.as_bytes()))
                && (after_space || !spec.comment_after_space)
            {
                if spec.lua
                    && let Some((level, body)) = self.long_open(i + 2)
                {
                    match self.long_end(body, level) {
                        Some(end) => {
                            self.push(i..end, Token::Comment);
                            i = end;
                            continue;
                        }
                        None => {
                            self.push(i..len, Token::Comment);
                            return Open::LongComment(level);
                        }
                    }
                }
                self.push(i..len, Token::Comment);
                return Open::Nothing;
            }
            if let Some((open, _)) = spec.block_comment
                && self.starts(i, open.as_bytes())
            {
                let (end, depth) = self.comment_end(spec, i + open.len(), 1);
                self.push(i..end, Token::Comment);
                if depth > 0 {
                    return Open::Comment(depth);
                }
                i = end;
                continue;
            }
            if spec.lua
                && let Some((level, body)) = self.long_open(i)
            {
                match self.long_end(body, level) {
                    Some(end) => {
                        self.push(i..end, Token::String);
                        i = end;
                        continue;
                    }
                    None => {
                        self.push(i..len, Token::String);
                        return Open::LongString(level);
                    }
                }
            }
            if spec.quotes.contains(&byte) {
                let (end, open) = self.string(spec, i, i);
                if open != Open::Nothing {
                    return open;
                }
                self.key(spec, i..end);
                i = end;
                defined = None;
                continue;
            }
            if spec.rust && byte == b'\'' {
                i = self.rust_quote(i, i);
                defined = None;
                continue;
            }
            // `.5` is a number, but not the `5` of `0..5`.
            let fraction = byte == b'.'
                && self.at(i + 1).is_some_and(|b| b.is_ascii_digit())
                && (i == 0 || self.b[i - 1] != b'.');
            if byte.is_ascii_digit() || fraction {
                let end = self.number_end(i);
                self.push(i..end, Token::Number);
                i = end;
                defined = None;
                continue;
            }
            if is_letter(byte) || spec.name_extra.contains(&byte) {
                let end = self.name_end(i, spec.name_extra);
                match self.word(spec, i, end, &mut defined) {
                    Ok(end) => i = end,
                    Err(open) => return open,
                }
                continue;
            }
            if spec.preprocessor && byte == b'#' && self.b[..i].iter().all(|b| is_space(*b)) {
                i = self.directive(i);
                continue;
            }
            if spec.decorators && byte == b'@' && self.at(i + 1).is_some_and(is_letter) {
                let mut end = self.name_end(i + 1, b"");
                while self.at(end) == Some(b'.') && self.at(end + 1).is_some_and(is_letter) {
                    end = self.name_end(end + 1, b"");
                }
                self.push(i..end, Token::Function);
                i = end;
                continue;
            }
            if spec.variables && byte == b'$' {
                i = self.variable(i);
                continue;
            }
            defined = None;
            i += 1;
        }
        Open::Nothing
    }

    /// A JSON string followed by `:` is a key.
    fn key(&mut self, spec: &Spec, range: Range<usize>) {
        if spec.keys
            && self.at(self.skip_space(range.end)) == Some(b':')
            && let Some(last) = self.out.last_mut()
            && last.0 == range
        {
            last.1 = Token::Type;
        }
    }

    /// The name from `start` to `end`: a keyword, a type, a function, the
    /// prefix of a string, or nothing. Returns where to go on, or what a
    /// string it starts leaves open.
    fn word(
        &mut self,
        spec: &Spec,
        start: usize,
        end: usize,
        defined: &mut Option<Token>,
    ) -> Result<usize, Open> {
        let word = &self.text[start..end];
        let next = self.at(end);
        // A prefix right before a quote: `b"…"`, `r#"…"#`, `f'…'`.
        if let Some(quote) = next
            && (spec.quotes.contains(&quote) || (spec.rust && quote == b'\''))
            && listed(spec.prefixes, word, true)
        {
            if quote == b'\'' && spec.rust {
                return Ok(self.rust_quote(start, end));
            }
            let (end, open) = self.string(spec, start, end);
            return if open == Open::Nothing {
                Ok(end)
            } else {
                Err(open)
            };
        }
        if spec.rust && matches!(word, "r" | "br" | "cr") && matches!(next, Some(b'"' | b'#')) {
            let hashes = self.b[end..].iter().take_while(|b| **b == b'#').count();
            if self.at(end + hashes) == Some(b'"') && hashes < 256 {
                let hashes = hashes as u8;
                return match self.raw_end(end + usize::from(hashes) + 1, hashes) {
                    Some(close) => {
                        self.push(start..close, Token::String);
                        Ok(close)
                    }
                    None => {
                        self.push(start..self.len(), Token::String);
                        Err(Open::Raw(hashes))
                    }
                };
            }
        }
        let keyword = listed(spec.keywords, word, spec.fold);
        if keyword {
            self.push(start..end, Token::Keyword);
            *defined = if spec.define_function.contains(&word) {
                Some(Token::Function)
            } else if spec.define_type.contains(&word) {
                Some(Token::Type)
            } else {
                None
            };
            return Ok(end);
        }
        if let Some(token) = defined.take() {
            self.push(start..end, token);
            return Ok(end);
        }
        // A macro: `name!(…)`, but not `name != …`.
        if spec.rust && next == Some(b'!') && self.at(end + 1) != Some(b'=') {
            self.push(start..end + 1, Token::Function);
            return Ok(end + 1);
        }
        let capitalised = spec.capitals
            && word.as_bytes()[0].is_ascii_uppercase()
            && word.bytes().any(|b| b.is_ascii_lowercase());
        let token = if capitalised || listed(spec.types, word, spec.fold) {
            Some(Token::Type)
        } else if listed(spec.builtins, word, false) || next == Some(b'(') {
            Some(Token::Function)
        } else {
            None
        };
        if let Some(token) = token {
            self.push(start..end, token);
        }
        Ok(end)
    }

    /// Rust's `'`: a character literal or a lifetime, from `start` with the
    /// quote at `quote_at`.
    fn rust_quote(&mut self, start: usize, quote_at: usize) -> usize {
        let body = quote_at + 1;
        if self.at(body) == Some(b'\\') {
            let end = self.quote_end(body, b'\'', true).unwrap_or(self.len());
            self.push(start..end, Token::String);
            return end;
        }
        if let Some(c) = self.text[body.min(self.len())..].chars().next()
            && self.at(body + c.len_utf8()) == Some(b'\'')
        {
            let end = body + c.len_utf8() + 1;
            self.push(start..end, Token::String);
            return end;
        }
        if self.at(body).is_some_and(is_letter) {
            let end = self.name_end(body, b"");
            self.push(start..end, Token::Type);
            return end;
        }
        body
    }

    /// A line starting with `#`: the directive is a keyword, and the file
    /// an `#include` names a string.
    fn directive(&mut self, hash: usize) -> usize {
        let name = self.skip_space(hash + 1);
        let end = self.name_end(name, b"");
        if end == name {
            return hash + 1;
        }
        self.push(hash..end, Token::Keyword);
        if matches!(&self.text[name..end], "include" | "import") {
            let open = self.skip_space(end);
            if self.at(open) == Some(b'<')
                && let Some(close) = self.b[open..].iter().position(|b| *b == b'>')
            {
                self.push(open..open + close + 1, Token::String);
                return open + close + 1;
            }
        }
        end
    }

    /// A shell variable at the `$` at `i`: `$name`, `${…}`, `$1`, `$?`.
    fn variable(&mut self, i: usize) -> usize {
        let end = match self.at(i + 1) {
            Some(b'{') => self.b[i..]
                .iter()
                .position(|b| *b == b'}')
                .map_or(self.len(), |close| i + close + 1),
            Some(byte) if is_letter(byte) => self.name_end(i + 1, b""),
            Some(b'0'..=b'9' | b'@' | b'*' | b'#' | b'?' | b'$' | b'!' | b'-') => i + 2,
            _ => return i + 1,
        };
        self.push(i..end, Token::Type);
        end
    }

    fn html(&mut self, state: Open) -> Open {
        let len = self.len();
        let mut i = 0;
        let mut in_tag = state == Open::Tag;
        if state == Open::HtmlComment {
            match self.find(0, b"-->") {
                Some(close) => {
                    self.push(0..close + 3, Token::Comment);
                    i = close + 3;
                }
                None => {
                    self.push(0..len, Token::Comment);
                    return Open::HtmlComment;
                }
            }
        }
        let mut after_equals = false;
        while i < len {
            let byte = self.b[i];
            if in_tag {
                match byte {
                    b' ' | b'\t' => i += 1,
                    b'>' => {
                        in_tag = false;
                        i += 1;
                    }
                    b'/' | b'?' if self.at(i + 1) == Some(b'>') => {
                        in_tag = false;
                        i += 2;
                    }
                    b'"' | b'\'' => {
                        let end = self.quote_end(i + 1, byte, false).unwrap_or(len);
                        self.push(i..end, Token::String);
                        after_equals = false;
                        i = end;
                    }
                    b'=' => {
                        after_equals = true;
                        i += 1;
                    }
                    _ => {
                        let end = self.b[i..]
                            .iter()
                            .position(|b| matches!(b, b' ' | b'\t' | b'=' | b'>' | b'"' | b'\''))
                            .map_or(len, |n| i + n)
                            .max(i + 1);
                        let token = if after_equals {
                            Token::String
                        } else {
                            Token::Type
                        };
                        if byte != b'/' {
                            self.push(i..end, token);
                        }
                        after_equals = false;
                        i = end;
                    }
                }
                continue;
            }
            if self.starts(i, b"<!--") {
                match self.find(i + 4, b"-->") {
                    Some(close) => {
                        self.push(i..close + 3, Token::Comment);
                        i = close + 3;
                    }
                    None => {
                        self.push(i..len, Token::Comment);
                        return Open::HtmlComment;
                    }
                }
                continue;
            }
            if byte == b'<' {
                let mut name = i + 1;
                if matches!(self.at(name), Some(b'/' | b'!' | b'?')) {
                    name += 1;
                }
                if self.at(name).is_some_and(|b| b.is_ascii_alphabetic()) {
                    let end = self.name_end(name, b"-:.");
                    self.push(name..end, Token::Keyword);
                    in_tag = true;
                    after_equals = false;
                    i = end;
                    continue;
                }
            }
            if byte == b'&' {
                let body = self.b[i + 1..]
                    .iter()
                    .take(32)
                    .take_while(|b| b.is_ascii_alphanumeric() || **b == b'#')
                    .count();
                if body > 0 && self.at(i + 1 + body) == Some(b';') {
                    self.push(i..i + body + 2, Token::Number);
                    i += body + 2;
                    continue;
                }
            }
            i += 1;
        }
        if in_tag { Open::Tag } else { Open::Nothing }
    }

    /// The first `needle` at or after `from`.
    fn find(&self, from: usize, needle: &[u8]) -> Option<usize> {
        let from = from.min(self.len());
        self.b[from..]
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|at| from + at)
    }

    fn css(&mut self, state: Open, scss: bool) -> Open {
        const COMMENTS: Spec = Spec {
            block_comment: Some(("/*", "*/")),
            ..BASE
        };
        let len = self.len();
        let mut i = match self.resume(&COMMENTS, 0, state) {
            Ok(i) => i,
            Err(open) => return open,
        };
        // A name followed by `:` is a property, not a selector with a
        // pseudo-class, if no `{` follows on the line and a `;` or `}`
        // does, or white space comes after the colon. In parentheses, as in
        // `(max-width: 600px)`, white space after the colon is enough.
        let last_brace = self.b.iter().rposition(|b| *b == b'{');
        let last_end = self.b.iter().rposition(|b| matches!(b, b';' | b'}'));
        let mut parentheses = 0usize;
        while i < len {
            let byte = self.b[i];
            let next = self.at(i + 1);
            if is_space(byte) {
                i += 1;
            } else if self.starts(i, b"/*") {
                let (end, depth) = self.comment_end(&COMMENTS, i + 2, 1);
                self.push(i..end, Token::Comment);
                if depth > 0 {
                    return Open::Comment(depth);
                }
                i = end;
            } else if scss && self.starts(i, b"//") && (i == 0 || self.b[i - 1] != b':') {
                self.push(i..len, Token::Comment);
                return Open::Nothing;
            } else if byte == b'"' || byte == b'\'' {
                let end = self.quote_end(i + 1, byte, true).unwrap_or(len);
                self.push(i..end, Token::String);
                i = end;
            } else if (byte == b'@' || byte == b'!') && next.is_some_and(is_letter) {
                let end = self.name_end(i + 1, b"-");
                self.push(i..end, Token::Keyword);
                i = end;
            } else if byte == b'#' && next.is_some_and(|b| is_letter(b) || b.is_ascii_digit()) {
                let end = self.name_end(i + 1, b"-");
                let digits = end - i - 1;
                if matches!(digits, 3 | 4 | 6 | 8)
                    && self.b[i + 1..end].iter().all(u8::is_ascii_hexdigit)
                {
                    self.push(i..end, Token::Number);
                }
                i = end;
            } else if scss && byte == b'$' && next.is_some_and(is_letter) {
                let end = self.name_end(i + 1, b"-");
                self.push(i..end, Token::Type);
                i = end;
            } else if byte.is_ascii_digit()
                || (matches!(byte, b'.' | b'-') && next.is_some_and(|b| b.is_ascii_digit()))
            {
                let end = self.number_end(i + 1);
                let end = if self.at(end) == Some(b'%') {
                    end + 1
                } else {
                    end
                };
                self.push(i..end, Token::Number);
                i = end;
            } else if is_letter(byte) || byte == b'-' {
                let end = self.name_end(i, b"-");
                let colon = self.skip_space(end);
                let spaced = self.at(colon + 1).is_none_or(is_space);
                if self.at(end) == Some(b'(') {
                    self.push(i..end, Token::Function);
                } else if self.at(colon) == Some(b':')
                    && self.at(colon + 1) != Some(b':')
                    && if parentheses > 0 {
                        spaced
                    } else {
                        last_brace.is_none_or(|brace| brace < colon)
                            && (spaced || last_end.is_some_and(|semicolon| semicolon > colon))
                    }
                {
                    self.push(i..end, Token::Type);
                }
                i = end;
            } else {
                match byte {
                    b'(' => parentheses += 1,
                    b')' => parentheses = parentheses.saturating_sub(1),
                    _ => {}
                }
                i += 1;
            }
        }
        Open::Nothing
    }

    fn yaml(&mut self) -> Open {
        let len = self.len();
        let mut i = self.skip_space(0);
        if i == len {
            return Open::Nothing;
        }
        if self.b[i] == b'#' {
            self.push(i..len, Token::Comment);
            return Open::Nothing;
        }
        if (self.starts(i, b"---") || self.starts(i, b"...")) && self.at(i + 3).is_none_or(is_space)
        {
            self.push(i..i + 3, Token::Keyword);
            i += 3;
        }
        // Sequence entries: `- `, perhaps several.
        loop {
            i = self.skip_space(i);
            if self.at(i) == Some(b'-') && self.at(i + 1).is_none_or(is_space) {
                i += 1;
            } else {
                break;
            }
        }
        if let Some((key, after)) = self.yaml_key(i) {
            self.push(key, Token::Type);
            i = after;
        }
        // Nothing in a value is carried to the next line.
        self.code(&YAML_VALUE, i, Open::Nothing);
        Open::Nothing
    }

    /// A key starting at `i` and where its value starts.
    fn yaml_key(&self, i: usize) -> Option<(Range<usize>, usize)> {
        let colon_ends = |j: usize| self.at(j) == Some(b':') && self.at(j + 1).is_none_or(is_space);
        match self.at(i)? {
            quote @ (b'"' | b'\'') => {
                let end = self.quote_end(i + 1, quote, quote == b'"')?;
                let colon = self.skip_space(end);
                colon_ends(colon).then_some((i..end, colon + 1))
            }
            b'{' | b'[' | b'&' | b'*' | b'!' | b'|' | b'>' | b'%' | b'@' | b'`' | b'#' => None,
            _ => {
                let mut j = i;
                while j < self.len() {
                    if colon_ends(j) {
                        let key = self.text[i..j].trim_end();
                        return Some((i..i + key.len(), j + 1));
                    }
                    if self.b[j] == b'#' && is_space(self.b[j - 1]) {
                        return None;
                    }
                    j += 1;
                }
                None
            }
        }
    }

    fn toml(&mut self, state: Open) -> Open {
        if state != Open::Nothing {
            return self.code(&TOML_VALUE, 0, state);
        }
        let len = self.len();
        let mut i = self.skip_space(0);
        match self.at(i) {
            Some(b'[') => {
                // A table header: `[name]` or `[[name]]`.
                if let Some(close) = self.b[i..].iter().position(|b| *b == b']') {
                    let close = i + close;
                    let name = self.text[i..close].trim_start_matches('[');
                    let name = name.trim();
                    let start = self.text[..close].len() - self.text[i..close].len()
                        + self.text[i..close].find(name).unwrap_or(0);
                    self.push(start..start + name.len(), Token::Type);
                    i = close + 1;
                    if self.at(i) == Some(b']') {
                        i += 1;
                    }
                }
            }
            Some(b'#') | None => {}
            Some(_) => {
                // A key: everything before `=` outside quotes.
                let mut quote = None;
                let mut j = i;
                while j < len {
                    let byte = self.b[j];
                    match quote {
                        Some(open) if byte == open => quote = None,
                        Some(_) => {}
                        None if byte == b'"' || byte == b'\'' => quote = Some(byte),
                        None if byte == b'=' || byte == b'#' => break,
                        None => {}
                    }
                    j += 1;
                }
                if self.at(j) == Some(b'=') {
                    let key = self.text[i..j].trim_end();
                    self.push(i..i + key.len(), Token::Type);
                    i = j + 1;
                }
            }
        }
        self.code(&TOML_VALUE, i, Open::Nothing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tokens of a text, as the text of each with its kind.
    fn lex(language: Language, text: &str) -> Vec<(std::string::String, Token)> {
        let (tokens, _) = highlight(language, text, LineState::default());
        check(text, &tokens);
        tokens
            .into_iter()
            .map(|(range, token)| (text[range].to_owned(), token))
            .collect()
    }

    fn check(text: &str, tokens: &[(Range<usize>, Token)]) {
        let mut end = 0;
        for (range, _) in tokens {
            assert!(range.start >= end, "overlap or disorder: {tokens:?}");
            assert!(range.start < range.end, "empty token {range:?}");
            assert!(text.is_char_boundary(range.start) && text.is_char_boundary(range.end));
            end = range.end;
        }
    }

    /// The kind of the first token that is exactly `needle`.
    fn kind(language: Language, text: &str, needle: &str) -> Option<Token> {
        lex(language, text)
            .into_iter()
            .find(|(found, _)| found == needle)
            .map(|(_, token)| token)
    }

    /// Whether some token covers the first byte of `needle`, and which.
    fn kind_at(language: Language, text: &str, needle: &str) -> Option<Token> {
        let at = text.find(needle).expect("needle");
        let (tokens, _) = highlight(language, text, LineState::default());
        tokens
            .into_iter()
            .find(|(range, _)| range.contains(&at))
            .map(|(_, token)| token)
    }

    use Token::*;

    #[test]
    fn info_strings_name_languages() {
        assert_eq!(language("rust"), Some(Language::Rust));
        assert_eq!(language("rs"), Some(Language::Rust));
        assert_eq!(language("rust,ignore"), Some(Language::Rust));
        assert_eq!(language("  Python  title=\"a.py\""), Some(Language::Python));
        assert_eq!(language("{.py}"), Some(Language::Python));
        assert_eq!(language("language-js"), Some(Language::JavaScript));
        assert_eq!(language("TSX"), Some(Language::TypeScript));
        assert_eq!(language("c++"), Some(Language::Cpp));
        assert_eq!(language("c#"), Some(Language::CSharp));
        assert_eq!(language("zsh"), Some(Language::Shell));
        assert_eq!(language("yml"), Some(Language::Yaml));
        assert_eq!(language("xml"), Some(Language::Html));
        assert_eq!(language("less"), Some(Language::Scss));
        assert_eq!(language(""), None);
        assert_eq!(language("brainfuck"), None);
        assert_eq!(language("math"), None);
        assert_eq!(language(&"x".repeat(100)), None);
    }

    #[test]
    fn unknown_languages_and_plain_text_get_no_colours() {
        assert_eq!(language("text"), None);
        assert!(lex(Language::Rust, "").is_empty());
        assert!(lex(Language::Rust, "   \n\n").is_empty());
        assert!(lex(Language::Python, "abc 中文 xyz").is_empty());
    }

    #[test]
    fn word_lists_are_sorted_for_the_binary_search() {
        let specs = [
            &RUST,
            &PYTHON,
            &JAVASCRIPT,
            &TYPESCRIPT,
            &JSON,
            &SHELL,
            &C,
            &CPP,
            &CSHARP,
            &JAVA,
            &KOTLIN,
            &SWIFT,
            &GO,
            &RUBY,
            &LUA,
            &SQL,
            &YAML_VALUE,
            &TOML_VALUE,
        ];
        for spec in specs {
            for list in [spec.keywords, spec.types, spec.builtins, spec.prefixes] {
                assert!(
                    list.windows(2).all(|pair| pair[0] < pair[1]),
                    "not sorted: {list:?}"
                );
                if spec.fold {
                    assert!(list.iter().all(|word| *word == word.to_ascii_lowercase()));
                }
            }
            for word in spec.define_function.iter().chain(spec.define_type) {
                assert!(listed(spec.keywords, word, spec.fold), "{word}");
            }
        }
    }

    #[test]
    fn rust() {
        let text = "pub fn main() -> Result<(), String> {\n    let s = \"a \\\" b\"; // 注释\n    println!(\"{}\", 0x1F + 1_000u32 + 2.5e-3);\n    let c = b'\\n'; let l: &'a str = r#\"raw \" here\"#;\n}\n";
        assert_eq!(kind(Language::Rust, text, "pub"), Some(Keyword));
        assert_eq!(kind(Language::Rust, text, "main"), Some(Function));
        assert_eq!(kind(Language::Rust, text, "Result"), Some(Type));
        assert_eq!(kind(Language::Rust, text, "String"), Some(Type));
        assert_eq!(kind(Language::Rust, text, "\"a \\\" b\""), Some(String));
        assert_eq!(kind(Language::Rust, text, "// 注释"), Some(Comment));
        assert_eq!(kind(Language::Rust, text, "println!"), Some(Function));
        assert_eq!(kind(Language::Rust, text, "0x1F"), Some(Number));
        assert_eq!(kind(Language::Rust, text, "1_000u32"), Some(Number));
        assert_eq!(kind(Language::Rust, text, "2.5e-3"), Some(Number));
        assert_eq!(kind(Language::Rust, text, "b'\\n'"), Some(String));
        assert_eq!(kind(Language::Rust, text, "'a"), Some(Type));
        assert_eq!(kind(Language::Rust, text, "str"), Some(Type));
        assert_eq!(
            kind(Language::Rust, text, "r#\"raw \" here\"#"),
            Some(String)
        );
        // `!=` is not a macro; a range is two numbers.
        assert_eq!(kind_at(Language::Rust, "a != b", "a"), None);
        let tokens = lex(Language::Rust, "0..10");
        assert_eq!(
            tokens,
            [("0".to_owned(), Number), ("10".to_owned(), Number)]
        );
        // Definitions.
        assert_eq!(kind(Language::Rust, "struct point;", "point"), Some(Type));
    }

    #[test]
    fn rust_block_comments_nest_and_run_over_lines() {
        let text = "/* a /* b */ still\nmore */ fn x() {}";
        assert_eq!(kind_at(Language::Rust, text, "still"), Some(Comment));
        assert_eq!(kind_at(Language::Rust, text, "more"), Some(Comment));
        assert_eq!(kind(Language::Rust, text, "fn"), Some(Keyword));
        let (_, state) = tokens(Language::Rust, "/* open", LineState::default());
        assert_ne!(state, LineState::default());
        let (found, state) = tokens(Language::Rust, "closed */ 1", state);
        assert_eq!(found, [(0..9, Comment), (10..11, Number)]);
        assert_eq!(state, LineState::default());
        // Strings run over lines, raw strings too.
        let text = "let s = \"one\ntwo\"; let r = r##\"x\ny\"##; 3";
        assert_eq!(kind_at(Language::Rust, text, "two"), Some(String));
        assert_eq!(kind_at(Language::Rust, text, "y\""), Some(String));
        assert_eq!(kind(Language::Rust, text, "3"), Some(Number));
    }

    #[test]
    fn python() {
        let text = "@dataclass\nclass Point(Base):\n    def __init__(self, x=1.5):  # 注释\n        return f\"{x}\" + r'\\d' + None\n";
        assert_eq!(kind(Language::Python, text, "@dataclass"), Some(Function));
        assert_eq!(kind(Language::Python, text, "class"), Some(Keyword));
        assert_eq!(kind(Language::Python, text, "Point"), Some(Type));
        assert_eq!(kind(Language::Python, text, "Base"), Some(Type));
        assert_eq!(kind(Language::Python, text, "__init__"), Some(Function));
        assert_eq!(kind(Language::Python, text, "1.5"), Some(Number));
        assert_eq!(kind(Language::Python, text, "# 注释"), Some(Comment));
        assert_eq!(kind(Language::Python, text, "f\"{x}\""), Some(String));
        assert_eq!(kind(Language::Python, text, "r'\\d'"), Some(String));
        assert_eq!(kind(Language::Python, text, "None"), Some(Keyword));
        // Triple quotes run over lines; single quotes do not.
        let text = "x = \"\"\"doc\n# not a comment\n\"\"\" + 2\ny = 'open\nz = 3";
        assert_eq!(kind_at(Language::Python, text, "# not"), Some(String));
        assert_eq!(kind(Language::Python, text, "2"), Some(Number));
        assert_eq!(kind(Language::Python, text, "3"), Some(Number));
        // Unless the line break is escaped.
        let text = "y = 'one \\\ntwo' + 4";
        assert_eq!(kind_at(Language::Python, text, "two"), Some(String));
        assert_eq!(kind(Language::Python, text, "4"), Some(Number));
    }

    #[test]
    fn javascript_and_typescript() {
        let text = "export async function load(url) {\n  const s = `a ${url}\nb`; // c\n  return new Map(); /* d */ $el.x\n}";
        assert_eq!(kind(Language::JavaScript, text, "export"), Some(Keyword));
        assert_eq!(kind(Language::JavaScript, text, "load"), Some(Function));
        assert_eq!(kind_at(Language::JavaScript, text, "b`"), Some(String));
        assert_eq!(kind(Language::JavaScript, text, "// c"), Some(Comment));
        assert_eq!(kind(Language::JavaScript, text, "Map"), Some(Type));
        assert_eq!(kind(Language::JavaScript, text, "/* d */"), Some(Comment));
        assert_eq!(kind_at(Language::JavaScript, text, "$el"), None);
        // An unclosed ordinary string ends with its line.
        let text = "let a = 'open\nlet b = 2";
        assert_eq!(kind(Language::JavaScript, text, "let"), Some(Keyword));
        assert_eq!(kind(Language::JavaScript, text, "2"), Some(Number));
        let text = "interface Shape { area: number }\ntype Id = string | null;";
        assert_eq!(kind(Language::TypeScript, text, "interface"), Some(Keyword));
        assert_eq!(kind(Language::TypeScript, text, "Shape"), Some(Type));
        assert_eq!(kind(Language::TypeScript, text, "number"), Some(Type));
        assert_eq!(kind(Language::TypeScript, text, "Id"), Some(Type));
        assert_eq!(kind(Language::TypeScript, text, "null"), Some(Keyword));
        // Not keywords in JavaScript.
        assert_eq!(kind(Language::JavaScript, text, "interface"), None);
    }

    #[test]
    fn json_keys_values_and_comments() {
        let text = "{\n  \"name\": \"a \\\"b\\\"\",\n  \"n\" : -1.5e+3, \"ok\": true, // c\n  \"x\": null\n}";
        assert_eq!(kind(Language::Json, text, "\"name\""), Some(Type));
        assert_eq!(kind(Language::Json, text, "\"n\""), Some(Type));
        assert_eq!(kind(Language::Json, text, "\"a \\\"b\\\"\""), Some(String));
        assert_eq!(kind(Language::Json, text, "1.5e+3"), Some(Number));
        assert_eq!(kind(Language::Json, text, "true"), Some(Keyword));
        assert_eq!(kind(Language::Json, text, "null"), Some(Keyword));
        assert_eq!(kind(Language::Json, text, "// c"), Some(Comment));
    }

    #[test]
    fn shell() {
        let text =
            "#!/bin/bash\nif [ -n \"$HOME\" ]; then echo 'it''s' ${PATH} $1 # c\nfi\nurl=a#b\n";
        assert_eq!(kind(Language::Shell, text, "#!/bin/bash"), Some(Comment));
        assert_eq!(kind(Language::Shell, text, "if"), Some(Keyword));
        assert_eq!(kind(Language::Shell, text, "\"$HOME\""), Some(String));
        assert_eq!(kind(Language::Shell, text, "echo"), Some(Function));
        assert_eq!(kind(Language::Shell, text, "'it'"), Some(String));
        assert_eq!(kind(Language::Shell, text, "${PATH}"), Some(Type));
        assert_eq!(kind(Language::Shell, text, "$1"), Some(Type));
        assert_eq!(kind(Language::Shell, text, "# c"), Some(Comment));
        assert_eq!(kind(Language::Shell, text, "fi"), Some(Keyword));
        // `#` inside a word is not a comment.
        assert_eq!(kind_at(Language::Shell, text, "#b"), None);
        // Quotes run over lines; single quotes have no escapes.
        let text = "echo 'a\\\nb' \"c\nd\" 5";
        assert_eq!(kind_at(Language::Shell, text, "b'"), Some(String));
        assert_eq!(kind_at(Language::Shell, text, "d\""), Some(String));
        assert_eq!(kind(Language::Shell, text, "5"), Some(Number));
    }

    #[test]
    fn c_and_cpp() {
        let text = "#include <stdio.h>\n#define N 10\nstruct node { int v; };\nint main(void) { char c = '\\''; /* a\n b */ return printf(\"%d\\n\", N); }";
        assert_eq!(kind(Language::C, text, "#include"), Some(Keyword));
        assert_eq!(kind(Language::C, text, "<stdio.h>"), Some(String));
        assert_eq!(kind(Language::C, text, "#define"), Some(Keyword));
        assert_eq!(kind(Language::C, text, "10"), Some(Number));
        assert_eq!(kind(Language::C, text, "node"), Some(Type));
        assert_eq!(kind(Language::C, text, "int"), Some(Type));
        assert_eq!(kind(Language::C, text, "main"), Some(Function));
        assert_eq!(kind(Language::C, text, "'\\''"), Some(String));
        assert_eq!(kind_at(Language::C, text, " b */"), Some(Comment));
        assert_eq!(kind(Language::C, text, "return"), Some(Keyword));
        assert_eq!(kind(Language::C, text, "\"%d\\n\""), Some(String));
        // Capitalised names are not types in C.
        assert_eq!(kind(Language::C, "Foo x;", "Foo"), None);
        let text = "template <typename T> class Box { public: T* get() const noexcept; };";
        assert_eq!(kind(Language::Cpp, text, "template"), Some(Keyword));
        assert_eq!(kind(Language::Cpp, text, "Box"), Some(Type));
        assert_eq!(kind(Language::Cpp, text, "get"), Some(Function));
        assert_eq!(kind(Language::Cpp, text, "noexcept"), Some(Keyword));
    }

    #[test]
    fn java_kotlin_swift_and_csharp() {
        let text = "@Override\npublic String name() { return \"\"\"\n  text\n  \"\"\"; }";
        assert_eq!(kind(Language::Java, text, "@Override"), Some(Function));
        assert_eq!(kind(Language::Java, text, "public"), Some(Keyword));
        assert_eq!(kind(Language::Java, text, "String"), Some(Type));
        assert_eq!(kind(Language::Java, text, "name"), Some(Function));
        assert_eq!(kind_at(Language::Java, text, "text"), Some(String));
        let text = "data class User(val id: Int)\nfun greet() = println(\"hi\")";
        assert_eq!(kind(Language::Kotlin, text, "data"), Some(Keyword));
        assert_eq!(kind(Language::Kotlin, text, "User"), Some(Type));
        assert_eq!(kind(Language::Kotlin, text, "Int"), Some(Type));
        assert_eq!(kind(Language::Kotlin, text, "greet"), Some(Function));
        let text =
            "struct Item: Codable { let id: UUID }\nfunc load() async throws -> [Item] { nil }";
        assert_eq!(kind(Language::Swift, text, "Item"), Some(Type));
        assert_eq!(kind(Language::Swift, text, "load"), Some(Function));
        assert_eq!(kind(Language::Swift, text, "throws"), Some(Keyword));
        assert_eq!(kind(Language::Swift, text, "nil"), Some(Keyword));
        let text = "#region x\npublic record Person(string Name);";
        assert_eq!(kind(Language::CSharp, text, "#region"), Some(Keyword));
        assert_eq!(kind(Language::CSharp, text, "Person"), Some(Type));
        assert_eq!(kind(Language::CSharp, text, "string"), Some(Type));
    }

    #[test]
    fn go_ruby_lua_and_sql() {
        let text =
            "func (s *Server) Run(port int) error {\n\tmsg := `raw \\n\nstill`\n\treturn nil\n}";
        assert_eq!(kind(Language::Go, text, "func"), Some(Keyword));
        assert_eq!(kind(Language::Go, text, "Run"), Some(Function));
        assert_eq!(kind(Language::Go, text, "error"), Some(Type));
        assert_eq!(kind_at(Language::Go, text, "still"), Some(String));
        assert_eq!(kind(Language::Go, text, "nil"), Some(Keyword));
        let text = "class Dog < Animal\n  def bark # woof\n    puts \"hi\"\n  end\nend";
        assert_eq!(kind(Language::Ruby, text, "Dog"), Some(Type));
        assert_eq!(kind(Language::Ruby, text, "bark"), Some(Function));
        assert_eq!(kind(Language::Ruby, text, "# woof"), Some(Comment));
        assert_eq!(kind(Language::Ruby, text, "end"), Some(Keyword));
        let text = "local t = [[long\nstring]] --[==[ block\ncomment ]==] -- line\nreturn 1";
        assert_eq!(kind(Language::Lua, text, "local"), Some(Keyword));
        assert_eq!(kind_at(Language::Lua, text, "string]]"), Some(String));
        assert_eq!(kind_at(Language::Lua, text, "comment"), Some(Comment));
        assert_eq!(kind(Language::Lua, text, "-- line"), Some(Comment));
        assert_eq!(kind(Language::Lua, text, "1"), Some(Number));
        let text =
            "SELECT name, COUNT(*) FROM users -- c\nwhere id = 'it''s' /* x\n y */ AND age > 18;";
        assert_eq!(kind(Language::Sql, text, "SELECT"), Some(Keyword));
        assert_eq!(kind(Language::Sql, text, "where"), Some(Keyword));
        assert_eq!(kind(Language::Sql, text, "COUNT"), Some(Function));
        assert_eq!(kind(Language::Sql, text, "-- c"), Some(Comment));
        assert_eq!(kind_at(Language::Sql, text, " y */"), Some(Comment));
        assert_eq!(kind(Language::Sql, text, "18"), Some(Number));
        assert_eq!(kind(Language::Sql, "x VARCHAR(10)", "VARCHAR"), Some(Type));
    }

    #[test]
    fn yaml_and_toml() {
        let text = "---\n# 注释\nname: \"Aster\" # c\nlist:\n  - key: 1.5\n  - plain text\nok: true\nurl: http://a#b\n";
        assert_eq!(kind(Language::Yaml, text, "---"), Some(Keyword));
        assert_eq!(kind(Language::Yaml, text, "# 注释"), Some(Comment));
        assert_eq!(kind(Language::Yaml, text, "name"), Some(Type));
        assert_eq!(kind(Language::Yaml, text, "\"Aster\""), Some(String));
        assert_eq!(kind(Language::Yaml, text, "# c"), Some(Comment));
        assert_eq!(kind(Language::Yaml, text, "key"), Some(Type));
        assert_eq!(kind(Language::Yaml, text, "1.5"), Some(Number));
        assert_eq!(kind_at(Language::Yaml, text, "plain"), None);
        assert_eq!(kind(Language::Yaml, text, "true"), Some(Keyword));
        assert_eq!(kind_at(Language::Yaml, text, "#b"), None);
        let text = "[package]\nname = \"aster\" # c\n[[bin]]\n\"quoted key\" = 'lit\\'\ndesc = \"\"\"\nmulti # no\n\"\"\"\nn = 3\n";
        assert_eq!(kind(Language::Toml, text, "package"), Some(Type));
        assert_eq!(kind(Language::Toml, text, "name"), Some(Type));
        assert_eq!(kind(Language::Toml, text, "\"aster\""), Some(String));
        assert_eq!(kind(Language::Toml, text, "# c"), Some(Comment));
        assert_eq!(kind(Language::Toml, text, "bin"), Some(Type));
        assert_eq!(kind(Language::Toml, text, "\"quoted key\""), Some(Type));
        assert_eq!(kind(Language::Toml, text, "'lit\\'"), Some(String));
        assert_eq!(kind_at(Language::Toml, text, "multi"), Some(String));
        assert_eq!(kind(Language::Toml, text, "n"), Some(Type));
        assert_eq!(kind(Language::Toml, text, "3"), Some(Number));
    }

    #[test]
    fn html_and_xml() {
        let text = "<!DOCTYPE html>\n<a href=\"/x\" data-id=7\n   hidden>Tom &amp; Jerry</a> <!-- c\nstill -->\n<br/>";
        assert_eq!(kind(Language::Html, text, "DOCTYPE"), Some(Keyword));
        assert_eq!(kind(Language::Html, text, "a"), Some(Keyword));
        assert_eq!(kind(Language::Html, text, "href"), Some(Type));
        assert_eq!(kind(Language::Html, text, "\"/x\""), Some(String));
        assert_eq!(kind(Language::Html, text, "data-id"), Some(Type));
        assert_eq!(kind(Language::Html, text, "7"), Some(String));
        // The tag goes on over the line break.
        assert_eq!(kind(Language::Html, text, "hidden"), Some(Type));
        assert_eq!(kind_at(Language::Html, text, "Tom"), None);
        assert_eq!(kind(Language::Html, text, "&amp;"), Some(Number));
        assert_eq!(kind_at(Language::Html, text, "still"), Some(Comment));
        assert_eq!(kind(Language::Html, text, "br"), Some(Keyword));
        assert_eq!(kind_at(Language::Html, "a < b", "<"), None);
    }

    #[test]
    fn css_and_scss() {
        let text = "@media (max-width: 600px) {\n  a:hover, .x { color: #fff; margin:0 -2px; background: url(a.png) !important }\n}\n/* c\nd */";
        assert_eq!(kind(Language::Css, text, "@media"), Some(Keyword));
        assert_eq!(kind(Language::Css, text, "max-width"), Some(Type));
        assert_eq!(kind(Language::Css, text, "600px"), Some(Number));
        assert_eq!(kind_at(Language::Css, text, "a:hover"), None);
        assert_eq!(kind_at(Language::Css, text, "hover"), None);
        assert_eq!(kind(Language::Css, text, "color"), Some(Type));
        assert_eq!(kind(Language::Css, text, "#fff"), Some(Number));
        assert_eq!(kind(Language::Css, text, "margin"), Some(Type));
        assert_eq!(kind(Language::Css, text, "-2px"), Some(Number));
        assert_eq!(kind(Language::Css, text, "url"), Some(Function));
        assert_eq!(kind(Language::Css, text, "!important"), Some(Keyword));
        assert_eq!(kind_at(Language::Css, text, "d */"), Some(Comment));
        // An id selector is not a colour.
        assert_eq!(kind_at(Language::Css, "#header { }", "#header"), None);
        let text = "$gap: 4px; // c\n.a { b: url(http://x) }";
        assert_eq!(kind(Language::Scss, text, "$gap"), Some(Type));
        assert_eq!(kind(Language::Scss, text, "// c"), Some(Comment));
        assert_eq!(kind_at(Language::Scss, text, "//x"), None);
    }

    #[test]
    fn state_is_carried_line_by_line() {
        // Lexing line by line with the carried state gives what lexing the
        // whole text does.
        let text = "/* a\nb */ x \"s\ns\" 'c' r#\"\n\"# y";
        let mut state = LineState::default();
        let mut at = 0;
        let mut joined = Vec::new();
        for line in text.split_inclusive('\n') {
            let (found, after) = tokens(Language::Rust, line, state);
            joined.extend(
                found
                    .into_iter()
                    .map(|(r, t)| (at + r.start..at + r.end, t)),
            );
            state = after;
            at += line.len();
        }
        assert_eq!(
            joined,
            highlight(Language::Rust, text, LineState::default()).0
        );
        // Carriage returns are not part of a comment.
        let (found, _) = tokens(Language::Python, "# c\r\n", LineState::default());
        assert_eq!(found, [(0..3, Comment)]);
    }

    #[test]
    fn long_lines_are_coloured_only_at_the_start() {
        for language in Language::ALL {
            let mut line = "fn x = \"s\" 1 # // /* <a b='c'> 长".repeat(5000);
            line.push_str(" true");
            let (found, _) = tokens(language, &line, LineState::default());
            check(&line, &found);
            assert!(found.iter().all(|(range, _)| range.end <= LINE_LIMIT));
            // Odd text does not upset any of them.
            for odd in [
                "\"", "'", "\\", "r#", "r#\"", "'\\", "${", "<", "</", "<!--", "&", "[==", "--[",
                "@", "#", "0x", "1e+", ".5", "/*/", "\"\"\"\"", "中'文", "'中'", "a: 'b", "[a",
                "= 1", "x =",
            ] {
                let mut state = LineState::default();
                for _ in 0..3 {
                    let (found, after) = tokens(language, odd, state);
                    check(odd, &found);
                    state = after;
                }
            }
        }
    }
}
