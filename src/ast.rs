//! 抽象構文木 (AST) の定義。値の型は整数(int)と真偽値(bool)の2種類。
//! 各ノードはソース位置 (Span) を持ち、診断メッセージで該当箇所を指せる。

use crate::span::Span;
use crate::types::Type;

#[derive(Debug, Clone, Copy)]
pub enum BinOp {
    // 算術 (int -> int)
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    // 比較 (int, int -> bool)
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    // 論理 (bool, bool -> bool, 短絡評価)
    And,
    Or,
}

#[derive(Debug, Clone, Copy)]
pub enum UnOp {
    Neg, // -x  (int -> int)
    Not, // !x  (bool -> bool)
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Var(String),
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Call {
        name: String,
        args: Vec<Expr>,
    },
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Let {
        name: String,
        value: Expr,
    },
    Assign {
        name: String,
        value: Expr,
    },
    Print(Expr),
    Return(Expr),
    If {
        cond: Expr,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    /// for (init; cond; step) { body }  — init/step は任意
    For {
        init: Option<Box<Stmt>>,
        cond: Expr,
        step: Option<Box<Stmt>>,
        body: Vec<Stmt>,
    },
    Break,
    Continue,
    ExprStmt(Expr),
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    /// 戻り値の型（構文で省略時は int）
    pub ret: Type,
    pub body: Vec<Stmt>,
    pub span: Span,
}

pub type Program = Vec<Function>;
