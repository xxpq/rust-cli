//! Markdown 的抽象语法树：块（结构）与行内（文字与样式）两层，外加表格列的对齐。
//! 移植自 packages/tui/tui/src/zcode/tui/markdown/Ast.scala（feat/tui a2bf878b34）。
//!
//! 纯数据，不依赖视图层：解析器在 TUI 之下，模型可以持有解析好的文档而不牵出渲染。
//! 语法是有意取的 GFM 子集（认得什么见 `block_parser` / `inline_parser`）；认不出的构造退化成字面文字，不报错。

/// GFM 表格的列对齐，取自分隔行的冒号。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Align {
    /// 没写冒号。
    None,
    Left,
    Center,
    Right,
}

/// 行内节点：一个块里的一段文字或一段带样式的范围。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    /// 字面文字（转义已解开，实体按原样保留）。
    Text(String),
    Emph(Vec<Inline>),
    Strong(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    /// 行内代码：原样，不再解析。
    Code(String),
    /// 行内公式（`$…$`、`\(…\)`）排成的一行 Unicode（docs/specs/rust-cli/tui-kit.md §3.5）：解析时已排好；排不成一行或
    /// 不支持的公式不是这个节点，是原文的 `Text`。`source` 是原文（连同定界符），复制时用它（§3.5「复制」）。
    Math {
        text: String,
        source: String,
    },
    /// `[content](dest "title")`。
    Link {
        content: Vec<Inline>,
        dest: String,
        title: Option<String>,
    },
    /// `![alt](dest "title")`。`alt` 是纯文字（图片里没有行内目标）。
    Image {
        alt: String,
        dest: String,
        title: Option<String>,
    },
    /// `<uri>` / `<email>` 与 GFM 的裸链接。
    Autolink {
        uri: String,
        is_email: bool,
    },
    /// 行尾两个空格或反斜杠造成的硬换行。
    HardBreak,
    /// 段落里的换行（渲染成空格 / 软折行）。
    SoftBreak,
}

/// 块级节点：文档的一个结构单元。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// ATX（`# h`）或 setext（`h\n===`）标题；`level` 在 1..=6。
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    /// 围栏或缩进代码块。`lang` 是信息串的第一个词，`info` 是整个信息串；`code` 保留原来的换行。`closed`：围栏闭合了
    /// （缩进代码总是闭合的）；没闭合的围栏一直到文字的末尾，流式回答里 mermaid 围栏闭合之前不画（tui-kit.md §3.5）。
    Code {
        info: Option<String>,
        lang: Option<String>,
        code: String,
        fenced: bool,
        closed: bool,
    },
    /// 独立的公式（行首的 `$$` / `\[` 到 `$$` / `\]`，docs/specs/rust-cli/tui-kit.md §3.5）。`source` 是原文（含定界符，
    /// 去掉缩进，行之间 `\n`）；`rows` 是排好的几行（闭合且支持时有）；`closed`：闭合的定界符到了。
    Math {
        source: String,
        rows: Option<Vec<String>>,
        closed: bool,
    },
    Quote(Vec<Block>),
    /// 有序或无序列表（Scala 的 `ListBlock`）。`start` 是第一个序号（只对有序列表有意义，无序为 1）；
    /// `tight` 决定条目之间是否按段落留空。
    List {
        ordered: bool,
        start: u32,
        tight: bool,
        items: Vec<ListItem>,
    },
    /// `header` 与每一行都是按列的单元格，列数与 `align` 相同（解析时已补齐或截断）。
    Table {
        align: Vec<Align>,
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    ThematicBreak,
}

/// [`Block::List`] 的一个条目。GFM 任务项（`- [ ]` / `- [x]`）的 `task` 是 `Some(勾选与否)`，其余为 `None`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListItem {
    pub blocks: Vec<Block>,
    pub task: Option<bool>,
}
