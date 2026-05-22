//! 字句解析 (Lexer): ソース文字列をトークン列に変換する。

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // リテラル・識別子
    Int(i64),
    Ident(String),
    // キーワード
    Fn,
    Let,
    If,
    Else,
    While,
    Return,
    Print,
    // 区切り
    LParen,    // (
    RParen,    // )
    LBrace,    // {
    RBrace,    // }
    Comma,     // ,
    Semicolon, // ;
    // 演算子
    Assign, // =
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,  // ==
    NotEq, // !=
    Lt,    // <
    Le,    // <=
    Gt,    // >
    Ge,    // >=
    Eof,
}

pub fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        let c = chars[i];

        // 空白
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // 行コメント "# ..."
        if c == '#' {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // 整数リテラル
        if c.is_ascii_digit() {
            let start = i;
            while i < n && chars[i].is_ascii_digit() {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            let v: i64 = s.parse().map_err(|_| format!("数値が大きすぎます: {}", s))?;
            toks.push(Tok::Int(v));
            continue;
        }

        // 識別子・キーワード
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            let t = match s.as_str() {
                "fn" => Tok::Fn,
                "let" => Tok::Let,
                "if" => Tok::If,
                "else" => Tok::Else,
                "while" => Tok::While,
                "return" => Tok::Return,
                "print" => Tok::Print,
                _ => Tok::Ident(s),
            };
            toks.push(t);
            continue;
        }

        // 2文字演算子
        let two: String = chars[i..(i + 2).min(n)].iter().collect();
        match two.as_str() {
            "==" => {
                toks.push(Tok::EqEq);
                i += 2;
                continue;
            }
            "!=" => {
                toks.push(Tok::NotEq);
                i += 2;
                continue;
            }
            "<=" => {
                toks.push(Tok::Le);
                i += 2;
                continue;
            }
            ">=" => {
                toks.push(Tok::Ge);
                i += 2;
                continue;
            }
            _ => {}
        }

        // 1文字トークン
        let t = match c {
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '{' => Tok::LBrace,
            '}' => Tok::RBrace,
            ',' => Tok::Comma,
            ';' => Tok::Semicolon,
            '=' => Tok::Assign,
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '%' => Tok::Percent,
            '<' => Tok::Lt,
            '>' => Tok::Gt,
            _ => return Err(format!("不正な文字: '{}'", c)),
        };
        toks.push(t);
        i += 1;
    }

    toks.push(Tok::Eof);
    Ok(toks)
}
