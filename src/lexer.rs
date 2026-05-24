//! 字句解析 (Lexer): ソース文字列をトークン列に変換する。
//! 各トークンはソース上の位置 (Span) を持つ。

use crate::diagnostics::Diagnostic;
use crate::span::{FileId, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // リテラル・識別子
    Int(i64),
    Float(f64),
    Str(String),
    /// 補間文字列 `"a {x} b"`。リテラル片と式ソース片の並び。
    InterpStr(Vec<Segment>),
    Ident(String),
    // キーワード
    Fn,
    Struct,
    Import,
    Pub,
    Let,
    If,
    Else,
    While,
    For,
    In,
    Break,
    Continue,
    Return,
    Print,
    True,
    False,
    Null,
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
    Dot,       // .
    // 演算子
    Assign, // =
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    PlusEq,    // +=
    MinusEq,   // -=
    StarEq,    // *=
    SlashEq,   // /=
    PercentEq, // %=
    EqEq,      // ==
    NotEq,     // !=
    Lt,        // <
    Le,        // <=
    Gt,        // >
    Ge,        // >=
    Bang,      // !
    AmpAmp,    // &&
    PipePipe,  // ||
    Arrow,     // ->
    Amp,       // &  (ビット AND)
    Pipe,      // |  (ビット OR)
    Caret,     // ^  (ビット XOR)
    Tilde,     // ~  (ビット NOT)
    Shl,       // << (左シフト)
    Shr,       // >> (右シフト)
    AmpEq,     // &=
    PipeEq,    // |=
    CaretEq,   // ^=
    ShlEq,     // <<=
    ShrEq,     // >>=
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: Tok,
    pub span: Span,
}

/// 補間文字列の構成片。`"a {x+1} b"` は Lit("a ") / Expr("x+1") / Lit(" b")。
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    /// リテラル片（エスケープ・`{{`/`}}` 解決済み）。
    Lit(String),
    /// `{ ... }` の中の式ソース。`offset` はそのソースのファイル内バイト位置で、
    /// 再字句解析したトークンの span をずらして正しい位置を指すために使う。
    Expr { src: String, offset: usize },
}

/// 位置 i のトークン開始バイトオフセット。終端では src の長さを返す。
fn byte_at(chars: &[(usize, char)], src: &str, i: usize) -> usize {
    if i < chars.len() {
        chars[i].0
    } else {
        src.len()
    }
}

