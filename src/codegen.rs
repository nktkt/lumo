//! コード生成 (Code Generation): AST を LLVM IR に変換する。
//!
//! 値の型は2種類: 整数 `int` (LLVM i64) と真偽値 `bool` (LLVM i1)。
//! `gen_expr` は値とその型 `Ty` を返し、型の整合性をその場で検査する
//! （正式な型検査パスは Phase 3 で導入予定。ここは式の型を下から組み立てる簡易版）。
//!
//! 変数はすべて alloca（スタック領域）に置き、load/store で読み書きする
//! （最適化パスの mem2reg がレジスタに昇格してくれる）。
//!
//! 現状の制限: 関数の引数・戻り値は int のみ（型注釈が無いため）。
//! bool を関数境界で受け渡すには型注釈と型検査(Phase 3)が必要。

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValue, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate, OptimizationLevel};

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::span::Span;

/// Lumo の値の型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Int,
    Bool,
}

impl Ty {
    fn name(self) -> &'static str {
        match self {
            Ty::Int => "int",
            Ty::Bool => "bool",
        }
    }
}

pub struct CodeGen<'ctx> {
    ctx: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    /// 現在の関数内の変数名 -> (スタック上のアドレス, 型)
    vars: HashMap<String, (PointerValue<'ctx>, Ty)>,
    /// 生成済みのグローバル文字列（書式・"true"/"false" など）をキャッシュ
    strings: HashMap<&'static str, PointerValue<'ctx>>,
}

impl<'ctx> CodeGen<'ctx> {
    pub fn new(ctx: &'ctx Context, name: &str) -> Self {
        CodeGen {
            ctx,
            module: ctx.create_module(name),
            builder: ctx.create_builder(),
            vars: HashMap::new(),
            strings: HashMap::new(),
        }
    }

