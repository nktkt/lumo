//! 字句解析 (Lexer): ソース文字列をトークン列に変換する。
//! 各トークンはソース上の位置 (Span) を持つ。

use crate::diagnostics::Diagnostic;
use crate::span::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // リテラル・識別子
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),
    // キーワード
    Fn,
    Let,
    If,
    Else,
    While,
    For,
    Break,
    Continue,
    Return,
    Print,
    True,
    False,
    // 区切り
    LParen,    // (
    RParen,    // )
    LBrace,    // {
    RBrace,    // }
    LBracket,  // [
    RBracket,  // ]
    Comma,     // ,
    Semicolon, // ;
    Colon,     // :
    // 演算子
    Assign, // =
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,     // ==
    NotEq,    // !=
    Lt,       // <
    Le,       // <=
    Gt,       // >
    Ge,       // >=
    Bang,     // !
    AmpAmp,   // &&
    PipePipe, // ||
    Arrow,    // ->
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: Tok,
    pub span: Span,
}

/// 位置 i のトークン開始バイトオフセット。終端では src の長さを返す。
fn byte_at(chars: &[(usize, char)], src: &str, i: usize) -> usize {
    if i < chars.len() {
        chars[i].0
    } else {
        src.len()
    }
}

pub fn lex(src: &str) -> Result<Vec<Token>, Diagnostic> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let n = chars.len();
    let mut toks = Vec::new();
    let mut i = 0;

    while i < n {
        let (off, c) = chars[i];

        // 空白
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // 行コメント "# ..."
        if c == '#' {
            while i < n && chars[i].1 != '\n' {
                i += 1;
            }
            continue;
        }

        // 文字列リテラル "..."（エスケープ: \n \t \\ \"）
        if c == '"' {
            let start = off;
            i += 1; // 開きクォートを消費
            let mut s = String::new();
            loop {
                if i >= n || chars[i].1 == '\n' {
                    return Err(Diagnostic::error("文字列が閉じられていません")
                        .with_code("E0004")
                        .at(Span::new(start, byte_at(&chars, src, i))));
                }
                let ch = chars[i].1;
                if ch == '"' {
                    i += 1; // 閉じクォートを消費
                    break;
                }
                if ch == '\\' {
                    let esc_start = chars[i].0;
                    i += 1;
                    if i >= n {
                        return Err(Diagnostic::error("文字列が閉じられていません")
                            .with_code("E0004")
                            .at(Span::new(start, src.len())));
                    }
                    let decoded = match chars[i].1 {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '\\' => '\\',
                        '"' => '"',
                        '0' => '\0',
                        other => {
                            return Err(Diagnostic::error(format!(
                                "不正なエスケープ: \\{}",
                                other
                            ))
                            .with_code("E0004")
                            .at(Span::new(esc_start, byte_at(&chars, src, i + 1))));
                        }
                    };
                    s.push(decoded);
                    i += 1;
                } else {
                    s.push(ch);
                    i += 1;
                }
            }
            toks.push(Token {
                kind: Tok::Str(s),
                span: Span::new(start, byte_at(&chars, src, i)),
            });
            continue;
        }

        // 数値リテラル（整数 or 小数）
        if c.is_ascii_digit() {
            let begin = i;
            while i < n && chars[i].1.is_ascii_digit() {
                i += 1;
            }
            // 小数点の後ろに数字が続く場合のみ float とみなす（"1." や "1.x" は不可）
            let is_float = i + 1 < n && chars[i].1 == '.' && chars[i + 1].1.is_ascii_digit();
            if is_float {
                i += 1; // '.' を消費
                while i < n && chars[i].1.is_ascii_digit() {
                    i += 1;
                }
            }
            let span = Span::new(off, byte_at(&chars, src, i));
            let s: String = chars[begin..i].iter().map(|(_, ch)| *ch).collect();
            let kind = if is_float {
                let v: f64 = s.parse().map_err(|_| {
                    Diagnostic::error(format!("不正な小数: {}", s))
                        .with_code("E0003")
                        .at(span)
                })?;
                Tok::Float(v)
            } else {
                let v: i64 = s.parse().map_err(|_| {
                    Diagnostic::error(format!("数値が大きすぎます: {}", s))
                        .with_code("E0003")
                        .at(span)
                })?;
                Tok::Int(v)
            };
            toks.push(Token { kind, span });
            continue;
        }

        // 識別子・キーワード
        if c.is_alphabetic() || c == '_' {
            let begin = i;
            while i < n && (chars[i].1.is_alphanumeric() || chars[i].1 == '_') {
                i += 1;
            }
            let span = Span::new(off, byte_at(&chars, src, i));
            let s: String = chars[begin..i].iter().map(|(_, ch)| *ch).collect();
            let kind = match s.as_str() {
                "fn" => Tok::Fn,
                "let" => Tok::Let,
                "if" => Tok::If,
                "else" => Tok::Else,
                "while" => Tok::While,
                "for" => Tok::For,
                "break" => Tok::Break,
                "continue" => Tok::Continue,
                "return" => Tok::Return,
                "print" => Tok::Print,
                "true" => Tok::True,
                "false" => Tok::False,
                _ => Tok::Ident(s),
            };
            toks.push(Token { kind, span });
            continue;
        }

        // 2文字演算子
        if i + 1 < n {
            let kind = match (chars[i].1, chars[i + 1].1) {
                ('=', '=') => Some(Tok::EqEq),
                ('!', '=') => Some(Tok::NotEq),
                ('<', '=') => Some(Tok::Le),
                ('>', '=') => Some(Tok::Ge),
                ('&', '&') => Some(Tok::AmpAmp),
                ('|', '|') => Some(Tok::PipePipe),
                ('-', '>') => Some(Tok::Arrow),
                _ => None,
            };
            if let Some(kind) = kind {
                let span = Span::new(off, byte_at(&chars, src, i + 2));
                toks.push(Token { kind, span });
                i += 2;
                continue;
            }
        }

        // 1文字トークン
        let kind = match c {
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '{' => Tok::LBrace,
            '}' => Tok::RBrace,
            '[' => Tok::LBracket,
            ']' => Tok::RBracket,
            ',' => Tok::Comma,
            ';' => Tok::Semicolon,
            ':' => Tok::Colon,
            '=' => Tok::Assign,
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '%' => Tok::Percent,
            '<' => Tok::Lt,
            '>' => Tok::Gt,
            '!' => Tok::Bang,
            _ => {
                return Err(Diagnostic::error(format!("不正な文字: '{}'", c))
                    .with_code("E0001")
                    .at(Span::new(off, off + c.len_utf8())));
            }
        };
        toks.push(Token {
            kind,
            span: Span::new(off, off + c.len_utf8()),
        });
        i += 1;
    }

    toks.push(Token {
        kind: Tok::Eof,
        span: Span::new(src.len(), src.len()),
    });
    Ok(toks)
}