pub fn lex(src: &str, file: FileId) -> Result<Vec<Token>, Diagnostic> {
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

        // 文字列リテラル "..."（エスケープ: \n \t \\ \" \0、補間 {expr}、{{ }} で波括弧）
        if c == '"' {
            let start = off;
            i += 1; // 開きクォートを消費
            let mut cur = String::new();
            let mut segments: Vec<Segment> = Vec::new();
            let mut interpolated = false;
            loop {
                if i >= n || chars[i].1 == '\n' {
                    return Err(Diagnostic::error("文字列が閉じられていません")
                        .with_code("E0004")
                        .at(Span::new(file, start, byte_at(&chars, src, i))));
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
                            .at(Span::new(file, start, src.len())));
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
                            .at(Span::new(
                                file,
                                esc_start,
                                byte_at(&chars, src, i + 1),
                            )));
                        }
                    };
                    cur.push(decoded);
                    i += 1;
                    continue;
                }
                // {{ / }} は波括弧そのもの
                if ch == '{' && i + 1 < n && chars[i + 1].1 == '{' {
                    cur.push('{');
                    i += 2;
                    continue;
                }
                if ch == '}' {
                    if i + 1 < n && chars[i + 1].1 == '}' {
                        cur.push('}');
                        i += 2;
                        continue;
                    }
                    return Err(Diagnostic::error(
                        "文字列中の } は }} と書いてください（補間の閉じは { と対応します）",
                    )
                    .with_code("E0004")
                    .at(Span::new(
                        file,
                        chars[i].0,
                        byte_at(&chars, src, i + 1),
                    )));
                }
                // {expr}: 式ソースを } まで取り込む（v1 は式に " や { を含められない）
                if ch == '{' {
                    interpolated = true;
                    segments.push(Segment::Lit(std::mem::take(&mut cur)));
                    i += 1; // '{' を消費
                    let expr_start = byte_at(&chars, src, i);
                    let mut esrc = String::new();
                    loop {
                        if i >= n || chars[i].1 == '\n' {
                            return Err(Diagnostic::error("補間 {…} が閉じられていません")
                                .with_code("E0004")
                                .at(Span::new(file, start, byte_at(&chars, src, i))));
                        }
                        let ec = chars[i].1;
                        if ec == '}' {
                            i += 1; // '}' を消費
                            break;
                        }
                        // 文字列の終端 " に先に当たった = 補間が閉じていない
                        // （v1 は補間式に " を含められないので、その案内も兼ねる）
                        if ec == '"' {
                            return Err(Diagnostic::error(
                                "補間 {…} が閉じられていません（補間式に \" は使えません。先に変数へ取り出してください）",
                            )
                            .with_code("E0004")
                            .at(Span::new(file, start, byte_at(&chars, src, i))));
                        }
                        // v1 は入れ子の波括弧（map リテラル等）を補間式に書けない
                        if ec == '{' {
                            return Err(Diagnostic::error(
                                "補間式に { は書けません（先に変数へ取り出してください）",
                            )
                            .with_code("E0004")
                            .at(Span::new(
                                file,
                                chars[i].0,
                                byte_at(&chars, src, i + 1),
                            )));
                        }
                        esrc.push(ec);
                        i += 1;
                    }
                    segments.push(Segment::Expr {
                        src: esrc,
                        offset: expr_start,
                    });
                    continue;
                }
                cur.push(ch);
                i += 1;
            }
            let span = Span::new(file, start, byte_at(&chars, src, i));
            if interpolated {
                segments.push(Segment::Lit(cur));
                toks.push(Token {
                    kind: Tok::InterpStr(segments),
                    span,
                });
            } else {
                toks.push(Token {
                    kind: Tok::Str(cur),
                    span,
                });
            }
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
            let span = Span::new(file, off, byte_at(&chars, src, i));
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
            let span = Span::new(file, off, byte_at(&chars, src, i));
            let s: String = chars[begin..i].iter().map(|(_, ch)| *ch).collect();
            let kind = match s.as_str() {
                "fn" => Tok::Fn,
                "struct" => Tok::Struct,
                "import" => Tok::Import,
                "pub" => Tok::Pub,
                "let" => Tok::Let,
                "if" => Tok::If,
                "else" => Tok::Else,
                "while" => Tok::While,
                "for" => Tok::For,
                "in" => Tok::In,
                "break" => Tok::Break,
                "continue" => Tok::Continue,
                "return" => Tok::Return,
                "print" => Tok::Print,
                "true" => Tok::True,
                "false" => Tok::False,
                "null" => Tok::Null,
                _ => Tok::Ident(s),
            };
            toks.push(Token { kind, span });
            continue;
        }

        // 3文字演算子（2文字より先に判定する: `<<=` を `<<` と誤認しないため）
        if i + 2 < n {
            let kind = match (chars[i].1, chars[i + 1].1, chars[i + 2].1) {
                ('<', '<', '=') => Some(Tok::ShlEq),
                ('>', '>', '=') => Some(Tok::ShrEq),
                _ => None,
            };
            if let Some(kind) = kind {
                let span = Span::new(file, off, byte_at(&chars, src, i + 3));
                toks.push(Token { kind, span });
                i += 3;
                continue;
            }
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
                ('+', '=') => Some(Tok::PlusEq),
                ('-', '=') => Some(Tok::MinusEq),
                ('*', '=') => Some(Tok::StarEq),
                ('/', '=') => Some(Tok::SlashEq),
                ('%', '=') => Some(Tok::PercentEq),
                ('<', '<') => Some(Tok::Shl),
                ('>', '>') => Some(Tok::Shr),
                ('&', '=') => Some(Tok::AmpEq),
                ('|', '=') => Some(Tok::PipeEq),
                ('^', '=') => Some(Tok::CaretEq),
                _ => None,
            };
            if let Some(kind) = kind {
                let span = Span::new(file, off, byte_at(&chars, src, i + 2));
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
            '.' => Tok::Dot,
            '=' => Tok::Assign,
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '%' => Tok::Percent,
            '<' => Tok::Lt,
            '>' => Tok::Gt,
            '!' => Tok::Bang,
            '&' => Tok::Amp,
            '|' => Tok::Pipe,
            '^' => Tok::Caret,
            '~' => Tok::Tilde,
            _ => {
                return Err(Diagnostic::error(format!("不正な文字: '{}'", c))
                    .with_code("E0001")
                    .at(Span::new(file, off, off + c.len_utf8())));
            }
        };
        toks.push(Token {
            kind,
            span: Span::new(file, off, off + c.len_utf8()),
        });
        i += 1;
    }

    toks.push(Token {
        kind: Tok::Eof,
        span: Span::new(file, src.len(), src.len()),
    });
    Ok(toks)
}
