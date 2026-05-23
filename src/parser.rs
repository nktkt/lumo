//! 構文解析 (Parser): トークン列を再帰下降でASTに変換する。
//! 各ノードにソース位置 (Span) を付け、構文エラーは Diagnostic で位置付きで返す。
//!
//! 演算子の優先順位（低い→高い）:
//!   比較 (== != < <= > >=) < 加減 (+ -) < 乗除 (* / %) < 単項 (-) < 一次式

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::lexer::{Tok, Token};
use crate::span::Span;
use crate::types::{intern, Type};

pub fn parse(tokens: Vec<Token>) -> Result<Program, Diagnostic> {
    let mut p = Parser {
        toks: tokens,
        pos: 0,
        last_end: 0,
    };
    p.parse_program()
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// 直前に消費したトークンの終了オフセット（ノードの span 終端に使う）
    last_end: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].kind
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
        let mut structs = Vec::new();
        let mut funcs = Vec::new();
        while self.peek() != &Tok::Eof {
            if self.peek() == &Tok::Struct {
                structs.push(self.parse_struct()?);
            } else {
                funcs.push(self.parse_function()?);
            }
        }
        Ok(Program { structs, funcs })
    }

    fn parse_struct(&mut self) -> Result<StructDef, Diagnostic> {
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
            span: Span::new(start, self.last_end),
        })
    }

    fn parse_function(&mut self) -> Result<Function, Diagnostic> {
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
            span: Span::new(start, self.last_end),
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
            // 配列型 [T]（T はスカラ。入れ子の配列は不可）
            Tok::LBracket => {
                let (elem_ty, elem_span) = self.parse_type()?;
                self.eat(&Tok::RBracket)?;
                let full = Span::new(span.start, self.last_end);
                let elem = elem_ty.as_elem().ok_or_else(|| {
                    Diagnostic::error(
                        "配列の要素にできるのは int/bool/float/string です（配列の配列は不可）",
                    )
                    .with_code("E0300")
                    .at(elem_span)
                })?;
                Ok((Type::Array(elem), full))
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
            Tok::For => {
                self.next();
                self.eat(&Tok::LParen)?;
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
            // 式から始まる文: `左辺値 = 式;`（代入）か、式文
            _ => {
                let e = self.parse_expr()?;
                let k = if self.peek() == &Tok::Assign {
                    self.next();
                    let value = self.parse_expr()?;
                    StmtKind::Assign { target: e, value }
                } else {
                    StmtKind::ExprStmt(e)
                };
                self.eat(&Tok::Semicolon)?;
                k
            }
        };
        Ok(Stmt {
            kind,
            span: Span::new(start, self.last_end),
        })
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
                if self.peek() == &Tok::Assign {
                    self.next();
                    let value = self.parse_expr()?;
                    StmtKind::Assign { target: e, value }
                } else {
                    StmtKind::ExprStmt(e)
                }
            }
        };
        Ok(Stmt {
            kind,
            span: Span::new(start, self.last_end),
        })
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
        let mut lhs = self.parse_comparison()?;
        while self.peek() == &Tok::AmpAmp {
            self.next();
            let rhs = self.parse_comparison()?;
            lhs = binary(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_comparison(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_additive()?;
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
                let index = self.parse_expr()?;
                self.eat(&Tok::RBracket)?;
                let span = Span::new(e.span.start, self.last_end);
                e = Expr {
                    kind: ExprKind::Index {
                        array: Box::new(e),
                        index: Box::new(index),
                    },
                    span,
                };
            } else if self.peek() == &Tok::Dot {
                self.next();
                let (field, _) = self.parse_ident()?;
                let span = Span::new(e.span.start, self.last_end);
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
                    span: Span::new(span.start, self.last_end),
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
                        span: Span::new(span.start, self.last_end),
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
                                span: Span::new(fname_span.start, value.span.end),
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
                        span: Span::new(span.start, self.last_end),
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
