//! 型検査 (Type Checking): パース後・コード生成前に走る独立したパス。
//!
//! 名前解決（変数・関数）と型の整合性をまとめて検査し、位置付きの診断を出す。
//! ここを通過したプログラムは「型が正しい」とみなせるので、codegen は
//! 型エラーを気にせず純粋な低レベル化に専念できる。

use std::collections::HashMap;

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::span::Span;
use crate::types::{intern, Type};

/// 関数シグネチャ（引数の型と戻り値の型）
struct Sig {
    params: Vec<Type>,
    ret: Type,
}

/// 構造体名 -> フィールド（定義順の (名前, 型)）
type Structs = HashMap<String, Vec<(String, Type)>>;

/// 型が実在する型を指すか検証する（構造体名が定義済みか。配列の要素も見る）。
fn validate_type(t: Type, structs: &Structs, span: Span) -> Result<(), Diagnostic> {
    let struct_name = match t {
        Type::Struct(n) => Some(n),
        Type::Array(crate::types::Elem::Struct(n)) => Some(n),
        _ => None,
    };
    if let Some(n) = struct_name {
        if !structs.contains_key(n) {
            return Err(Diagnostic::error(format!("不明な型: {}", n))
                .with_code("E0300")
                .at(span));
        }
    }
    Ok(())
}

