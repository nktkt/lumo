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
    // ビット演算 (int, int -> int)。シフト量は 64 で剰余を取る。
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Debug, Clone, Copy)]
pub enum UnOp {
    Neg,    // -x  (int -> int)
    Not,    // !x  (bool -> bool)
    BitNot, // ~x  (int -> int)
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
    /// `null` リテラル（参照型と互換）
    Null,
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
    /// 配列リテラル `[e1, e2, ...]`（要素は同じ型・1個以上）
    Array(Vec<Expr>),
    /// 添字アクセス `array[index]`
    Index {
        array: Box<Expr>,
        index: Box<Expr>,
    },
    /// スライス `seq[lo:hi]`（配列・文字列）。`lo` 省略は 0、`hi` 省略は長さ。
    Slice {
        seq: Box<Expr>,
        lo: Option<Box<Expr>>,
        hi: Option<Box<Expr>>,
    },
    /// 構造体リテラル `Name { field: value, ... }`
    StructLit {
        name: String,
        fields: Vec<FieldInit>,
    },
    /// map リテラル `{key: value, ...}`（空 `{}` も。キー・値は式）
    MapLit(Vec<(Expr, Expr)>),
    /// フィールドアクセス `obj.field`
    Field {
        obj: Box<Expr>,
        field: String,
    },
}

/// 構造体リテラル中の1フィールド初期化 `name: value`
#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
    pub span: Span,
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
        /// 任意の型注釈 `let x: T = ...`。省略時は初期値から型を推論する。
        ty: Option<Type>,
        value: Expr,
    },
    /// 代入。左辺 `target` は変数(`Var`)か添字(`Index`)。
    Assign {
        target: Expr,
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
    /// for (var in iter) { body } — 配列なら各要素、map なら各キーを順に束縛する
    ForIn {
        var: String,
        iter: Expr,
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

/// 構造体定義 `struct Name { f: T, ... }`
#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<Param>,
    pub span: Span,
}

/// `import "path";` 宣言（ファイル先頭に置く）。パスは import する側のファイルからの相対。
#[derive(Debug, Clone)]
pub struct ImportDecl {
    pub path: String,
    pub span: Span,
}

/// 1ファイルを解析した結果（import 宣言・構造体定義・関数の集まり）。
/// ドライバが import を解決し、全ファイルの structs/funcs を1つに統合してから
/// typeck/codegen に渡す（それらは `imports` を見ない）。
#[derive(Debug, Clone)]
pub struct Program {
    pub imports: Vec<ImportDecl>,
    pub structs: Vec<StructDef>,
    pub funcs: Vec<Function>,
}
