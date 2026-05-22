//! 構文解析 (Parser): トークン列を再帰下降でASTに変換する。
//!
//! 演算子の優先順位（低い→高い）:
//!   比較 (== != < <= > >=) < 加減 (+ -) < 乗除 (* / %) < 単項 (-) < 一次式

use crate::ast::*;
use crate::lexer::Tok;

pub fn parse(tokens: Vec<Tok>) -> Result<Program, String> {
    let mut p = Parser { toks: tokens, pos: 0 };
    p.parse_program()
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos]
    }

    fn peek2(&self) -> Option<&Tok> {
        self.toks.get(self.pos + 1)
    }

    fn next(&mut self) -> Tok {
        let t = self.toks[self.pos].clone();
        self.pos += 1;
        t
    }

    fn eat(&mut self, t: &Tok) -> Result<(), String> {
        if self.peek() == t {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("{:?} を期待しましたが {:?} が来ました", t, self.peek()))
        }
    }

    fn parse_ident(&mut self) -> Result<String, String> {
        match self.next() {
            Tok::Ident(s) => Ok(s),
            other => Err(format!("識別子を期待しましたが {:?} が来ました", other)),
        }
    }

    fn parse_program(&mut self) -> Result<Program, String> {
        let mut funcs = Vec::new();
        while self.peek() != &Tok::Eof {
            funcs.push(self.parse_function()?);
        }
        Ok(funcs)
    }

    fn parse_function(&mut self) -> Result<Function, String> {
        self.eat(&Tok::Fn)?;
        let name = self.parse_ident()?;
        self.eat(&Tok::LParen)?;
        let mut params = Vec::new();
        if self.peek() != &Tok::RParen {
            loop {
                params.push(self.parse_ident()?);
                if self.peek() == &Tok::Comma {
                    self.next();
                } else {
                    break;
                }
            }
        }
        self.eat(&Tok::RParen)?;
        let body = self.parse_block()?;
        Ok(Function { name, params, body })
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>, String> {
        self.eat(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        while self.peek() != &Tok::RBrace && self.peek() != &Tok::Eof {
            stmts.push(self.parse_stmt()?);
        }
        self.eat(&Tok::RBrace)?;
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        match self.peek() {
            Tok::Let => {
                self.next();
                let name = self.parse_ident()?;
                self.eat(&Tok::Assign)?;
                let value = self.parse_expr()?;
                self.eat(&Tok::Semicolon)?;
                Ok(Stmt::Let { name, value })
            }
            Tok::Return => {
                self.next();
                let value = self.parse_expr()?;
                self.eat(&Tok::Semicolon)?;
                Ok(Stmt::Return(value))
            }
            Tok::Print => {
                self.next();
                let value = self.parse_expr()?;
                self.eat(&Tok::Semicolon)?;
                Ok(Stmt::Print(value))
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
                Ok(Stmt::If {
                    cond,
                    then_body,
                    else_body,
                })
            }
            Tok::While => {
                self.next();
                self.eat(&Tok::LParen)?;
                let cond = self.parse_expr()?;
                self.eat(&Tok::RParen)?;
                let body = self.parse_block()?;
                Ok(Stmt::While { cond, body })
            }
            // 識別子で始まり次が "=" なら代入文
            Tok::Ident(_) if self.peek2() == Some(&Tok::Assign) => {
                let name = self.parse_ident()?;
                self.eat(&Tok::Assign)?;
                let value = self.parse_expr()?;
                self.eat(&Tok::Semicolon)?;
                Ok(Stmt::Assign { name, value })
            }
            // それ以外は式文（関数呼び出しなど）
            _ => {
                let e = self.parse_expr()?;
                self.eat(&Tok::Semicolon)?;
                Ok(Stmt::ExprStmt(e))
            }
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
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
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.next();
            let rhs = self.parse_multiplicative()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, String> {
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
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        if self.peek() == &Tok::Minus {
            self.next();
            let operand = self.parse_unary()?;
            // -x は 0 - x として表現する
            return Ok(Expr::Binary {
                op: BinOp::Sub,
                lhs: Box::new(Expr::Int(0)),
                rhs: Box::new(operand),
            });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Tok::Int(n) => Ok(Expr::Int(n)),
            Tok::LParen => {
                let e = self.parse_expr()?;
                self.eat(&Tok::RParen)?;
                Ok(e)
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
                    Ok(Expr::Call { name, args })
                } else {
                    Ok(Expr::Var(name))
                }
            }
            other => Err(format!("式を期待しましたが {:?} が来ました", other)),
        }
    }
}