    fn i64t(&self) -> inkwell::types::IntType<'ctx> {
        self.ctx.i64_type()
    }

    /// 型に対応する LLVM の整数型 (int=i64, bool=i1)
    fn llvm_ty(&self, ty: Ty) -> inkwell::types::IntType<'ctx> {
        match ty {
            Ty::Int => self.ctx.i64_type(),
            Ty::Bool => self.ctx.bool_type(),
        }
    }

    pub fn compile(&mut self, program: &Program) -> Result<(), Diagnostic> {
        // 1) C の printf を宣言（print の実装に使う）
        self.declare_printf();

        // 2) すべての関数シグネチャを先に宣言する（前方参照・相互再帰のため）
        //    引数・戻り値は int (i64) 固定。
        for f in program {
            if self.module.get_function(&f.name).is_some() {
                return Err(
                    Diagnostic::error(format!("関数 {} が二重に定義されています", f.name))
                        .with_code("E0103")
                        .at(f.span),
                );
            }
            let i64t = self.i64t();
            let param_types: Vec<BasicMetadataTypeEnum> =
                f.params.iter().map(|_| i64t.into()).collect();
            let fn_type = i64t.fn_type(&param_types, false);
            self.module.add_function(&f.name, fn_type, None);
        }

        // 3) 各関数の本体を生成する
        for f in program {
            self.gen_function(f)?;
        }

        if self.module.get_function("main").is_none() {
            return Err(
                Diagnostic::error("エントリポイント `fn main()` が見つかりません")
                    .with_code("E0100"),
            );
        }

        // 4) 生成したIRの整合性を検証する（コンパイラ側のバグ検出用）
        if self.module.verify().is_err() {
            return Err(Diagnostic::error(format!(
                "内部エラー: 生成したLLVM IRが不正です\n{}",
                self.ir_string()
            )));
        }
        Ok(())
    }

    fn declare_printf(&self) {
        let i32t = self.ctx.i32_type();
        let i8ptr = self.ctx.ptr_type(AddressSpace::default());
        // int printf(char*, ...) — 可変長引数なので最後の引数を true にする
        let printf_ty = i32t.fn_type(&[i8ptr.into()], true);
        self.module
            .add_function("printf", printf_ty, Some(Linkage::External));
    }

    fn gen_function(&mut self, f: &Function) -> Result<(), Diagnostic> {
        let function = self.module.get_function(&f.name).unwrap();
        let entry = self.ctx.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        self.vars.clear();

        // 仮引数(int)をスタックにコピーして、ローカル変数として扱えるようにする
        for (i, pname) in f.params.iter().enumerate() {
            let param = function.get_nth_param(i as u32).unwrap().into_int_value();
            let alloca = self.builder.build_alloca(self.i64t(), pname).unwrap();
            self.builder.build_store(alloca, param).unwrap();
            self.vars.insert(pname.clone(), (alloca, Ty::Int));
        }

        for stmt in &f.body {
            self.gen_stmt(stmt, function)?;
        }

        // 明示的な return が無いまま関数末尾に達したら return 0 を補う
        if self.block_open() {
            self.builder
                .build_return(Some(&self.i64t().const_int(0, false)))
                .unwrap();
        }
        Ok(())
    }

    /// 現在の基本ブロックがまだ終端命令(return/branch)を持っていなければ true
    fn block_open(&self) -> bool {
        self.builder
            .get_insert_block()
            .map(|b| b.get_terminator().is_none())
            .unwrap_or(false)
    }

    /// 現在コード生成中の関数
    fn cur_function(&self) -> FunctionValue<'ctx> {
        self.builder
            .get_insert_block()
            .unwrap()
            .get_parent()
            .unwrap()
    }

    fn expect(&self, want: Ty, got: Ty, span: Span) -> Result<(), Diagnostic> {
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

    fn gen_stmt(&mut self, stmt: &Stmt, function: FunctionValue<'ctx>) -> Result<(), Diagnostic> {
        match &stmt.kind {
            StmtKind::Let { name, value } => {
                let (v, ty) = self.gen_expr(value)?;
                // 同名の再 let は新しい alloca で上書きする（型が変わってもよい）
                let alloca = self.builder.build_alloca(self.llvm_ty(ty), name).unwrap();
                self.vars.insert(name.clone(), (alloca, ty));
                self.builder.build_store(alloca, v).unwrap();
            }
            StmtKind::Assign { name, value } => {
                let (v, ty) = self.gen_expr(value)?;
                let (ptr, var_ty) = *self.vars.get(name).ok_or_else(|| {
                    Diagnostic::error(format!("未定義の変数への代入: {}", name))
                        .with_code("E0101")
                        .at(stmt.span)
                })?;
                if ty != var_ty {
                    return Err(Diagnostic::error(format!(
                        "変数 {} は {} 型ですが {} 型を代入しようとしました",
                        name,
                        var_ty.name(),
                        ty.name()
                    ))
                    .with_code("E0200")
                    .at(stmt.span));
                }
                self.builder.build_store(ptr, v).unwrap();
            }
            StmtKind::Print(e) => {
                let (v, ty) = self.gen_expr(e)?;
                let printf = self.module.get_function("printf").unwrap();
                match ty {
                    Ty::Int => {
                        let fmt = self.global_str("%lld\n", "fmt_int");
                        self.builder
                            .build_call(printf, &[fmt.into(), v.into()], "printf_call")
                            .unwrap();
                    }
                    Ty::Bool => {
                        // bool は "true" / "false" として表示する
                        let fmt = self.global_str("%s\n", "fmt_str");
                        let t = self.global_str("true", "str_true");
                        let f = self.global_str("false", "str_false");
                        let s = self.builder.build_select(v, t, f, "boolstr").unwrap();
                        self.builder
                            .build_call(printf, &[fmt.into(), s.into()], "printf_call")
                            .unwrap();
                    }
                }
            }
            StmtKind::Return(e) => {
                let (v, ty) = self.gen_expr(e)?;
                // 関数の戻り値は int 固定
                if ty != Ty::Int {
                    return Err(Diagnostic::error(format!(
                        "関数は int を返しますが {} を返そうとしています",
                        ty.name()
                    ))
                    .with_code("E0202")
                    .at(e.span));
                }
                self.builder.build_return(Some(&v)).unwrap();
            }
            StmtKind::ExprStmt(e) => {
                self.gen_expr(e)?;
            }
            StmtKind::If {
                cond,
                then_body,
                else_body,
            } => {
                let cond_val = self.gen_cond(cond)?;
                let then_bb = self.ctx.append_basic_block(function, "then");
                let else_bb = self.ctx.append_basic_block(function, "else");
                let merge_bb = self.ctx.append_basic_block(function, "ifcont");

                self.builder
                    .build_conditional_branch(cond_val, then_bb, else_bb)
                    .unwrap();

                // then 節
                self.builder.position_at_end(then_bb);
                for s in then_body {
                    self.gen_stmt(s, function)?;
                }
                if self.block_open() {
                    self.builder.build_unconditional_branch(merge_bb).unwrap();
                }

                // else 節
                self.builder.position_at_end(else_bb);
                for s in else_body {
                    self.gen_stmt(s, function)?;
                }
                if self.block_open() {
                    self.builder.build_unconditional_branch(merge_bb).unwrap();
                }

                // 合流点
                self.builder.position_at_end(merge_bb);
            }
            StmtKind::While { cond, body } => {
                let cond_bb = self.ctx.append_basic_block(function, "while.cond");
                let body_bb = self.ctx.append_basic_block(function, "while.body");
                let end_bb = self.ctx.append_basic_block(function, "while.end");

                self.builder.build_unconditional_branch(cond_bb).unwrap();

                self.builder.position_at_end(cond_bb);
                let cond_val = self.gen_cond(cond)?;
                self.builder
                    .build_conditional_branch(cond_val, body_bb, end_bb)
                    .unwrap();

                self.builder.position_at_end(body_bb);
                for s in body {
                    self.gen_stmt(s, function)?;
                }
                if self.block_open() {
                    self.builder.build_unconditional_branch(cond_bb).unwrap();
                }

                self.builder.position_at_end(end_bb);
            }
        }
        Ok(())
    }

    /// 条件式を評価する。型は bool でなければならない。
    fn gen_cond(&mut self, e: &Expr) -> Result<IntValue<'ctx>, Diagnostic> {
        let (v, ty) = self.gen_expr(e)?;
        if ty != Ty::Bool {
            return Err(Diagnostic::error(format!(
                "条件は bool である必要がありますが {} が使われています",
                ty.name()
            ))
            .with_code("E0201")
            .at(e.span));
        }
        Ok(v)
    }

    fn gen_expr(&mut self, e: &Expr) -> Result<(IntValue<'ctx>, Ty), Diagnostic> {
        match &e.kind {
            ExprKind::Int(n) => Ok((self.i64t().const_int(*n as u64, true), Ty::Int)),
            ExprKind::Bool(b) => Ok((self.ctx.bool_type().const_int(*b as u64, false), Ty::Bool)),
            ExprKind::Var(name) => {
                let (ptr, ty) = *self.vars.get(name).ok_or_else(|| {
                    Diagnostic::error(format!("未定義の変数: {}", name))
                        .with_code("E0101")
                        .at(e.span)
                })?;
                let v = self
                    .builder
                    .build_load(self.llvm_ty(ty), ptr, name)
                    .unwrap()
                    .into_int_value();
                Ok((v, ty))
            }
            ExprKind::Unary { op, expr } => {
                let (v, ty) = self.gen_expr(expr)?;
                match op {
                    UnOp::Neg => {
                        self.expect(Ty::Int, ty, expr.span)?;
                        Ok((self.builder.build_int_neg(v, "neg").unwrap(), Ty::Int))
                    }
                    UnOp::Not => {
                        self.expect(Ty::Bool, ty, expr.span)?;
                        Ok((self.builder.build_not(v, "not").unwrap(), Ty::Bool))
                    }
                }
            }
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinOp::And | BinOp::Or => self.gen_logical(*op, lhs, rhs),
                _ => {
                    let (l, lty) = self.gen_expr(lhs)?;
                    let (r, rty) = self.gen_expr(rhs)?;
                    match op {
                        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                            self.expect(Ty::Int, lty, lhs.span)?;
                            self.expect(Ty::Int, rty, rhs.span)?;
                            let b = &self.builder;
                            let v = match op {
                                BinOp::Add => b.build_int_add(l, r, "add").unwrap(),
                                BinOp::Sub => b.build_int_sub(l, r, "sub").unwrap(),
                                BinOp::Mul => b.build_int_mul(l, r, "mul").unwrap(),
                                BinOp::Div => b.build_int_signed_div(l, r, "div").unwrap(),
                                BinOp::Mod => b.build_int_signed_rem(l, r, "rem").unwrap(),
                                _ => unreachable!(),
                            };
                            Ok((v, Ty::Int))
                        }
                        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                            self.expect(Ty::Int, lty, lhs.span)?;
                            self.expect(Ty::Int, rty, rhs.span)?;
                            let pred = match op {
                                BinOp::Eq => IntPredicate::EQ,
                                BinOp::Ne => IntPredicate::NE,
                                BinOp::Lt => IntPredicate::SLT,
                                BinOp::Le => IntPredicate::SLE,
                                BinOp::Gt => IntPredicate::SGT,
                                BinOp::Ge => IntPredicate::SGE,
                                _ => unreachable!(),
                            };
                            let v = self.builder.build_int_compare(pred, l, r, "cmp").unwrap();
                            Ok((v, Ty::Bool))
                        }
                        BinOp::And | BinOp::Or => unreachable!(),
                    }
                }
            },
            ExprKind::Call { name, args } => {
                let function = self.module.get_function(name).ok_or_else(|| {
                    Diagnostic::error(format!("未定義の関数: {}", name))
                        .with_code("E0102")
                        .at(e.span)
                })?;
                let expected = function.count_params() as usize;
                if expected != args.len() {
                    return Err(Diagnostic::error(format!(
                        "関数 {} は引数 {} 個ですが {} 個渡されました",
                        name,
                        expected,
                        args.len()
                    ))
                    .with_code("E0104")
                    .at(e.span));
                }
                let mut argvals: Vec<BasicMetadataValueEnum> = Vec::with_capacity(args.len());
                for a in args {
                    let (av, aty) = self.gen_expr(a)?;
                    // 関数の引数は int のみ
                    self.expect(Ty::Int, aty, a.span)?;
                    argvals.push(av.into());
                }
                let call = self.builder.build_call(function, &argvals, "call").unwrap();
                let v = call.try_as_basic_value().unwrap_basic().into_int_value();
                Ok((v, Ty::Int))
            }
        }
    }

    /// 短絡評価する論理演算 `&&` / `||`。両辺は bool。
    fn gen_logical(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
    ) -> Result<(IntValue<'ctx>, Ty), Diagnostic> {
        let (l, lty) = self.gen_expr(lhs)?;
        self.expect(Ty::Bool, lty, lhs.span)?;

        let function = self.cur_function();
        let entry_bb = self.builder.get_insert_block().unwrap();
        let rhs_bb = self.ctx.append_basic_block(function, "logic.rhs");
        let merge_bb = self.ctx.append_basic_block(function, "logic.merge");

        // && は左が真のときだけ右を評価、|| は左が偽のときだけ右を評価
        if matches!(op, BinOp::And) {
            self.builder
                .build_conditional_branch(l, rhs_bb, merge_bb)
                .unwrap();
        } else {
            self.builder
                .build_conditional_branch(l, merge_bb, rhs_bb)
                .unwrap();
        }

        // 右辺ブロック
        self.builder.position_at_end(rhs_bb);
        let (r, rty) = self.gen_expr(rhs)?;
        self.expect(Ty::Bool, rty, rhs.span)?;
        let rhs_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // 合流: phi で結果を選ぶ
        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(self.ctx.bool_type(), "logic")
            .unwrap();
        // 短絡したときの値: && なら false、|| なら true
        let short = self
            .ctx
            .bool_type()
            .const_int(u64::from(matches!(op, BinOp::Or)), false);
        phi.add_incoming(&[
            (&short as &dyn BasicValue, entry_bb),
            (&r as &dyn BasicValue, rhs_end),
        ]);
        Ok((phi.as_basic_value().into_int_value(), Ty::Bool))
    }

    /// グローバル文字列を名前でキャッシュしつつ作り、ポインタを返す
    fn global_str(&mut self, text: &str, name: &'static str) -> PointerValue<'ctx> {
        if let Some(p) = self.strings.get(name) {
            return *p;
        }
        let g = self
            .builder
            .build_global_string_ptr(text, name)
            .unwrap()
            .as_pointer_value();
        self.strings.insert(name, g);
        g
    }

    pub fn ir_string(&self) -> String {
        self.module.print_to_string().to_string()
    }

    /// JIT で main を即時実行し、終了コードを返す
    pub fn jit_run(&self) -> Result<i64, String> {
        Target::initialize_native(&InitializationConfig::default())?;
        let ee = self
            .module
            .create_jit_execution_engine(OptimizationLevel::None)
            .map_err(|e| e.to_string())?;
        unsafe {
            let main = ee
                .get_function::<unsafe extern "C" fn() -> i64>("main")
                .map_err(|e| e.to_string())?;
            Ok(main.call())
        }
    }

    /// オブジェクトファイルを書き出し、clang でリンクしてネイティブ実行ファイルを作る
    pub fn build_executable(&self, out: &str) -> Result<(), String> {
        Target::initialize_all(&InitializationConfig::default());
        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
        let tm = target
            .create_target_machine(
                &triple,
                "generic",
                "",
                OptimizationLevel::Default,
                RelocMode::PIC,
                CodeModel::Default,
            )
            .ok_or("ターゲットマシンを作成できません")?;

        let obj_path = format!("{}.o", out);
        tm.write_to_file(&self.module, FileType::Object, Path::new(&obj_path))
            .map_err(|e| e.to_string())?;

        let status = Command::new("clang")
            .arg(&obj_path)
            .arg("-o")
            .arg(out)
            .status()
            .map_err(|e| format!("clang の起動に失敗: {}", e))?;
        if !status.success() {
            return Err("リンクに失敗しました".to_string());
        }
        let _ = std::fs::remove_file(&obj_path);
        Ok(())
    }
}
