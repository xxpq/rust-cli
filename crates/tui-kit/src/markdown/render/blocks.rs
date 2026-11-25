//! 一个块 → Element：段落、标题、引用、横线、代码（面板或细栏）、列表、表格，以及流式回答末尾那个开放的块。
//! 移植自 packages/tui/tui/src/zcode/tui/markdown/render/MarkdownRender.scala（feat/tui a2bf878b34）。
use std::sync::Arc;

use crate::app::element::{
    ColumnAlign, Element, Wrap, beside, by_width, caret_at, code_panel, empty, hr, map_lines,
    single_line, stack, table, wrap_styled,
};
use crate::markdown::ast::{Align, Block, Inline, ListItem};
use crate::markdown::syntax::{self, Kind};
use crate::render::{Color, Span, Style, attr, width};

use super::inline::{Run, cut_hidden};
use super::{HIDDEN, INDENT, Md, Tail, piped, width_of};

/// 一块在哪里：第一行与之后各行的前缀、在几层列表里、文字的墨（引用里 fg2，做完的任务 fg3）。
#[derive(Clone, Debug)]
pub(super) struct At {
    pub(super) first: Vec<Span>,
    pub(super) cont: Vec<Span>,
    pub(super) depth: usize,
    pub(super) ink: Style,
}

impl At {
    /// 顶层：内容从第 3 列开始。
    pub(super) fn top() -> At {
        At {
            first: vec![Span::plain(INDENT)],
            cont: vec![Span::plain(INDENT)],
            depth: 0,
            ink: Style::DEFAULT,
        }
    }

    /// 第一块之后的块在同一个地方：每一行都是接续。
    fn rest(&self) -> At {
        At {
            first: self.cont.clone(),
            ..self.clone()
        }
    }
}

fn space() -> Span {
    Span::plain(" ")
}

fn joined(a: &[Span], b: impl IntoIterator<Item = Span>) -> Vec<Span> {
    a.iter().cloned().chain(b).collect()
}

const BULLETS: [&str; 3] = ["•", "◦", "▪"];

/// 画成文字图的代码块的信息串第一个词（tui-kit.md §3.5）。
pub(super) const MERMAID: &str = "mermaid";

/// 代码面板与 plain 的细栏在前缀之后占的列：面板两边各一格留白，细栏是 `▏` 与一个空格。图要画在剩下的宽度里。
const CODE_GUTTER: usize = 2;

/// mermaid 代码块的围栏：kit 的块不记原来的围栏字，复制的原文总写三个反引号（tui-kit.md §3.5「复制」待确认 (h)）。
const FENCE: &str = "```";

/// mermaid 代码块的原文：围栏加信息串、代码，闭合时再加闭合的围栏（tui-kit.md §3.5「复制」）。
fn fenced(info: Option<&str>, code: &str, closed: bool) -> String {
    let close = if closed {
        format!("\n{FENCE}")
    } else {
        String::new()
    };
    format!("{FENCE}{}\n{code}{close}", info.unwrap_or(MERMAID))
}

/// 复制时整块换成原文 `text`（用户决定 2026-11-12，tui-kit.md §3.5「复制」；以前复制出的是屏幕上的方框图与 Unicode
/// 公式）：块铺出的每一段（连同面板的边、标签、细栏与画不出时的原因）都带着它，复制只写一次。原文每一行前面接容器的
/// 前缀（第一行 `at.first`，其余 `at.cont` 的字），复制出的原文仍在引用与列表里。
fn copied_as(e: Element, text: &str, at: &At) -> Element {
    let lead = |spans: &[Span]| spans.iter().map(|sp| sp.text.as_ref()).collect::<String>();
    let (first, cont) = (lead(&at.first), lead(&at.cont));
    let lines: Vec<String> = text
        .split('\n')
        .enumerate()
        .map(|(i, line)| format!("{}{line}", if i == 0 { &first } else { &cont }))
        .collect();
    let copy: Arc<str> = Arc::from(lines.join("\n"));
    map_lines(e, move |mut lines| {
        for sp in lines.iter_mut().flat_map(|l| l.spans.iter_mut()) {
            sp.decor.copy = Some(Arc::clone(&copy));
        }
        lines
    })
}

/// 是画成文字图的代码块（信息串第一个词是 `mermaid`）。
pub(super) fn is_mermaid(lang: Option<&str>) -> bool {
    lang == Some(MERMAID)
}

impl Md {
    fn quote_rail(&self) -> [Span; 2] {
        [Span::new("│", self.theme.line), space()]
    }

