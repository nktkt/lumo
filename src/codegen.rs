//! コード生成 (Code Generation): 型検査(typeck)を通過した AST を LLVM IR に変換する。
//!
//! typeck が型の正しさを保証しているので、ここでは型エラーを気にせず純粋に
//! 低レベル化する。値は LLVM の `BasicValueEnum` で持ち、Lumo の型
//! (int=i64 / bool=i1 / float=f64) を添えて回すことで命令と print 書式を選ぶ。
//!
//! 変数はすべて alloca（スタック領域）に置き、load/store で読み書きする
//! （最適化パスの mem2reg がレジスタに昇格してくれる）。

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, PointerValue,
};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel};

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::types::Type;

pub struct CodeGen<'ctx> {
    ctx: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    /// 現在の関数内の変数名 -> (スタック上のアドレス, 型)
    vars: HashMap<String, (PointerValue<'ctx>, Type)>,
    /// 関数名 -> 戻り値の型（呼び出し式の型を知るため）
    fn_rets: HashMap<String, Type>,
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
            fn_rets: HashMap::new(),
            strings: HashMap::new(),
        }
    }

    /// Lumo の型に対応する LLVM の基本型 (int=i64, bool=i1, float=f64)
    fn basic_ty(&self, ty: Type) -> BasicTypeEnum<'ctx> {
        match ty {
            Type::Int => self.ctx.i64_type().into(),
            Type::Bool => self.ctx.bool_type().into(),
            Type::Float => self.ctx.f64_type().into(),
        }
    }

    /// 型のゼロ値（関数末尾まで return が無かったときの既定戻り値に使う）
    fn zero_of(&self, ty: Type) -> BasicValueEnum<'ctx> {
        match ty {
            Type::Int => self.ctx.i64_type().const_int(0, false).into(),
            Type::Bool => self.ctx.bool_type().const_int(0, false).into(),
            Type::Float => self.ctx.f64_type().const_float(0.0).into(),
        }
    }

    pub fn compile(&mut self, program: &Program) -> Result<(), Diagnostic> {
        // 1) C の printf を宣言（print の実装に使う）
        self.declare_printf();

        // 2) すべての関数シグネチャを先に宣言する（前方参照・相互再帰のため）。
        //    typeck が重複や main の存在を確認済みなのでここでは検査しない。
        for f in program {
            let param_types: Vec<BasicMetadataTypeEnum> = f
                .params
                .iter()
                .map(|p| self.basic_ty(p.ty).into())
                .collect();
            let fn_type = self.basic_ty(f.ret).fn_type(&param_types, false);
            self.module.add_function(&f.name, fn_type, None);
            self.fn_rets.insert(f.name.clone(), f.ret);
        }

        // 3) 各関数の本体を生成する
        for f in program {
            self.gen_function(f);
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

    fn gen_function(&mut self, f: &Function) {
        let function = self.module.get_function(&f.name).unwrap();
        let entry = self.ctx.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        self.vars.clear();

        // 仮引数をスタックにコピーして、ローカル変数として扱えるようにする
        for (i, p) in f.params.iter().enumerate() {
            let param = function.get_nth_param(i as u32).unwrap();
            let alloca = self
                .builder
                .build_alloca(self.basic_ty(p.ty), &p.name)
                .unwrap();
            self.builder.build_store(alloca, param).unwrap();
            self.vars.insert(p.name.clone(), (alloca, p.ty));
        }

        for stmt in &f.body {
            self.gen_stmt(stmt, function);
        }

        // 明示的な return が無いまま関数末尾に達したら戻り値型のゼロを返す
        if self.block_open() {
            let zero = self.zero_of(f.ret);
            self.builder.build_return(Some(&zero)).unwrap();
        }
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

    fn gen_stmt(&mut self, stmt: &Stmt, function: FunctionValue<'ctx>) {
        match &stmt.kind {
            StmtKind::Let { name, value } => {
                let (v, ty) = self.gen_expr(value);
                // 同名の再 let は新しい alloca で上書きする（型が変わってもよい）
                let alloca = self.builder.build_alloca(self.basic_ty(ty), name).unwrap();
                self.vars.insert(name.clone(), (alloca, ty));
                self.builder.build_store(alloca, v).unwrap();
            }
            StmtKind::Assign { name, value } => {
                let (v, _) = self.gen_expr(value);
                let (ptr, _) = self.vars[name];
                self.builder.build_store(ptr, v).unwrap();
            }
            StmtKind::Print(e) => {
                let (v, ty) = self.gen_expr(e);
                let printf = self.module.get_function("printf").unwrap();
                match ty {
                    Type::Int => {
                        let fmt = self.global_str("%lld\n", "fmt_int");
                        self.builder
                            .build_call(printf, &[fmt.into(), v.into()], "printf_call")
                            .unwrap();
                    }
                    Type::Float => {
                        let fmt = self.global_str("%g\n", "fmt_float");
                        self.builder
                            .build_call(printf, &[fmt.into(), v.into()], "printf_call")
                            .unwrap();
                    }
                    Type::Bool => {
                        // bool は "true" / "false" として表示する
                        let fmt = self.global_str("%s\n", "fmt_str");
                        let t = self.global_str("true", "str_true");
                        let f = self.global_str("false", "str_false");
                        let s = self
                            .builder
                            .build_select(v.into_int_value(), t, f, "boolstr")
                            .unwrap();
                        self.builder
                            .build_call(printf, &[fmt.into(), s.into()], "printf_call")
                            .unwrap();
                    }
                }
            }
            StmtKind::Return(e) => {
                let (v, _) = self.gen_expr(e);
                self.builder.build_return(Some(&v)).unwrap();
            }
            StmtKind::ExprStmt(e) => {
                self.gen_expr(e);
            }
            StmtKind::If {
                cond,
                then_body,
                else_body,
            } => {
                let (cond_val, _) = self.gen_expr(cond);
                let cond_val = cond_val.into_int_value();
                let then_bb = self.ctx.append_basic_block(function, "then");
                let else_bb = self.ctx.append_basic_block(function, "else");
                let merge_bb = self.ctx.append_basic_block(function, "ifcont");

                self.builder
                    .build_conditional_branch(cond_val, then_bb, else_bb)
                    .unwrap();

                // then 節
                self.builder.position_at_end(then_bb);
                for s in then_body {
                    self.gen_stmt(s, function);
                }
                if self.block_open() {
                    self.builder.build_unconditional_branch(merge_bb).unwrap();
                }

                // else 節
                self.builder.position_at_end(else_bb);
                for s in else_body {
                    self.gen_stmt(s, function);
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
                let (cond_val, _) = self.gen_expr(cond);
                self.builder
                    .build_conditional_branch(cond_val.into_int_value(), body_bb, end_bb)
                    .unwrap();

                self.builder.position_at_end(body_bb);
                for s in body {
                    self.gen_stmt(s, function);
                }
                if self.block_open() {
                    self.builder.build_unconditional_branch(cond_bb).unwrap();
                }

                self.builder.position_at_end(end_bb);
            }
        }
    }

    /// 式を評価し、(LLVM値, 型) を返す。型は typeck により正しさが保証されている。
    fn gen_expr(&mut self, e: &Expr) -> (BasicValueEnum<'ctx>, Type) {
        match &e.kind {
            ExprKind::Int(n) => (
                self.ctx.i64_type().const_int(*n as u64, true).into(),
                Type::Int,
            ),
            ExprKind::Float(x) => (self.ctx.f64_type().const_float(*x).into(), Type::Float),
            ExprKind::Bool(b) => (
                self.ctx.bool_type().const_int(u64::from(*b), false).into(),
                Type::Bool,
            ),
            ExprKind::Var(name) => {
                let (ptr, ty) = self.vars[name];
                let v = self
                    .builder
                    .build_load(self.basic_ty(ty), ptr, name)
                    .unwrap();
                (v, ty)
            }
            ExprKind::Unary { op, expr } => {
                let (v, ty) = self.gen_expr(expr);
                match op {
                    UnOp::Neg if ty == Type::Float => (
                        self.builder
                            .build_float_neg(v.into_float_value(), "fneg")
                            .unwrap()
                            .into(),
                        Type::Float,
                    ),
                    UnOp::Neg => (
                        self.builder
                            .build_int_neg(v.into_int_value(), "neg")
                            .unwrap()
                            .into(),
                        Type::Int,
                    ),
                    UnOp::Not => (
                        self.builder
                            .build_not(v.into_int_value(), "not")
                            .unwrap()
                            .into(),
                        Type::Bool,
                    ),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinOp::And | BinOp::Or => self.gen_logical(*op, lhs, rhs),
                _ => {
                    let (l, lty) = self.gen_expr(lhs);
                    let (r, _) = self.gen_expr(rhs);
                    self.gen_arith_or_cmp(*op, l, r, lty)
                }
            },
            ExprKind::Call { name, args } => {
                let function = self.module.get_function(name).unwrap();
                let mut argvals: Vec<BasicMetadataValueEnum> = Vec::with_capacity(args.len());
                for a in args {
                    let (av, _) = self.gen_expr(a);
                    argvals.push(av.into());
                }
                let call = self.builder.build_call(function, &argvals, "call").unwrap();
                let v = call.try_as_basic_value().unwrap_basic();
                (v, self.fn_rets[name])
            }
        }
    }

    /// 算術・比較演算（論理を除く二項演算）。`ty` は両辺の型（typeck が一致を保証）。
    fn gen_arith_or_cmp(
        &self,
        op: BinOp,
        l: BasicValueEnum<'ctx>,
        r: BasicValueEnum<'ctx>,
        ty: Type,
    ) -> (BasicValueEnum<'ctx>, Type) {
        let b = &self.builder;
        if ty == Type::Float {
            let l = l.into_float_value();
            let r = r.into_float_value();
            match op {
                BinOp::Add => (b.build_float_add(l, r, "fadd").unwrap().into(), Type::Float),
                BinOp::Sub => (b.build_float_sub(l, r, "fsub").unwrap().into(), Type::Float),
                BinOp::Mul => (b.build_float_mul(l, r, "fmul").unwrap().into(), Type::Float),
                BinOp::Div => (b.build_float_div(l, r, "fdiv").unwrap().into(), Type::Float),
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    let pred = match op {
                        BinOp::Eq => FloatPredicate::OEQ,
                        BinOp::Ne => FloatPredicate::ONE,
                        BinOp::Lt => FloatPredicate::OLT,
                        BinOp::Le => FloatPredicate::OLE,
                        BinOp::Gt => FloatPredicate::OGT,
                        BinOp::Ge => FloatPredicate::OGE,
                        _ => unreachable!(),
                    };
                    (
                        b.build_float_compare(pred, l, r, "fcmp").unwrap().into(),
                        Type::Bool,
                    )
                }
                BinOp::Mod | BinOp::And | BinOp::Or => unreachable!(),
            }
        } else {
            let l = l.into_int_value();
            let r = r.into_int_value();
            match op {
                BinOp::Add => (b.build_int_add(l, r, "add").unwrap().into(), Type::Int),
                BinOp::Sub => (b.build_int_sub(l, r, "sub").unwrap().into(), Type::Int),
                BinOp::Mul => (b.build_int_mul(l, r, "mul").unwrap().into(), Type::Int),
                BinOp::Div => (
                    b.build_int_signed_div(l, r, "div").unwrap().into(),
                    Type::Int,
                ),
                BinOp::Mod => (
                    b.build_int_signed_rem(l, r, "rem").unwrap().into(),
                    Type::Int,
                ),
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    let pred = match op {
                        BinOp::Eq => IntPredicate::EQ,
                        BinOp::Ne => IntPredicate::NE,
                        BinOp::Lt => IntPredicate::SLT,
                        BinOp::Le => IntPredicate::SLE,
                        BinOp::Gt => IntPredicate::SGT,
                        BinOp::Ge => IntPredicate::SGE,
                        _ => unreachable!(),
                    };
                    (
                        b.build_int_compare(pred, l, r, "cmp").unwrap().into(),
                        Type::Bool,
                    )
                }
                BinOp::And | BinOp::Or => unreachable!(),
            }
        }
    }

    /// 短絡評価する論理演算 `&&` / `||`。両辺は bool（typeck が保証）。
    fn gen_logical(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> (BasicValueEnum<'ctx>, Type) {
        let (l, _) = self.gen_expr(lhs);
        let l = l.into_int_value();

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
        let (r, _) = self.gen_expr(rhs);
        let r = r.into_int_value();
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
        (phi.as_basic_value(), Type::Bool)
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

    /// LLVM の最適化パスをモジュールに適用する（level 0 は何もしない）。
    /// `default<On>` パイプライン（mem2reg・インライン化・定数畳み込み等）を走らせる。
    pub fn optimize(&self, level: u8) -> Result<(), String> {
        if level == 0 {
            return Ok(());
        }
        let tm = self.host_target_machine()?;
        self.module
            .run_passes(
                &format!("default<O{}>", level),
                &tm,
                PassBuilderOptions::create(),
            )
            .map_err(|e| e.to_string())
    }

    /// ホスト向けの TargetMachine を作る（最適化とオブジェクト出力で共用）。
    fn host_target_machine(&self) -> Result<TargetMachine, String> {
        Target::initialize_all(&InitializationConfig::default());
        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
        target
            .create_target_machine(
                &triple,
                "generic",
                "",
                OptimizationLevel::Default,
                RelocMode::PIC,
                CodeModel::Default,
            )
            .ok_or_else(|| "ターゲットマシンを作成できません".to_string())
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
        let tm = self.host_target_machine()?;

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
