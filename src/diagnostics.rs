//! 診断（エラー）メッセージ。ソース位置を指す場合はその行を抜き出し、
//! キャレット(^)で該当箇所を下線表示する。
//!
//! 表示例:
//! ```text
//! error[E0101]: 未定義の変数: x
//!   --> examples/bad.lum:2:12
//!   |
//! 2 |     return x;
//!   |            ^
//! ```

use crate::span::{SourceMap, Span};

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: Option<&'static str>,
    pub message: String,
    pub span: Option<Span>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Diagnostic {
            code: None,
            message: message.into(),
            span: None,
        }
    }

    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    pub fn at(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    /// 人間向けに整形した文字列を返す（末尾に改行を含む）。
    /// span の `file` から [`SourceMap`] を引き、正しいファイル名・該当行を表示する。
    pub fn render(&self, sources: &SourceMap) -> String {
        let code = self.code.map(|c| format!("[{}]", c)).unwrap_or_default();
        let mut out = format!("error{}: {}\n", code, self.message);

        let Some(span) = self.span else {
            return out;
        };

        let file = sources.get(span.file);
        let src = file.src.as_str();
        let filename = file.path.as_str();
        let loc = locate(src, span.start);
        let line_text = &src[loc.line_start..loc.line_end];
        let num = loc.line.to_string();
        let gutter = " ".repeat(num.len());

        // span 開始までの文字数（タブ等の見た目は簡略化して1文字幅とする）
        let pad = src[loc.line_start..span.start.min(loc.line_end)]
            .chars()
            .count();
        // 下線の長さ（その行内に収める。最低1）
        let underline = src[span.start.min(loc.line_end)..span.end.min(loc.line_end)]
            .chars()
            .count()
            .max(1);

        out.push_str(&format!("  --> {}:{}:{}\n", filename, loc.line, loc.col));
        out.push_str(&format!("{} |\n", gutter));
        out.push_str(&format!("{} | {}\n", num, line_text));
        out.push_str(&format!(
            "{} | {}{}\n",
            gutter,
            " ".repeat(pad),
            "^".repeat(underline)
        ));
        out
    }
}

struct Loc {
    line: usize,
    col: usize,
    line_start: usize,
    line_end: usize,
}

/// バイトオフセットから、1始まりの行・列と、その行の範囲を求める。
fn locate(src: &str, offset: usize) -> Loc {
    let offset = offset.min(src.len());
    let mut line_start = 0usize;
    let mut line = 1usize;
    for (i, b) in src.bytes().enumerate() {
        if i >= offset {
            break;
        }
        if b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    let line_end = src[line_start..]
        .find('\n')
        .map(|p| line_start + p)
        .unwrap_or(src.len());
    let col = src[line_start..offset].chars().count() + 1;
    Loc {
        line,
        col,
        line_start,
        line_end,
    }
}