    fn code_rail(&self) -> [Span; 2] {
        [Span::new("▏", self.theme.line), space()]
    }

    /// 每块一个元素；第一块从 `at` 开始，其余的接续它。
    pub(super) fn render_list(&self, blocks: &[Block], at: &At) -> Vec<Element> {
        blocks
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if i == 0 {
                    self.render_block(b, at)
                } else {
                    self.render_block(b, &at.rest())
                }
            })
            .collect()
    }

    pub(super) fn render_block(&self, b: &Block, at: &At) -> Element {
        match b {
            Block::Paragraph(content) => {
                let runs = self.paragraph_runs(content, at.ink);
                wrap_styled(
                    at.first.clone(),
                    at.cont.clone(),
                    self.spans(runs, true),
                    Wrap::Word,
                )
            }
            Block::Heading { level, content } => {
                let runs = self.inline_runs(content, at.ink.merge(self.heading_style(*level)));
                wrap_styled(
                    at.first.clone(),
                    at.cont.clone(),
                    self.spans(runs, true),
                    Wrap::Word,
                )
            }
            Block::Code {
                info,
                lang,
                code,
                closed: true,
                ..
            } if is_mermaid(lang.as_deref()) => self.render_mermaid(info.as_deref(), code, at),
            // 没闭合的 mermaid 围栏是普通的代码块（源码）；复制时同样是原文（没有闭合的围栏行）。
            Block::Code {
                info,
                lang,
                code,
                closed: false,
                ..
            } if is_mermaid(lang.as_deref()) => copied_as(
                self.render_code(lang.as_deref(), code, at, &[], false),
                &fenced(info.as_deref(), code, false),
                at,
            ),
            Block::Code { lang, code, .. } => {
                self.render_code(lang.as_deref(), code, at, &[], false)
            }
            Block::Math { source, rows, .. } => {
                copied_as(self.render_math(source, rows.as_deref(), at), source, at)
            }
            Block::Quote(inner) => {
                let elems = self.render_list(inner, &self.quoted(at));
                self.join_blocks(inner.iter().zip(elems).collect(), true)
            }
            // 从内容缩进画到行宽末端的一条细线。
            Block::ThematicBreak => beside(
                single_line(at.first.clone(), false),
                width_of(&at.first),
                hr('─', Color::Default).style(self.theme.line),
            ),
            Block::List { .. } => self.render_list_block(b, at, None),
            Block::Table { .. } => self.render_table(b, at),
        }
    }

    /// 流式回答末尾的块：[`HIDDEN`] 之后的字画成空白，笔尖在下一个词要去的地方。列表与引用把它传给自己的最后一块；
    /// 开着的代码块写到播放头为止、不画下边；还在到达的表格不画（列宽取决于每一行），可能是表头的段落也不画：
    /// 笔尖停在它的位置。
    pub(super) fn render_open(&self, b: &Block, at: &At, tail: &Tail) -> Element {
        match b {
            Block::Paragraph(content) if piped(content) && !tail.ended => {
                // 以前表头在 `|---|` 行到达前是段落：先按原样打出 `| A | B |`，分隔行一到又整行消失。
                // 第一行以 `|` 开头的段落还在到达时先不画，结束时还不是表格就照常画成段落（§4.2「流式节奏」的「表格」）。
                self.resting_nib(at, tail)
            }
            Block::Paragraph(content) => self.prose(at, self.paragraph_runs(content, at.ink), tail),
            Block::Heading { level, content } => {
                let runs = self.inline_runs(content, at.ink.merge(self.heading_style(*level)));
                self.prose(at, runs, tail)
            }
            Block::List { .. } => self.render_list_block(b, at, Some(tail)),
            Block::Quote(inner) => match inner.split_last() {
                Some((last, init)) => {
                    let q = self.quoted(at);
                    let mut elems = self.render_list(init, &q);
                    let last_at = if init.is_empty() { q } else { q.rest() };
                    elems.push(self.render_open(last, &last_at, tail));
                    self.join_blocks(inner.iter().zip(elems).collect(), true)
                }
                None => self.render_block(b, at),
            },
            // 用户决定 2026-11-12（tui-kit.md §3.5「流式」）：没闭合的公式块与 mermaid 围栏先流式显示原文，闭合的那一帧
            // 换成公式或图。第一版把它们藏起来、只留笔尖，等闭合时整块出现。公式块的原文照段落的样子打（代理写的字、
            // 笔尖骑在末尾）；没闭合的 mermaid 围栏走下面普通的代码块。
            Block::Math {
                closed: false,
                source,
                ..
            } => self.prose(at, vec![Run::new(source.clone(), at.ink)], tail),
            Block::Code {
                info,
                lang,
                code,
                closed: true,
                ..
            } if is_mermaid(lang.as_deref()) => self.render_mermaid(info.as_deref(), code, at),
            Block::Code { lang, code, .. } => {
                let shown = code.split(HIDDEN).next().unwrap_or("");
                self.render_code(lang.as_deref(), shown, at, &tail.nib, !tail.ended)
            }
            Block::Table { .. } => {
                if tail.ended {
                    self.render_table(b, at)
                } else {
                    self.resting_nib(at, tail)
                }
            }
            other => self.render_block(other, at),
        }
    }

    /// 不画的块的位置上只有笔尖（没有笔尖就什么都不画）。
    fn resting_nib(&self, at: &At, tail: &Tail) -> Element {
        if tail.nib.is_empty() {
            return empty();
        }
        single_line(joined(&at.first, tail.nib.iter().cloned()), false)
    }

    fn prose(&self, at: &At, runs: Vec<Run>, tail: &Tail) -> Element {
        let (shown, hidden) = cut_hidden(runs);
        let mut spans = self.spans(shown, true);
        if hidden > 0 {
            spans.push(Span::plain("\u{a0}".repeat(hidden)));
        }
        let body = wrap_styled(at.first.clone(), at.cont.clone(), spans, Wrap::Word);
        if tail.nib.is_empty() {
            return body;
        }
        caret_at(body, tail.nib.clone())
    }

    /// h1 brand 粗体，h2 粗体，h3 及以下 fg2 粗体；标题下没有横线。
    fn heading_style(&self, level: u8) -> Style {
        match level {
            1 => Style::new(self.theme.brand, Color::Default, attr::BOLD),
            2 => Style::BOLD,
            _ => self.theme.fg2.with_attr(attr::BOLD),
        }
    }

    /// 引用里：line 色的细栏，文字 fg2。
    fn quoted(&self, at: &At) -> At {
        At {
            first: joined(&at.first, self.quote_rail()),
            cont: joined(&at.cont, self.quote_rail()),
            depth: at.depth,
            ink: self.theme.fg2,
        }
    }

    /* ---- 公式与文字图（tui-kit.md §3.5） ---- */

    /// 独立的公式：排好的几行每行都放得下时从正文列起左对齐画出（正文的墨，不折行）；放不下、不支持或没闭合时是原文
    /// （按词折行）。
    fn render_math(&self, source: &str, rows: Option<&[String]>, at: &At) -> Element {
        let verbatim = wrap_styled(
            at.first.clone(),
            at.cont.clone(),
            vec![Span::new(source.to_owned(), at.ink)],
            Wrap::Word,
        );
        let Some(rows) = rows else {
            return verbatim;
        };
        let rows = rows.to_vec();
        let (first, cont, ink) = (at.first.clone(), at.cont.clone(), at.ink);
        by_width(move |w| {
            let room = w.saturating_sub(width_of(&first).max(width_of(&cont)));
            if rows.iter().any(|row| width::string_width(row) > room) {
                return verbatim.clone();
            }
            stack(rows.iter().enumerate().map(|(i, row)| {
                let lead = if i == 0 { &first } else { &cont };
                single_line(joined(lead, [Span::new(row.clone(), ink)]), false)
            }))
        })
    }

    /// 闭合的 mermaid 代码块：在代码块里的宽度上画得出时画成图，否则是 fg3 的一句原因、下面是普通的代码块（源码）。图与
    /// 代码块一样：rich 在面板上、`mermaid` 在第一行右端；plain 是标签一行、细栏后面的图。节点是语法的 type 墨（plain
    /// 没有颜色），连线 fg3，标签是正文；图不标成代理写的字（整块出现，不逐字落定）。
    fn render_mermaid(&self, info: Option<&str>, code: &str, at: &At) -> Element {
        let drawn = self.drawn_mermaid(code, at);
        copied_as(drawn, &fenced(info, code, true), at)
    }

    /// [`Md::render_mermaid`] 画出的样子（图，或原因与代码块）。
    fn drawn_mermaid(&self, code: &str, at: &At) -> Element {
        // 用户决定 2026-11-12（tui-kit.md §3.5「Mermaid」）：画不出时先说为什么，修改前只有代码块，看不出是不支持还是
        // 放不下。原因占第一行，代码块接在它下面（从接续行开始）。
        let source = self.render_code(Some(MERMAID), code, &at.rest(), &[], false);
        let (code, at, theme, words) = (code.to_owned(), at.clone(), self.theme, self.words);
        let rail = self.code_rail();
        by_width(move |w| {
            let lead = width_of(&at.first).max(width_of(&at.cont));
            let room = w.saturating_sub(lead + CODE_GUTTER);
            let diagram = match rust_cli_vendor_mermaid::render_spans(&code, room) {
                Ok(diagram) => diagram,
                Err(error) => {
                    let why = match error {
                        rust_cli_vendor_mermaid::RenderError::Unsupported => {
                            words.mermaid_unsupported
                        }
                        rust_cli_vendor_mermaid::RenderError::TooWide => words.mermaid_too_wide,
                        rust_cli_vendor_mermaid::RenderError::Limit => words.mermaid_limit,
                    };
                    let said = wrap_styled(
                        at.first.clone(),
                        at.cont.clone(),
                        vec![Span::new(why, theme.fg3)],
                        Wrap::Word,
                    );
                    return stack([said, source.clone()]);
                }
            };
            let node = if theme.rich {
                Style::fg(theme.syntax.ty)
            } else {
                Style::DEFAULT
            };
            let lines: Vec<Vec<Span>> = diagram
                .into_iter()
                .map(|line| {
                    line.into_iter()
                        .map(|sp| {
                            let style = match sp.role {
                                rust_cli_vendor_mermaid::Role::Node => node,
                                rust_cli_vendor_mermaid::Role::Edge => theme.fg3,
                                rust_cli_vendor_mermaid::Role::Text => Style::DEFAULT,
                            };
                            Span::new(sp.text, style)
                        })
                        .collect()
                })
                .collect();
            match theme.panel {
                Some(panel) => code_panel(
                    at.first.clone(),
                    lines,
                    panel,
                    vec![Span::new(MERMAID, theme.fg3)],
                    false,
                ),
                None => {
                    let tag =
                        single_line(joined(&at.first, [Span::new(MERMAID, theme.fg3)]), false);
                    let railed = joined(&at.cont, rail.clone());
                    let body = lines
                        .into_iter()
                        .map(|l| single_line(joined(&railed, l), false));
                    stack(std::iter::once(tag).chain(body))
                }
            }
        })
    }

    /* ---- 代码块 ---- */

    /// 一个代码块；正在写（`open`）时笔尖骑在代码末尾，还没有下边（§4.2「流式节奏」的「代码块」）。
    fn render_code(
        &self,
        lang: Option<&str>,
        code: &str,
        at: &At,
        nib: &[Span],
        open: bool,
    ) -> Element {
        let mut lines = self.highlighted(lang, code);
        if let Some(last) = lines.last_mut() {
            last.extend_from_slice(nib);
        }
        match self.theme.panel {
            // 内容缩进处的面板；语言在它第一行的右端。
            Some(panel) => {
                let label = lang
                    .map(|l| vec![Span::new(l.to_owned(), self.theme.fg3)])
                    .unwrap_or_default();
                code_panel(at.first.clone(), lines, panel, label, open)
            }
            None => {
                // 第一个铺出的行从 `at.first` 开始，其余接续。有语言标签时它占第一行，代码往下推。
                let body_first = if lang.is_some() { &at.cont } else { &at.first };
                let tag = lang.map(|l| {
                    single_line(
                        joined(&at.first, [Span::new(l.to_owned(), self.theme.fg3)]),
                        false,
                    )
                });
                let body = lines.into_iter().enumerate().map(|(i, l)| {
                    let first = if i == 0 { body_first } else { &at.cont };
                    // 代码是原样的：按字符折（从不在词之间断开、不合并空格）。
                    wrap_styled(
                        joined(first, self.code_rail()),
                        joined(&at.cont, self.code_rail()),
                        l,
                        Wrap::Char,
                    )
                });
                stack(tag.into_iter().chain(body))
            }
        }
    }

    /// 代码的各行：rich 主题且 [`syntax`] 认得这门语言时用安静的语法墨；否则原样。
    fn highlighted(&self, lang: Option<&str>, code: &str) -> Vec<Vec<Span>> {
        let lexed = lang
            .filter(|_| self.theme.rich)
            .and_then(|l| syntax::lex(l, code));
        match lexed {
            Some(lines) => lines
                .into_iter()
                .map(|toks| {
                    toks.into_iter()
                        .map(|t| Span::new(t.text.to_owned(), self.own(self.syntax_style(t.kind))))
                        .collect()
                })
                .collect(),
            None => code
                .split('\n')
                .map(|l| vec![Span::new(l.to_owned(), self.own(Style::DEFAULT))])
                .collect(),
        }
    }

    /// 每类记号用主题的语法墨；注释 fg3 斜体，其余是正文。
    fn syntax_style(&self, k: Kind) -> Style {
        let y = &self.theme.syntax;
        match k {
            Kind::Keyword => Style::fg(y.keyword),
            Kind::Str => Style::fg(y.string),
            Kind::Num => Style::fg(y.number),
            Kind::Func => Style::fg(y.function),
            Kind::Type => Style::fg(y.ty),
            Kind::Const => Style::fg(y.constant),
            Kind::Key => Style::fg(y.key),
            Kind::Comment => self.theme.fg3.with_attr(attr::ITALIC),
            Kind::Plain => Style::DEFAULT,
        }
    }

    /* ---- 列表 ---- */

    /// 有 `open` 时，最后一条的最后一块是流式的尾巴（[`Md::render_open`]）。
    fn render_list_block(&self, b: &Block, at: &At, open: Option<&Tail>) -> Element {
        let Block::List {
            ordered,
            start,
            tight,
            items,
        } = b
        else {
            return empty();
        };
        // 有序列表的序号按最宽的那个右对齐。
        let digits = if *ordered {
            (u64::from(*start) + items.len() as u64 - 1)
                .to_string()
                .len()
        } else {
            0
        };
        let item_elems = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let (marker, ink) = self.list_marker(*ordered, *start, idx, item, at, digits);
                let pad = " ".repeat(width_of(&marker));
                let inner = At {
                    first: joined(if idx == 0 { &at.first } else { &at.cont }, marker),
                    cont: joined(&at.cont, [Span::plain(pad)]),
                    depth: at.depth + 1,
                    ink,
                };
                let elems = match open {
                    Some(t) if idx == items.len() - 1 && !item.blocks.is_empty() => {
                        let n = item.blocks.len();
                        let mut elems = self.render_list(&item.blocks[..n - 1], &inner);
                        let last_at = if n == 1 { inner } else { inner.rest() };
                        elems.push(self.render_open(&item.blocks[n - 1], &last_at, t));
                        elems
                    }
                    _ => self.render_list(&item.blocks, &inner),
                };
                self.join_blocks(item.blocks.iter().zip(elems).collect(), !tight)
            })
            .collect();
        self.join_spaced(item_elems, !tight)
    }

    /// 一条的标记与文字的墨。任务的勾代替圆点：做完是 ok 色的 `✓`、文字 fg3，没做是 fg3 的 `□`。
    /// 圆点随深度变（`•` `◦` `▪`）；序号右对齐到 `digits`。
    fn list_marker(
        &self,
        ordered: bool,
        start: u32,
        idx: usize,
        item: &ListItem,
        at: &At,
        digits: usize,
    ) -> (Vec<Span>, Style) {
        match item.task {
            Some(true) => (
                vec![Span::new("✓", Style::fg(self.theme.ok)), space()],
                self.theme.fg3,
            ),
            Some(false) => (vec![Span::new("□", self.theme.fg3), space()], at.ink),
            None => {
                let mark = if ordered {
                    let n = format!("{}.", u64::from(start) + idx as u64);
                    format!("{}{n}", " ".repeat(digits + 1 - n.len()))
                } else {
                    BULLETS[at.depth % BULLETS.len()].to_owned()
                };
                (vec![Span::new(mark, self.theme.fg3), space()], at.ink)
            }
        }
    }

    /* ---- 表格 ---- */

    /// 造一个与宽度无关的 [`Element::Table`]；分列宽与格子折行在布局里做，那里知道宽度。
    fn render_table(&self, b: &Block, at: &At) -> Element {
        let Block::Table {
            align,
            header,
            rows,
        } = b
        else {
            return empty();
        };
        if align.is_empty() {
            return empty();
        }
        // 表格整张出现、整张落定一次，不算进最新的字（§4.2「笔」的「表格」）。
        let cell = |c: &[Inline], st: Style| self.spans(self.inline_runs(c, st), false);
        let header = header.iter().map(|c| cell(c, Style::BOLD)).collect();
        let rows = rows
            .iter()
            .map(|r| r.iter().map(|c| cell(c, Style::DEFAULT)).collect())
            .collect();
        table(
            at.first.clone(),
            at.cont.clone(),
            align.iter().map(|&a| column_align(a)).collect(),
            header,
            rows,
            self.theme.line,
        )
    }
}

fn column_align(a: Align) -> ColumnAlign {
    match a {
        Align::Right => ColumnAlign::Right,
        Align::Center => ColumnAlign::Center,
        Align::None | Align::Left => ColumnAlign::Left,
    }
}
