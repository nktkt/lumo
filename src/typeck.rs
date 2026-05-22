//! 型検査 (Type Checking): パース後・コード生成前に走る独立したパス。
//!
//! 名前解決（変数・関数）と型の整合性をまとめて検査し、位置付きの診断を出す。
//! ここを通過したプログラムは「型が正しい」とみなせるので、codegen は
//! 型エラーを気にせず純粋な低レベル化に専念できる。

use std::collections::HashMap;

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::span::Span;
use crate::types::Type;

/// 関数シグネチャ（引数の型と戻り値の型）
struct Sig {
    params: Vec<Type>,
    ret: Type,
}

pub fn check(program: &Program) -> Result<(), Diagnostic> {
    // 1) 全関数のシグネチャを集める（前方参照・相互再帰のため）
    let mut sigs: HashMap<String, Sig> = HashMap::new();
    for f in program {
        if is_reserved_name(&f.name) {
            return Err(Diagnostic::error(format!(
                "{} は型名・組み込み関数なので関数名に使えません",
                f.name
            ))
            .with_code("E0302")
            .at(f.span));
        }
        if sigs.contains_key(&f.name) {
            return Err(
                Diagnostic::error(format!("関数 {} が二重に定義されています", f.name))
                    .with_code("E0103")
                    .at(f.span),
            );
        }
        sigs.insert(
            f.name.clone(),
            Sig {
                params: f.params.iter().map(|p| p.ty).collect(),
                ret: f.ret,
            },
        );
    }

    if !sigs.contains_key("main") {
        return Err(
            Diagnostic::error("エントリポイント `fn main()` が見つかりません").with_code("E0100"),
        );
    }

    // 2) 各関数の本体を検査
    for f in program {
        let mut checker = FnChecker {
            sigs: &sigs,
            // 関数スコープ（引数とトップレベルのローカル）を最初の層にする
            scopes: vec![HashMap::new()],
            ret: f.ret,
            loops: 0,
        };
        for p in &f.params {
            // 同じスコープ内での重複だけをエラーにする
            if checker.scopes[0].insert(p.name.clone(), p.ty).is_some() {
                return Err(
                    Diagnostic::error(format!("引数 {} が重複しています", p.name))
                        .with_code("E0301")
                        .at(p.span),
                );
            }
        }
        for stmt in &f.body {
            checker.check_stmt(stmt)?;
        }
    }
    Ok(())
}

struct FnChecker<'a> {
    sigs: &'a HashMap<String, Sig>,
    /// レキシカルスコープのスタック（内側が末尾）。`let` は最内へ、参照は内→外で探索。
    scopes: Vec<HashMap<String, Type>>,
    ret: Type,
    /// 入れ子になっているループの深さ（break/continue の検査用）
    loops: u32,
}