pub fn check(program: &Program) -> Result<(), Diagnostic> {
    // 1) 構造体レジストリを作る（フィールド名の重複・予約名を検査）
    let mut structs: Structs = HashMap::new();
    for s in &program.structs {
        if is_reserved_name(&s.name) {
            return Err(Diagnostic::error(format!(
                "{} は予約語なので構造体名に使えません",
                s.name
            ))
            .with_code("E0302")
            .at(s.span));
        }
        if structs.contains_key(&s.name) {
            return Err(
                Diagnostic::error(format!("構造体 {} が二重に定義されています", s.name))
                    .with_code("E0304")
                    .at(s.span),
            );
        }
        let mut fields: Vec<(String, Type)> = Vec::new();
        for f in &s.fields {
            if fields.iter().any(|(n, _)| n == &f.name) {
                return Err(
                    Diagnostic::error(format!("フィールド {} が重複しています", f.name))
                        .with_code("E0307")
                        .at(f.span),
                );
            }
            fields.push((f.name.clone(), f.ty));
        }
        structs.insert(s.name.clone(), fields);
    }
    // フィールドの型が実在する型を指すか検証
    for s in &program.structs {
        for f in &s.fields {
            validate_type(f.ty, &structs, f.span)?;
        }
    }

    // 2) 関数シグネチャを集める（前方参照・相互再帰のため）
    let mut sigs: HashMap<String, Sig> = HashMap::new();
    for f in &program.funcs {
        if is_reserved_name(&f.name) || structs.contains_key(&f.name) {
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
        for p in &f.params {
            validate_type(p.ty, &structs, p.span)?;
        }
        validate_type(f.ret, &structs, f.span)?;
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

    // 3) 各関数の本体を検査
    for f in &program.funcs {
        let mut checker = FnChecker {
            sigs: &sigs,
            structs: &structs,
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
    structs: &'a Structs,
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
            StmtKind::Let { name, ty, value } => {
                let t = self.check_expr(value)?;
                let declared = match ty {
                    Some(a) => {
                        validate_type(*a, self.structs, stmt.span)?;
                        // 初期値が注釈型に代入可能か（null は参照型に可）
                        if t != *a && !(t == Type::Null && a.is_reference()) {
                            return Err(Diagnostic::error(format!(
                                "{} 型の変数に {} 型を入れようとしました",
                                a.name(),
                                t.name()
                            ))
                            .with_code("E0200")
                            .at(value.span));
                        }
                        *a
                    }
                    None => {
                        if t == Type::Null {
                            return Err(Diagnostic::error(
                                "null だけからは変数の型を推論できません（型注釈 `let x: T = null` を使ってください）",
                            )
                            .with_code("E0208")
                            .at(value.span));
                        }
                        t
                    }
                };
                // 最内スコープに束縛（外側の同名変数はシャドーイング）
                self.declare(name, declared);
            }
            StmtKind::Assign { target, value } => {
                // 文字列はイミュータブル: s[i] = ... は不可
                if let ExprKind::Index { array, .. } = &target.kind {
                    if self.check_expr(array)? == Type::Str {
                        return Err(Diagnostic::error(
                            "文字列はイミュータブルです（要素を書き換えられません）",
                        )
                        .with_code("E0207")
                        .at(target.span));
                    }
                }
                // 左辺は変数か添字かフィールド（lvalue）。その型と右辺の型を一致させる。
                let target_ty = match &target.kind {
                    ExprKind::Var(_) | ExprKind::Index { .. } | ExprKind::Field { .. } => {
                        self.check_expr(target)?
                    }
                    _ => {
                        return Err(Diagnostic::error(
                            "代入先が変数・配列要素・フィールドのいずれでもありません",
                        )
                        .with_code("E0204")
                        .at(target.span));
                    }
                };
                let t = self.check_expr(value)?;
                // null は参照型の代入先に入れられる
                if t != target_ty && !(t == Type::Null && target_ty.is_reference()) {
                    return Err(Diagnostic::error(format!(
                        "代入先は {} 型ですが {} 型を代入しようとしました",
                        target_ty.name(),
                        t.name()
                    ))
                    .with_code("E0200")
                    .at(stmt.span));
                }
            }
            StmtKind::Print(e) => {
                let t = self.check_expr(e)?;
                if !matches!(t, Type::Int | Type::Bool | Type::Float | Type::Str) {
                    return Err(Diagnostic::error(format!(
                        "print できるのは int/bool/float/string です（{} は不可）",
                        t.name()
                    ))
                    .with_code("E0200")
                    .at(e.span));
                }
            }
            StmtKind::Return(e) => {
                let t = self.check_expr(e)?;
                if t != self.ret && !(t == Type::Null && self.ret.is_reference()) {
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
            ExprKind::Null => Ok(Type::Null),
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
                    BinOp::Add => {
                        // 数値は加算、文字列は連結。どちらも両辺同型。
                        if lt == Type::Str {
                            expect(Type::Str, rt, rhs.span)?;
                            Ok(Type::Str)
                        } else {
                            if !lt.is_numeric() {
                                return Err(numeric_required(lt, lhs.span));
                            }
                            expect(lt, rt, rhs.span)?;
                            Ok(lt)
                        }
                    }
                    BinOp::Sub | BinOp::Mul | BinOp::Div => {
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
                    BinOp::Eq | BinOp::Ne => {
                        // 等価比較: int/float/string 同士、または参照型 vs null -> bool
                        if lt == Type::Null || rt == Type::Null {
                            // 片方が null。もう片方は参照型か null でなければならない。
                            let other = if lt == Type::Null { rt } else { lt };
                            if other != Type::Null && !other.is_reference() {
                                return Err(Diagnostic::error(format!(
                                    "null と比較できるのは参照型(string/array/struct)だけですが {} が使われています",
                                    other.name()
                                ))
                                .with_code("E0200")
                                .at(e.span));
                            }
                        } else if lt == Type::Str {
                            expect(Type::Str, rt, rhs.span)?;
                        } else {
                            if !lt.is_numeric() {
                                return Err(numeric_required(lt, lhs.span));
                            }
                            expect(lt, rt, rhs.span)?;
                        }
                        Ok(Type::Bool)
                    }
                    BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        // 大小比較は int 同士 / float 同士 -> bool
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

                // 組み込み len(): 文字列か配列の長さ -> int
                if name == "len" {
                    if args.len() != 1 {
                        return Err(Diagnostic::error(format!(
                            "len() は引数1個ですが {} 個渡されました",
                            args.len()
                        ))
                        .with_code("E0104")
                        .at(e.span));
                    }
                    let at = self.check_expr(&args[0])?;
                    if !matches!(at, Type::Str | Type::Array(_)) {
                        return Err(Diagnostic::error(format!(
                            "len() は string か配列に使えますが {} が渡されました",
                            at.name()
                        ))
                        .with_code("E0200")
                        .at(args[0].span));
                    }
                    return Ok(Type::Int);
                }

                // 組み込み read_line(): stdin から1行読む。EOF では null。型は string。
                if name == "read_line" {
                    if !args.is_empty() {
                        return Err(Diagnostic::error(format!(
                            "read_line() は引数を取りませんが {} 個渡されました",
                            args.len()
                        ))
                        .with_code("E0104")
                        .at(e.span));
                    }
                    return Ok(Type::Str);
                }

                // 組み込み chr(): バイト値(int)を1文字の文字列にする
                if name == "chr" {
                    if args.len() != 1 {
                        return Err(Diagnostic::error(format!(
                            "chr() は引数1個ですが {} 個渡されました",
                            args.len()
                        ))
                        .with_code("E0104")
                        .at(e.span));
                    }
                    let at = self.check_expr(&args[0])?;
                    expect(Type::Int, at, args[0].span)?;
                    return Ok(Type::Str);
                }

                // 組み込み str(): int/float/bool/string を文字列にする
                if name == "str" {
                    if args.len() != 1 {
                        return Err(Diagnostic::error(format!(
                            "str() は引数1個ですが {} 個渡されました",
                            args.len()
                        ))
                        .with_code("E0104")
                        .at(e.span));
                    }
                    let at = self.check_expr(&args[0])?;
                    if !matches!(at, Type::Int | Type::Float | Type::Bool | Type::Str) {
                        return Err(Diagnostic::error(format!(
                            "str() は int/float/bool/string を変換しますが {} が渡されました",
                            at.name()
                        ))
                        .with_code("E0200")
                        .at(args[0].span));
                    }
                    return Ok(Type::Str);
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
            ExprKind::Array(elems) => {
                // 空の配列リテラルは要素型を推論できないので不可
                let first = elems.first().ok_or_else(|| {
                    Diagnostic::error("空の配列リテラルは書けません（要素型を推論できません）")
                        .with_code("E0206")
                        .at(e.span)
                })?;
                let elem_ty = self.check_expr(first)?;
                let elem = elem_ty.as_elem().ok_or_else(|| {
                    Diagnostic::error(format!(
                        "配列の要素にできるのは int/bool/float/string です（{} は不可）",
                        elem_ty.name()
                    ))
                    .with_code("E0206")
                    .at(first.span)
                })?;
                // 残りの要素も同じ型か検査
                for el in &elems[1..] {
                    let t = self.check_expr(el)?;
                    expect(elem_ty, t, el.span)?;
                }
                Ok(Type::Array(elem))
            }
            ExprKind::Index { array, index } => {
                let arr_ty = self.check_expr(array)?;
                let it = self.check_expr(index)?;
                expect(Type::Int, it, index.span)?;
                match arr_ty {
                    Type::Array(elem) => Ok(elem.to_type()),
                    // 文字列の添字は i 番目のバイトを int で返す
                    Type::Str => Ok(Type::Int),
                    other => Err(Diagnostic::error(format!(
                        "添字でアクセスできるのは配列か文字列だけですが {} が使われています",
                        other.name()
                    ))
                    .with_code("E0205")
                    .at(array.span)),
                }
            }
            ExprKind::StructLit { name, fields } => {
                let def = self.structs.get(name).ok_or_else(|| {
                    Diagnostic::error(format!("不明な構造体: {}", name))
                        .with_code("E0303")
                        .at(e.span)
                })?;
                // フィールドの過不足・重複・型を検査する（出現順は問わない）
                let mut seen: Vec<&str> = Vec::new();
                for fi in fields {
                    let Some((_, fty)) = def.iter().find(|(n, _)| n == &fi.name) else {
                        return Err(Diagnostic::error(format!(
                            "構造体 {} にフィールド {} はありません",
                            name, fi.name
                        ))
                        .with_code("E0306")
                        .at(fi.span));
                    };
                    if seen.contains(&fi.name.as_str()) {
                        return Err(Diagnostic::error(format!(
                            "フィールド {} が二重に指定されています",
                            fi.name
                        ))
                        .with_code("E0307")
                        .at(fi.span));
                    }
                    seen.push(&fi.name);
                    let vt = self.check_expr(&fi.value)?;
                    expect(*fty, vt, fi.value.span)?;
                }
                if seen.len() != def.len() {
                    let missing: Vec<&str> = def
                        .iter()
                        .filter(|(n, _)| !seen.contains(&n.as_str()))
                        .map(|(n, _)| n.as_str())
                        .collect();
                    return Err(Diagnostic::error(format!(
                        "構造体 {} のフィールドが足りません: {}",
                        name,
                        missing.join(", ")
                    ))
                    .with_code("E0307")
                    .at(e.span));
                }
                Ok(Type::Struct(intern(name)))
            }
            ExprKind::Field { obj, field } => {
                let obj_ty = self.check_expr(obj)?;
                match obj_ty {
                    Type::Struct(sname) => {
                        let def = self.structs.get(sname).unwrap();
                        def.iter()
                            .find(|(n, _)| n == field)
                            .map(|(_, t)| *t)
                            .ok_or_else(|| {
                                Diagnostic::error(format!(
                                    "構造体 {} にフィールド {} はありません",
                                    sname, field
                                ))
                                .with_code("E0306")
                                .at(e.span)
                            })
                    }
                    other => Err(Diagnostic::error(format!(
                        "フィールドアクセスできるのは構造体だけですが {} が使われています",
                        other.name()
                    ))
                    .with_code("E0305")
                    .at(obj.span)),
                }
            }
        }
    }
}

fn expect(want: Type, got: Type, span: Span) -> Result<(), Diagnostic> {
    // null は任意の参照型（string/array/struct）として受け入れる
    if want == got || (got == Type::Null && want.is_reference()) {
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
    matches!(
        name,
        "int" | "float" | "bool" | "string" | "len" | "str" | "chr" | "read_line"
    )
}

fn numeric_required(got: Type, span: Span) -> Diagnostic {
    Diagnostic::error(format!(
        "数値(int または float)が必要ですが {} が使われています",
        got.name()
    ))
    .with_code("E0200")
    .at(span)
}
