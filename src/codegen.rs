//! コード生成 (Code Generation): AST を LLVM IR に変換する。
//!
//! 設計を単純にするため、すべての値は i64 として扱う。
//! 比較演算は LLVM では i1 を返すので、結果を i64 にゼロ拡張して統一する。
//! 変数はすべて alloca（スタック領域）に置き、load/store で読み書きする
//! （最適化パスの mem2reg がレジスタに昇格してくれる）。

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
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate, OptimizationLevel};

use crate::ast::*;

pub struct CodeGen<'ctx> {
    ctx: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    /// 現在の関数内の変数名 -> スタック上のアドレス
    vars: HashMap<String, PointerValue<'ctx>>,
    /// print 用の書式文字列 "%lld\n"（最初の print 時に1度だけ作る）
    fmt: Option<PointerValue<'ctx>>,
}

impl<'ctx> CodeGen<'ctx> {
    pub fn new(ctx: &'ctx Context, name: &str) -> Self {
        CodeGen {
            ctx,
            module: ctx.create_module(name),
            builder: ctx.create_builder(),
            vars: HashMap::new(),
            fmt: None,
        }
    }

    fn i64t(&self) -> inkwell::types::IntType<'ctx> {
        self.ctx.i64_type()
    }

    pub fn compile(&mut self, program: &Program) -> Result<(), String> {
        // 1) C の printf を宣言（print の実装に使う）
        self.declare_printf();

        // 2) すべての関数シグネチャを先に宣言する（前方参照・相互再帰のため）
        for f in program {
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
            return Err("エントリポイント `fn main()` が見つかりません".to_string());
        }

        // 4) 生成したIRの整合性を検証する
        if self.module.verify().is_err() {
            return Err(format!("生成したLLVM IRが不正です:\n{}", self.ir_string()));
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

    fn gen_function(&mut self, f: &Function) -> Result<(), String> {
        let function = self.module.get_function(&f.name).unwrap();
        let entry = self.ctx.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        self.vars.clear();

        // 仮引数をスタックにコピーして、ローカル変数として扱えるようにする
        for (i, pname) in f.params.iter().enumerate() {
            let param = function.get_nth_param(i as u32).unwrap().into_int_value();
            let alloca = self.builder.build_alloca(self.i64t(), pname).unwrap();
            self.builder.build_store(alloca, param).unwrap();
            self.vars.insert(pname.clone(), alloca);
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

    fn gen_stmt(&mut self, stmt: &Stmt, function: FunctionValue<'ctx>) -> Result<(), String> {
        match stmt {
            Stmt::Let { name, value } => {
                let v = self.gen_expr(value)?;
                let alloca = match self.vars.get(name) {
                    Some(p) => *p,
                    None => {
                        let a = self.builder.build_alloca(self.i64t(), name).unwrap();
                        self.vars.insert(name.clone(), a);
                        a
                    }
                };
                self.builder.build_store(alloca, v).unwrap();
            }
            Stmt::Assign { name, value } => {
                let v = self.gen_expr(value)?;
                let ptr = *self
                    .vars
                    .get(name)
                    .ok_or_else(|| format!("未定義の変数への代入: {}", name))?;
                self.builder.build_store(ptr, v).unwrap();
            }
            Stmt::Print(e) => {
                let v = self.gen_expr(e)?;
                let fmt = self.fmt_ptr();
                let printf = self.module.get_function("printf").unwrap();
                let args: Vec<BasicMetadataValueEnum> = vec![fmt.into(), v.into()];
                self.builder.build_call(printf, &args, "printf_call").unwrap();
            }
            Stmt::Return(e) => {
                let v = self.gen_expr(e)?;
                self.builder.build_return(Some(&v)).unwrap();
            }
            Stmt::ExprStmt(e) => {
                self.gen_expr(e)?;
            }
            Stmt::If {
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
            Stmt::While { cond, body } => {
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

    /// 条件式を評価し、i64 の結果を「0 でなければ真」として i1 に変換する
    fn gen_cond(&mut self, e: &Expr) -> Result<IntValue<'ctx>, String> {
        let v = self.gen_expr(e)?;
        let zero = self.i64t().const_int(0, false);
        Ok(self
            .builder
            .build_int_compare(IntPredicate::NE, v, zero, "cond")
            .unwrap())
    }

    fn gen_expr(&mut self, e: &Expr) -> Result<IntValue<'ctx>, String> {
        match e {
            Expr::Int(n) => Ok(self.i64t().const_int(*n as u64, true)),
            Expr::Var(name) => {
                let ptr = *self
                    .vars
                    .get(name)
                    .ok_or_else(|| format!("未定義の変数: {}", name))?;
                Ok(self
                    .builder
                    .build_load(self.i64t(), ptr, name)
                    .unwrap()
                    .into_int_value())
            }
            Expr::Call { name, args } => {
                let function = self
                    .module
                    .get_function(name)
                    .ok_or_else(|| format!("未定義の関数: {}", name))?;
                let expected = function.count_params() as usize;
                if expected != args.len() {
                    return Err(format!(
                        "関数 {} は引数 {} 個ですが {} 個渡されました",
                        name,
                        expected,
                        args.len()
                    ));
                }
                let mut argvals: Vec<BasicMetadataValueEnum> = Vec::with_capacity(args.len());
                for a in args {
                    argvals.push(self.gen_expr(a)?.into());
                }
                let call = self.builder.build_call(function, &argvals, "call").unwrap();
                Ok(call.try_as_basic_value().unwrap_basic().into_int_value())
            }
            Expr::Binary { op, lhs, rhs } => {
                let l = self.gen_expr(lhs)?;
                let r = self.gen_expr(rhs)?;
                let b = &self.builder;
                let v = match op {
                    BinOp::Add => b.build_int_add(l, r, "add").unwrap(),
                    BinOp::Sub => b.build_int_sub(l, r, "sub").unwrap(),
                    BinOp::Mul => b.build_int_mul(l, r, "mul").unwrap(),
                    BinOp::Div => b.build_int_signed_div(l, r, "div").unwrap(),
                    BinOp::Mod => b.build_int_signed_rem(l, r, "rem").unwrap(),
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
                        let cmp = b.build_int_compare(pred, l, r, "cmp").unwrap();
                        // i1 -> i64 にゼロ拡張して型を統一する
                        b.build_int_z_extend(cmp, self.i64t(), "bool").unwrap()
                    }
                };
                Ok(v)
            }
        }
    }

    /// "%lld\n" のグローバル文字列を必要時に1度だけ作り、ポインタを返す
    fn fmt_ptr(&mut self) -> PointerValue<'ctx> {
        if self.fmt.is_none() {
            let g = self
                .builder
                .build_global_string_ptr("%lld\n", "fmt")
                .unwrap()
                .as_pointer_value();
            self.fmt = Some(g);
        }
        self.fmt.unwrap()
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