impl FnChecker<'_> {
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// 最内スコープに変数を宣言する（外側を隠すシャドーイングを許す）。
    fn declare(&mut self, name: &str, ty: Type) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), ty);
    }

    /// 内側のスコープから順に変数を探す。
    fn lookup(&self, name: &str) -> Option<Type> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    /// 文の並びを新しいスコープで検査する。
    fn check_block(&mut self, stmts: &[Stmt]) -> Result<(), Diagnostic> {
        self.push_scope();
        for s in stmts {
            self.check_stmt(s)?;
        }
        self.pop_scope();
        Ok(())
    }
    fn check_stmt(&mut self, stmt: &Stmt) -> Result<(), Diagnostic> {
        match &stmt.kind {
            StmtKind::Let { name, value } => {
                let t = self.check_expr(value)?;
                // 最内スコープに束縛（外側の同名変数はシャドーイング）
                self.declare(name, t);
            }
            StmtKind::Assign { name, value } => {
                let t = self.check_expr(value)?;
                let var_ty = self.lookup(name).ok_or_else(|| {
                    Diagnostic::error(format!("未定義の変数への代入: {}", name))
                        .with_code("E0101")
                        .at(stmt.span)
                })?;
                if t != var_ty {
                    return Err(Diagnostic::error(format!(
                        "変数 {} は {} 型ですが {} 型を代入しようとしました",
                        name,
                        var_ty.name(),
                        t.name()
                    ))
                    .with_code("E0200")
                    .at(stmt.span));
                }
            }
            StmtKind::Print(e) => {
                self.check_expr(e)?;
            }
            StmtKind::Return(e) => {
                let t = self.check_expr(e)?;
                if t != self.ret {
                    return Err(Diagnostic::error(format!(
                        "関数は {} を返しますが {} を返そうとしています",
                        self.ret.name(),
                        t.name()
                    ))
                    .with_code("E0202")
                    .at(e.span));
                }
            }
            StmtKind::ExprStmt(e) => {
                self.check_expr(e)?;
            }
            StmtKind::If {
                cond,
                then_body,
                else_body,
            } => {
                self.check_cond(cond)?;
                self.check_block(then_body)?;
                self.check_block(else_body)?;
            }
            StmtKind::While { cond, body } => {
                self.check_cond(cond)?;
                self.loops += 1;
                self.check_block(body)?;
                self.loops -= 1;
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                // for 自体のスコープ（init の変数は cond/body/step から見える）
                self.push_scope();
                if let Some(init) = init {
                    self.check_stmt(init)?;
                }
                self.check_cond(cond)?;
                self.loops += 1;
                self.check_block(body)?;
                self.loops -= 1;
                if let Some(step) = step {
                    self.check_stmt(step)?;
                }
                self.pop_scope();
            }
            StmtKind::Break => {
                if self.loops == 0 {
                    return Err(Diagnostic::error("break はループの外では使えません")
                        .with_code("E0203")
                        .at(stmt.span));
                }
            }
            StmtKind::Continue => {
                if self.loops == 0 {
                    return Err(Diagnostic::error("continue はループの外では使えません")
                        .with_code("E0203")
                        .at(stmt.span));
                }
            }
        }
        Ok(())
    }

    fn check_cond(&mut self, e: &Expr) -> Result<(), Diagnostic> {
        let t = self.check_expr(e)?;
        if t != Type::Bool {
            return Err(Diagnostic::error(format!(
                "条件は bool である必要がありますが {} が使われています",
                t.name()
            ))
            .with_code("E0201")
            .at(e.span));
        }
        Ok(())
    }

    fn check_expr(&mut self, e: &Expr) -> Result<Type, Diagnostic> {
        match &e.kind {
            ExprKind::Int(_) => Ok(Type::Int),
            ExprKind::Float(_) => Ok(Type::Float),
            ExprKind::Bool(_) => Ok(Type::Bool),
            ExprKind::Str(_) => Ok(Type::Str),
            ExprKind::Var(name) => self.lookup(name).ok_or_else(|| {
                Diagnostic::error(format!("未定義の変数: {}", name))
                    .with_code("E0101")
                    .at(e.span)
            }),
            ExprKind::Unary { op, expr } => {
                let t = self.check_expr(expr)?;
                match op {
                    UnOp::Neg => {
                        // 単項マイナスは数値(int/float)に使える
                        if !t.is_numeric() {
                            return Err(numeric_required(t, expr.span));
                        }
                        Ok(t)
                    }
                    UnOp::Not => {
                        expect(Type::Bool, t, expr.span)?;
                        Ok(Type::Bool)
                    }
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let lt = self.check_expr(lhs)?;
                let rt = self.check_expr(rhs)?;
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                        // 算術は int 同士 / float 同士。結果は同じ型。
                        if !lt.is_numeric() {
                            return Err(numeric_required(lt, lhs.span));
                        }
                        expect(lt, rt, rhs.span)?;
                        Ok(lt)
                    }
                    BinOp::Mod => {
                        // 剰余は int のみ
                        expect(Type::Int, lt, lhs.span)?;
                        expect(Type::Int, rt, rhs.span)?;
                        Ok(Type::Int)
                    }
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        // 比較は int 同士 / float 同士 -> bool
                        if !lt.is_numeric() {
                            return Err(numeric_required(lt, lhs.span));
                        }
                        expect(lt, rt, rhs.span)?;
                        Ok(Type::Bool)
                    }
                    BinOp::And | BinOp::Or => {
                        expect(Type::Bool, lt, lhs.span)?;
                        expect(Type::Bool, rt, rhs.span)?;
                        Ok(Type::Bool)
                    }
                }
            }
            ExprKind::Call { name, args } => {
                // 組み込み変換 int()/float()
                if name == "int" || name == "float" {
                    if args.len() != 1 {
                        return Err(Diagnostic::error(format!(
                            "{}() は引数1個ですが {} 個渡されました",
                            name,
                            args.len()
                        ))
                        .with_code("E0104")
                        .at(e.span));
                    }
                    let at = self.check_expr(&args[0])?;
                    if !at.is_numeric() {
                        return Err(Diagnostic::error(format!(
                            "{}() は数値(int または float)を変換しますが {} が渡されました",
                            name,
                            at.name()
                        ))
                        .with_code("E0200")
                        .at(args[0].span));
                    }
                    return Ok(if name == "int" {
                        Type::Int
                    } else {
                        Type::Float
                    });
                }

                let (param_types, ret) = {
                    let sig = self.sigs.get(name).ok_or_else(|| {
                        Diagnostic::error(format!("未定義の関数: {}", name))
                            .with_code("E0102")
                            .at(e.span)
                    })?;
                    (sig.params.clone(), sig.ret)
                };
                if param_types.len() != args.len() {
                    return Err(Diagnostic::error(format!(
                        "関数 {} は引数 {} 個ですが {} 個渡されました",
                        name,
                        param_types.len(),
                        args.len()
                    ))
                    .with_code("E0104")
                    .at(e.span));
                }
                for (arg, &pty) in args.iter().zip(param_types.iter()) {
                    let at = self.check_expr(arg)?;
                    expect(pty, at, arg.span)?;
                }
                Ok(ret)
            }
        }
    }
}

fn expect(want: Type, got: Type, span: Span) -> Result<(), Diagnostic> {
    if want == got {
        Ok(())
    } else {
        Err(Diagnostic::error(format!(
            "型が合いません: {} が必要ですが {} が渡されました",
            want.name(),
            got.name()
        ))
        .with_code("E0200")
        .at(span))
    }
}

/// 型名・組み込み関数名として予約されている識別子か（関数名に使えない）。
fn is_reserved_name(name: &str) -> bool {
    matches!(name, "int" | "float" | "bool" | "string")
}

fn numeric_required(got: Type, span: Span) -> Diagnostic {
    Diagnostic::error(format!(
        "数値(int または float)が必要ですが {} が使われています",
        got.name()
    ))
    .with_code("E0200")
    .at(span)
}
