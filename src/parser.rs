//! 構文解析 (Parser): トークン列を再帰下降でASTに変換する。
//! 各ノードにソース位置 (Span) を付け、構文エラーは Diagnostic で位置付きで返す。
//!
//! 演算子の優先順位（低い→高い）:
//!   比較 (== != < <= > >=) < 加減 (+ -) < 乗除 (* / %) < 単項 (-) < 一次式

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::lexer::{lex, Segment, Tok, Token};
use crate::span::{FileId, Span};
use crate::types::{intern, Type};

pub fn parse(tokens: Vec<Token>) -> Result<Program, Diagnostic> {
    // 全トークンは同じファイル由来。空入力でも Eof があるので first() で取れる。
    let file = tokens.first().map(|t| t.span.file).unwrap_or(FileId(0));
    let mut p = Parser {
        toks: tokens,
        pos: 0,
        last_end: 0,
        file,
    };
    p.parse_program()
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// 直前に消費したトークンの終了オフセット（ノードの span 終端に使う）
    last_end: usize,
    /// 解析中のファイル（全ノードの span に付与する）
    file: FileId,
}

impl Parser {
    /// 解析中ファイルの span を作る。
    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.file, start, end)
    }

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].kind
    }

    /// 1つ先のトークン（for-in の `x in` 判定などの 2 トークン先読み用）。
    /// 末尾は Eof なので範囲外でも Eof を返す。
    fn peek2(&self) -> &Tok {
        let i = self.pos + 1;
        let i = if i < self.toks.len() {
            i
        } else {
            self.toks.len() - 1
        };
        &self.toks[i].kind
    }

    fn cur_span(&self) -> Span {
        self.toks[self.pos].span
    }

    fn next(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        self.last_end = t.span.end;
        self.pos += 1;
        t
    }

    fn err(&self, msg: impl Into<String>) -> Diagnostic {
        Diagnostic::error(msg)
            .with_code("E0002")
            .at(self.cur_span())
    }

    fn eat(&mut self, want: &Tok) -> Result<Token, Diagnostic> {
        if self.peek() == want {
            Ok(self.next())
        } else {
            Err(self.err(format!(
                "{:?} を期待しましたが {:?} が来ました",
                want,
                self.peek()
            )))
        }
    }

    fn parse_ident(&mut self) -> Result<(String, Span), Diagnostic> {
        let span = self.cur_span();
        match self.next().kind {
            Tok::Ident(s) => Ok((s, span)),
            other => Err(Diagnostic::error(format!(
                "識別子を期待しましたが {:?} が来ました",
                other
            ))
            .with_code("E0002")
            .at(span)),
        }
    }

    fn parse_program(&mut self) -> Result<Program, Diagnostic> {
        // import 宣言はファイル先頭にまとめて置く。
        let mut imports = Vec::new();
        while self.peek() == &Tok::Import {
            imports.push(self.parse_import()?);
        }

        let mut structs = Vec::new();
        let mut enums = Vec::new();
        let mut funcs = Vec::new();
        while self.peek() != &Tok::Eof {
            // 任意の `pub` 修飾子（他ファイルへ公開する）。
            let is_pub = self.peek() == &Tok::Pub;
            if is_pub {
                self.next();
            }
            match self.peek() {
                Tok::Struct => structs.push(self.parse_struct(is_pub)?),
                Tok::Enum => enums.push(self.parse_enum(is_pub)?),
                Tok::Fn => funcs.push(self.parse_function(is_pub)?),
                // import が後から来たら、先頭に置くよう促す（紛れもない誤りを明確に）
                Tok::Import => {
                    return Err(self.err("import はファイル先頭に置いてください"));
                }
                _ if is_pub => {
                    return Err(self.err("pub の後には fn / struct / enum が必要です"));
                }
                _ => funcs.push(self.parse_function(is_pub)?),
            }
        }
        Ok(Program {
            imports,
            structs,
            enums,
            funcs,
        })
    }

    /// `enum Name { Variant [ "(" type {"," type} ")" ] {"," ...} [","] }`
    fn parse_enum(&mut self, is_pub: bool) -> Result<EnumDef, Diagnostic> {
        let start = self.cur_span().start;
        self.eat(&Tok::Enum)?;
        let (name, _) = self.parse_ident()?;
        self.eat(&Tok::LBrace)?;
        let mut variants = Vec::new();
        if self.peek() != &Tok::RBrace {
            loop {
                let (vname, vname_span) = self.parse_ident()?;
                let mut fields = Vec::new();
                if self.peek() == &Tok::LParen {
                    self.next();
                    if self.peek() != &Tok::RParen {
                        loop {
                            fields.push(self.parse_type()?.0);
                            if self.peek() == &Tok::Comma {
                                self.next();
                            } else {
                                break;
                            }
                        }
                    }
                    self.eat(&Tok::RParen)?;
                }
                variants.push(EnumVariant {
                    name: vname,
                    fields,
                    span: self.span(vname_span.start, self.last_end),
                });
                if self.peek() == &Tok::Comma {
                    self.next();
                    if self.peek() == &Tok::RBrace {
                        break; // 末尾カンマ可
                    }
                } else {
                    break;
                }
            }
        }
        self.eat(&Tok::RBrace)?;
        Ok(EnumDef {
            name,
            variants,
            span: self.span(start, self.last_end),
            is_pub,
        })
    }

    /// `import "相対パス.lum";`
    fn parse_import(&mut self) -> Result<ImportDecl, Diagnostic> {
        let start = self.cur_span().start;
        self.eat(&Tok::Import)?;
        let path_span = self.cur_span();
        let path = match self.next().kind {
            Tok::Str(s) => s,
            other => {
                return Err(Diagnostic::error(format!(
                    "import にはパス文字列が必要ですが {:?} が来ました",
                    other
                ))
                .with_code("E0002")
                .at(path_span));
            }
        };
        self.eat(&Tok::Semicolon)?;
        Ok(ImportDecl {
            path,
            span: self.span(start, self.last_end),
        })
    }

    fn parse_struct(&mut self, is_pub: bool) -> Result<StructDef, Diagnostic> {
        let start = self.cur_span().start;
        self.eat(&Tok::Struct)?;
        let (name, _) = self.parse_ident()?;
        self.eat(&Tok::LBrace)?;
        let mut fields = Vec::new();
        if self.peek() != &Tok::RBrace {
            loop {
                let (fname, name_span) = self.parse_ident()?;
                self.eat(&Tok::Colon)?;
                let (fty, ty_span) = self.parse_type()?;
                fields.push(Param {
                    name: fname,
                    ty: fty,
                    span: name_span.merge(ty_span),
                });
                if self.peek() == &Tok::Comma {
                    self.next();
                    // 末尾カンマを許す
                    if self.peek() == &Tok::RBrace {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        self.eat(&Tok::RBrace)?;
        Ok(StructDef {
            name,
            fields,
            span: self.span(start, self.last_end),
            is_pub,
        })
    }

    fn parse_function(&mut self, is_pub: bool) -> Result<Function, Diagnostic> {
        let start = self.cur_span().start;
        self.eat(&Tok::Fn)?;
        let (name, _) = self.parse_ident()?;
        self.eat(&Tok::LParen)?;
        let mut params = Vec::new();
        if self.peek() != &Tok::RParen {
            loop {
                // 引数は `名前: 型` という型注釈付き
                let (pname, name_span) = self.parse_ident()?;
                self.eat(&Tok::Colon)?;
                let (pty, ty_span) = self.parse_type()?;
                params.push(Param {
                    name: pname,
                    ty: pty,
                    span: name_span.merge(ty_span),
                });
                if self.peek() == &Tok::Comma {
                    self.next();
                } else {
                    break;
                }
            }
        }
        self.eat(&Tok::RParen)?;
        // 戻り値の型は `-> 型`。省略時は int。
        let ret = if self.peek() == &Tok::Arrow {
            self.next();
            self.parse_type()?.0
        } else {
            Type::Int
        };
        let body = self.parse_block()?;
        Ok(Function {
            name,
            params,
            ret,
            body,
            span: self.span(start, self.last_end),
            is_pub,
        })
    }

    fn parse_type(&mut self) -> Result<(Type, Span), Diagnostic> {
        let span = self.cur_span();
        match self.next().kind {
            Tok::Ident(name) => match name.as_str() {
                "int" => Ok((Type::Int, span)),
                "bool" => Ok((Type::Bool, span)),
                "float" => Ok((Type::Float, span)),
                "string" => Ok((Type::Str, span)),
                // それ以外は構造体名（実在するかは typeck が検証）
                _ => Ok((Type::Struct(intern(&name)), span)),
            },
            // 配列型 [T]（T はスカラ・構造体・配列・map。入れ子可）
            Tok::LBracket => {
                let (elem_ty, _) = self.parse_type()?;
                self.eat(&Tok::RBracket)?;
                let full = self.span(span.start, self.last_end);
                // 型注釈に null は書けないので as_elem は必ず Some
                let elem = elem_ty.as_elem().expect("型注釈に null は現れない");
                Ok((Type::Array(elem), full))
            }
            // map 型 {string: V}（キーは string 固定、値はスカラ・構造体・配列・map）
            Tok::LBrace => {
                let (key_ty, key_span) = self.parse_type()?;
                if key_ty != Type::Str {
                    return Err(Diagnostic::error(format!(
                        "map のキーは string でなければなりません（{} は不可）",
                        key_ty.name()
                    ))
                    .with_code("E0300")
                    .at(key_span));
                }
                self.eat(&Tok::Colon)?;
                let (val_ty, _) = self.parse_type()?;
                self.eat(&Tok::RBrace)?;
                let full = self.span(span.start, self.last_end);
                let v = val_ty.as_elem().expect("型注釈に null は現れない");
                Ok((Type::Map(v), full))
            }
            other => Err(
                Diagnostic::error(format!("型名を期待しましたが {:?} が来ました", other))
                    .with_code("E0300")
                    .at(span),
            ),
        }
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>, Diagnostic> {
        self.eat(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        while self.peek() != &Tok::RBrace && self.peek() != &Tok::Eof {
            stmts.push(self.parse_stmt()?);
        }
        self.eat(&Tok::RBrace)?;
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.cur_span().start;
        let kind = match self.peek() {
            Tok::Let => {
                self.next();
                let (name, _) = self.parse_ident()?;
                let ty = if self.peek() == &Tok::Colon {
                    self.next();
                    Some(self.parse_type()?.0)
                } else {
                    None
                };
                self.eat(&Tok::Assign)?;
                let value = self.parse_expr()?;
                self.eat(&Tok::Semicolon)?;
                StmtKind::Let { name, ty, value }
            }
            Tok::Return => {
                self.next();
                let value = self.parse_expr()?;
                self.eat(&Tok::Semicolon)?;
                StmtKind::Return(value)
            }
            Tok::Print => {
                self.next();
                let value = self.parse_expr()?;
                self.eat(&Tok::Semicolon)?;
                StmtKind::Print(value)
            }
            Tok::If => {
                self.next();
                self.eat(&Tok::LParen)?;
                let cond = self.parse_expr()?;
                self.eat(&Tok::RParen)?;
                let then_body = self.parse_block()?;
                let else_body = if self.peek() == &Tok::Else {
                    self.next();
                    // "else if" は else の中に if 文を1つ置く形にする
                    if self.peek() == &Tok::If {
                        vec![self.parse_stmt()?]
                    } else {
                        self.parse_block()?
                    }
                } else {
                    Vec::new()
                };
                StmtKind::If {
                    cond,
                    then_body,
                    else_body,
                }
            }
            Tok::While => {
                self.next();
                self.eat(&Tok::LParen)?;
                let cond = self.parse_expr()?;
                self.eat(&Tok::RParen)?;
                let body = self.parse_block()?;
                StmtKind::While { cond, body }
            }
            Tok::Match => self.parse_match()?,
            Tok::For => {
                self.next();
                self.eat(&Tok::LParen)?;
                // for-in: `for (x in iter)` — 先頭が `Ident in` ならこちら
                if matches!(self.peek(), Tok::Ident(_)) && self.peek2() == &Tok::In {
                    let (var, _) = self.parse_ident()?;
                    self.eat(&Tok::In)?;
                    let iter = self.parse_expr()?;
                    self.eat(&Tok::RParen)?;
                    let body = self.parse_block()?;
                    StmtKind::ForIn { var, iter, body }
                } else {
                    // for (init; cond; step) — init と step は省略可
                    let init = if self.peek() == &Tok::Semicolon {
                        None
                    } else {
                        Some(Box::new(self.parse_simple()?))
                    };
                    self.eat(&Tok::Semicolon)?;
                    let cond = self.parse_expr()?;
                    self.eat(&Tok::Semicolon)?;
                    let step = if self.peek() == &Tok::RParen {
                        None
                    } else {
                        Some(Box::new(self.parse_simple()?))
                    };
                    self.eat(&Tok::RParen)?;
                    let body = self.parse_block()?;
                    StmtKind::For {
                        init,
                        cond,
                        step,
                        body,
                    }
                }
            }
            Tok::Break => {
                self.next();
                self.eat(&Tok::Semicolon)?;
                StmtKind::Break
            }
            Tok::Continue => {
                self.next();
                self.eat(&Tok::Semicolon)?;
                StmtKind::Continue
            }
            // 式から始まる文: `左辺値 = 式;`（代入・複合代入）か、式文
            _ => {
                let e = self.parse_expr()?;
                let k = self.finish_assign_or_expr(e)?;
                self.eat(&Tok::Semicolon)?;
                k
            }
        };
        Ok(Stmt {
            kind,
            span: self.span(start, self.last_end),
        })
    }

    /// `match scrut { Pattern => 文 ... }`。Pattern は `_` か `Variant` か
    /// `Variant(b0, b1, ...)`。各アーム本体は1つの文（ブロックでも可）。
    fn parse_match(&mut self) -> Result<StmtKind, Diagnostic> {
        self.eat(&Tok::Match)?;
        // 被検査値は括弧で囲む（if/while と同様）。これで直後の `{` を構造体リテラルと
        // 取り違えずに match 本体の開きと判別できる。
        self.eat(&Tok::LParen)?;
        let scrut = self.parse_expr()?;
        self.eat(&Tok::RParen)?;
        self.eat(&Tok::LBrace)?;
        let mut arms = Vec::new();
        while self.peek() != &Tok::RBrace && self.peek() != &Tok::Eof {
            let arm_start = self.cur_span().start;
            // パターン: `_`（ワイルドカード）か `Variant` / `Variant(b...)`。
            // `_` は識別子としてレキシングされる。
            let (vname, _) = self.parse_ident()?;
            let (wildcard, variant, bindings) = if vname == "_" {
                (true, String::new(), Vec::new())
            } else {
                let mut binds = Vec::new();
                if self.peek() == &Tok::LParen {
                    self.next();
                    if self.peek() != &Tok::RParen {
                        loop {
                            let (b, _) = self.parse_ident()?;
                            binds.push(b);
                            if self.peek() == &Tok::Comma {
                                self.next();
                            } else {
                                break;
                            }
                        }
                    }
                    self.eat(&Tok::RParen)?;
                }
                (false, vname, binds)
            };
            self.eat(&Tok::FatArrow)?;
            // 本体はブロック `{ ... }` か、単一の文 `stmt;`。
            let body = if self.peek() == &Tok::LBrace {
                self.parse_block()?
            } else {
                vec![self.parse_stmt()?]
            };
            arms.push(MatchArm {
                wildcard,
                variant,
                bindings,
                span: self.span(arm_start, self.last_end),
                body,
            });
        }
        self.eat(&Tok::RBrace)?;
        Ok(StmtKind::Match { scrut, arms })
    }

    /// セミコロンを伴わない単純文（let / 代入 / 式）。for の init・step 用。
    fn parse_simple(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.cur_span().start;
        let kind = match self.peek() {
            Tok::Let => {
                self.next();
                let (name, _) = self.parse_ident()?;
                let ty = if self.peek() == &Tok::Colon {
                    self.next();
                    Some(self.parse_type()?.0)
                } else {
                    None
                };
                self.eat(&Tok::Assign)?;
                let value = self.parse_expr()?;
                StmtKind::Let { name, ty, value }
            }
            _ => {
                let e = self.parse_expr()?;
                self.finish_assign_or_expr(e)?
            }
        };
        Ok(Stmt {
            kind,
            span: self.span(start, self.last_end),
        })
    }

    /// 左辺(式)を読んだ後、`=`・複合代入(`+= -= *= /= %=`)・式文のどれかを作る。
    /// 複合代入 `a OP= b` は `a = a OP b` に脱糖する。左辺 `a` が2回現れるので、
    /// 添字やキーに副作用のある式を書くとそれが2回評価される点に注意（通常は無害）。
    fn finish_assign_or_expr(&mut self, e: Expr) -> Result<StmtKind, Diagnostic> {
        let compound = match self.peek() {
            Tok::PlusEq => Some(BinOp::Add),
            Tok::MinusEq => Some(BinOp::Sub),
            Tok::StarEq => Some(BinOp::Mul),
            Tok::SlashEq => Some(BinOp::Div),
            Tok::PercentEq => Some(BinOp::Mod),
            Tok::AmpEq => Some(BinOp::BitAnd),
            Tok::PipeEq => Some(BinOp::BitOr),
            Tok::CaretEq => Some(BinOp::BitXor),
            Tok::ShlEq => Some(BinOp::Shl),
            Tok::ShrEq => Some(BinOp::Shr),
            _ => None,
        };
        if self.peek() == &Tok::Assign {
            self.next();
            let value = self.parse_expr()?;
            Ok(StmtKind::Assign { target: e, value })
        } else if let Some(op) = compound {
            self.next();
            let value = self.parse_expr()?;
            let span = self.span(e.span.start, self.last_end);
            let combined = Expr {
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(e.clone()),
                    rhs: Box::new(value),
                },
                span,
            };
            Ok(StmtKind::Assign {
                target: e,
                value: combined,
            })
        } else {
            Ok(StmtKind::ExprStmt(e))
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_and()?;
        while self.peek() == &Tok::PipePipe {
            self.next();
            let rhs = self.parse_and()?;
            lhs = binary(BinOp::Or, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_bitor()?;
        while self.peek() == &Tok::AmpAmp {
            self.next();
            let rhs = self.parse_bitor()?;
            lhs = binary(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    // ビット演算は C/Rust に倣い | < ^ < & < 比較 の優先順位。
    fn parse_bitor(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_bitxor()?;
        while self.peek() == &Tok::Pipe {
            self.next();
            let rhs = self.parse_bitxor()?;
            lhs = binary(BinOp::BitOr, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_bitxor(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_bitand()?;
        while self.peek() == &Tok::Caret {
            self.next();
            let rhs = self.parse_bitand()?;
            lhs = binary(BinOp::BitXor, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_bitand(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_comparison()?;
        while self.peek() == &Tok::Amp {
            self.next();
            let rhs = self.parse_comparison()?;
            lhs = binary(BinOp::BitAnd, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_comparison(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_shift()?;
        loop {
            let op = match self.peek() {
                Tok::EqEq => BinOp::Eq,
                Tok::NotEq => BinOp::Ne,
                Tok::Lt => BinOp::Lt,
                Tok::Le => BinOp::Le,
                Tok::Gt => BinOp::Gt,
                Tok::Ge => BinOp::Ge,
                _ => break,
            };
            self.next();
            let rhs = self.parse_shift()?;
            lhs = binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    // シフトは比較より高く、加減より低い（`a + b << c` は `(a+b) << c`）。
    fn parse_shift(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_additive()?;
        loop {
            let op = match self.peek() {
                Tok::Shl => BinOp::Shl,
                Tok::Shr => BinOp::Shr,
                _ => break,
            };
            self.next();
            let rhs = self.parse_additive()?;
            lhs = binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.next();
            let rhs = self.parse_multiplicative()?;
            lhs = binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Mod,
                _ => break,
            };
            self.next();
            let rhs = self.parse_unary()?;
            lhs = binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, Diagnostic> {
        let op = match self.peek() {
            Tok::Minus => Some(UnOp::Neg),
            Tok::Bang => Some(UnOp::Not),
            Tok::Tilde => Some(UnOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            let op_span = self.cur_span();
            self.next();
            let operand = self.parse_unary()?;
            let span = op_span.merge(operand.span);
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op,
                    expr: Box::new(operand),
                },
                span,
            });
        }
        self.parse_postfix()
    }

    /// 一次式に後置の添字 `[index]` とフィールドアクセス `.field` を（連鎖して）付ける。
    fn parse_postfix(&mut self) -> Result<Expr, Diagnostic> {
        let mut e = self.parse_primary()?;
        loop {
            if self.peek() == &Tok::LBracket {
                self.next();
                // 角括弧の中身は添字 `[i]` かスライス `[lo:hi]`。`:` の有無で区別する。
                // lo/hi はどちらも省略でき（`[:]` `[i:]` `[:j]`）、省略は 0／長さを意味する。
                let lo = if self.peek() == &Tok::Colon {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                if self.peek() == &Tok::Colon {
                    self.next(); // ':' を読み飛ばす
                    let hi = if self.peek() == &Tok::RBracket {
                        None
                    } else {
                        Some(self.parse_expr()?)
                    };
                    self.eat(&Tok::RBracket)?;
                    let span = self.span(e.span.start, self.last_end);
                    e = Expr {
                        kind: ExprKind::Slice {
                            seq: Box::new(e),
                            lo: lo.map(Box::new),
                            hi: hi.map(Box::new),
                        },
                        span,
                    };
                } else {
                    // `:` が無ければ添字。lo は必ず Some（先頭が `:` なら上で slice 側へ）。
                    let index = lo.expect("添字には式があるはず");
                    self.eat(&Tok::RBracket)?;
                    let span = self.span(e.span.start, self.last_end);
                    e = Expr {
                        kind: ExprKind::Index {
                            array: Box::new(e),
                            index: Box::new(index),
                        },
                        span,
                    };
                }
            } else if self.peek() == &Tok::Dot {
                self.next();
                let (field, _) = self.parse_ident()?;
                let span = self.span(e.span.start, self.last_end);
                e = Expr {
                    kind: ExprKind::Field {
                        obj: Box::new(e),
                        field,
                    },
                    span,
                };
            } else {
                break;
            }
        }
        Ok(e)
    }

    /// 補間文字列の構成片を `("lit" + str(expr) + "lit" + …)` という式に脱糖する。
    /// typeck/codegen は通常の `+`/`str()` として扱うので追加変更は要らない。
    fn desugar_interp(&self, segments: Vec<Segment>, span: Span) -> Result<Expr, Diagnostic> {
        let mut acc: Option<Expr> = None;
        for seg in segments {
            let part = match seg {
                Segment::Lit(s) => Expr {
                    kind: ExprKind::Str(s),
                    span,
                },
                Segment::Expr { src, offset } => {
                    let inner = self.parse_interp_expr(&src, offset, span.file)?;
                    let s = inner.span;
                    Expr {
                        kind: ExprKind::Call {
                            name: "str".to_string(),
                            args: vec![inner],
                        },
                        span: s,
                    }
                }
            };
            acc = Some(match acc {
                None => part,
                Some(a) => binary(BinOp::Add, a, part),
            });
        }
        // segments は必ず Lit を含む（空でも Lit("")）ので acc は Some
        Ok(acc.unwrap_or(Expr {
            kind: ExprKind::Str(String::new()),
            span,
        }))
    }

    /// 補間 `{…}` の中の式ソースを再字句解析・再構文解析して 1 つの式にする。
    /// トークンの span を `offset` だけずらし、エラーが元ファイルの正しい位置を指すようにする。
    fn parse_interp_expr(
        &self,
        src: &str,
        offset: usize,
        file: FileId,
    ) -> Result<Expr, Diagnostic> {
        let mut toks = lex(src, file)?;
        for t in &mut toks {
            t.span.start += offset;
            t.span.end += offset;
        }
        let mut p = Parser {
            toks,
            pos: 0,
            last_end: 0,
            file,
        };
        let e = p.parse_expr()?;
        if p.peek() != &Tok::Eof {
            return Err(Diagnostic::error("補間 {…} の中は 1 つの式にしてください")
                .with_code("E0002")
                .at(p.cur_span()));
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<Expr, Diagnostic> {
        let span = self.cur_span();
        match self.next().kind {
            Tok::Int(n) => Ok(Expr {
                kind: ExprKind::Int(n),
                span,
            }),
            Tok::Float(x) => Ok(Expr {
                kind: ExprKind::Float(x),
                span,
            }),
            Tok::Str(s) => Ok(Expr {
                kind: ExprKind::Str(s),
                span,
            }),
            // 補間文字列を `("lit" + str(expr) + "lit" + …)` に脱糖する
            Tok::InterpStr(segments) => self.desugar_interp(segments, span),
            Tok::True => Ok(Expr {
                kind: ExprKind::Bool(true),
                span,
            }),
            Tok::False => Ok(Expr {
                kind: ExprKind::Bool(false),
                span,
            }),
            Tok::Null => Ok(Expr {
                kind: ExprKind::Null,
                span,
            }),
            Tok::LParen => {
                let e = self.parse_expr()?;
                self.eat(&Tok::RParen)?;
                Ok(e)
            }
            Tok::LBracket => {
                // 配列リテラル [e1, e2, ...]
                let mut elems = Vec::new();
                if self.peek() != &Tok::RBracket {
                    loop {
                        elems.push(self.parse_expr()?);
                        if self.peek() == &Tok::Comma {
                            self.next();
                            // 末尾カンマを許す
                            if self.peek() == &Tok::RBracket {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                self.eat(&Tok::RBracket)?;
                Ok(Expr {
                    kind: ExprKind::Array(elems),
                    span: self.span(span.start, self.last_end),
                })
            }
            // map リテラル {key: value, ...}（先頭が名前なら構造体リテラルなので、
            // ここに来る `{` は必ず map。空 `{}` も可）
            Tok::LBrace => {
                let mut pairs = Vec::new();
                if self.peek() != &Tok::RBrace {
                    loop {
                        let key = self.parse_expr()?;
                        self.eat(&Tok::Colon)?;
                        let value = self.parse_expr()?;
                        pairs.push((key, value));
                        if self.peek() == &Tok::Comma {
                            self.next();
                            // 末尾カンマを許す
                            if self.peek() == &Tok::RBrace {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                self.eat(&Tok::RBrace)?;
                Ok(Expr {
                    kind: ExprKind::MapLit(pairs),
                    span: self.span(span.start, self.last_end),
                })
            }
            Tok::Ident(name) => {
                if self.peek() == &Tok::LParen {
                    // 関数呼び出し
                    self.next();
                    let mut args = Vec::new();
                    if self.peek() != &Tok::RParen {
                        loop {
                            args.push(self.parse_expr()?);
                            if self.peek() == &Tok::Comma {
                                self.next();
                            } else {
                                break;
                            }
                        }
                    }
                    self.eat(&Tok::RParen)?;
                    Ok(Expr {
                        kind: ExprKind::Call { name, args },
                        span: self.span(span.start, self.last_end),
                    })
                } else if self.peek() == &Tok::LBrace {
                    // 構造体リテラル Name { field: value, ... }
                    self.next();
                    let mut fields = Vec::new();
                    if self.peek() != &Tok::RBrace {
                        loop {
                            let (fname, fname_span) = self.parse_ident()?;
                            self.eat(&Tok::Colon)?;
                            let value = self.parse_expr()?;
                            fields.push(FieldInit {
                                span: self.span(fname_span.start, value.span.end),
                                name: fname,
                                value,
                            });
                            if self.peek() == &Tok::Comma {
                                self.next();
                                if self.peek() == &Tok::RBrace {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                    }
                    self.eat(&Tok::RBrace)?;
                    Ok(Expr {
                        kind: ExprKind::StructLit { name, fields },
                        span: self.span(span.start, self.last_end),
                    })
                } else {
                    Ok(Expr {
                        kind: ExprKind::Var(name),
                        span,
                    })
                }
            }
            other => Err(
                Diagnostic::error(format!("式を期待しましたが {:?} が来ました", other))
                    .with_code("E0002")
                    .at(span),
            ),
        }
    }
}

fn binary(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
    let span = lhs.span.merge(rhs.span);
    Expr {
        kind: ExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        span,
    }
}
