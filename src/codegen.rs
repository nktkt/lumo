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

use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, StructType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, PointerValue,
};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel};

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::types::{intern, Type};

pub struct CodeGen<'ctx> {
    ctx: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    /// レキシカルスコープのスタック。各層が 変数名 -> (alloca, 型)。
    /// typeck と同じ入れ子規則で push/pop し、参照は内→外で解決する。
    scopes: Vec<HashMap<String, (PointerValue<'ctx>, Type)>>,
    /// 関数名 -> 戻り値の型（呼び出し式の型を知るため）
    fn_rets: HashMap<String, Type>,
    /// 生成済みのグローバル文字列（書式・"true"/"false" など）をキャッシュ
    strings: HashMap<&'static str, PointerValue<'ctx>>,
    /// 入れ子ループの (continue先, break先) ブロックのスタック
    loop_stack: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,
    /// 構造体名 -> (LLVM構造体型, フィールド定義順の (名前, 型))
    structs: HashMap<String, (StructType<'ctx>, Vec<(String, Type)>)>,
}

impl<'ctx> CodeGen<'ctx> {
    pub fn new(ctx: &'ctx Context, name: &str) -> Self {
        CodeGen {
            ctx,
            module: ctx.create_module(name),
            builder: ctx.create_builder(),
            scopes: Vec::new(),
            fn_rets: HashMap::new(),
            strings: HashMap::new(),
            loop_stack: Vec::new(),
            structs: HashMap::new(),
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// 最内スコープに変数を束縛する（外側を隠すシャドーイングを許す）。
    fn declare_var(&mut self, name: &str, ptr: PointerValue<'ctx>, ty: Type) {
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), (ptr, ty));
    }

    /// 内側のスコープから順に変数を探す。
    fn lookup_var(&self, name: &str) -> (PointerValue<'ctx>, Type) {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())
            .expect("typeck guarantees the variable exists")
    }

    /// Lumo の型に対応する LLVM の基本型。
    /// int=i64, bool=i1, float=f64, string/array=ポインタ。
    fn basic_ty(&self, ty: Type) -> BasicTypeEnum<'ctx> {
        match ty {
            Type::Int => self.ctx.i64_type().into(),
            Type::Bool => self.ctx.bool_type().into(),
            Type::Float => self.ctx.f64_type().into(),
            Type::Str | Type::Array(_) | Type::Struct(_) | Type::Null => {
                self.ctx.ptr_type(AddressSpace::default()).into()
            }
        }
    }

    /// 型のゼロ値（関数末尾まで return が無かったときの既定戻り値に使う）
    fn zero_of(&self, ty: Type) -> BasicValueEnum<'ctx> {
        match ty {
            Type::Int => self.ctx.i64_type().const_int(0, false).into(),
            Type::Bool => self.ctx.bool_type().const_int(0, false).into(),
            Type::Float => self.ctx.f64_type().const_float(0.0).into(),
            Type::Str | Type::Array(_) | Type::Struct(_) | Type::Null => self
                .ctx
                .ptr_type(AddressSpace::default())
                .const_null()
                .into(),
        }
    }

    pub fn compile(&mut self, program: &Program) -> Result<(), Diagnostic> {
        // 1) ランタイム（printf と、文字列ヒープ用の libc 関数・lumo_alloc）を用意する
        self.declare_runtime();

        // 2) 構造体の LLVM 型を登録する（フィールドは basic_ty。構造体フィールドは
        //    ポインタなので相互参照・再帰構造も問題ない）。
        for s in &program.structs {
            let field_types: Vec<BasicTypeEnum> =
                s.fields.iter().map(|f| self.basic_ty(f.ty)).collect();
            let st = self.ctx.struct_type(&field_types, false);
            let fields = s.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
            self.structs.insert(s.name.clone(), (st, fields));
        }

        // 3) すべての関数シグネチャを先に宣言する（前方参照・相互再帰のため）。
        //    typeck が重複や main の存在を確認済みなのでここでは検査しない。
        for f in &program.funcs {
            let param_types: Vec<BasicMetadataTypeEnum> = f
                .params
                .iter()
                .map(|p| self.basic_ty(p.ty).into())
                .collect();
            let fn_type = self.basic_ty(f.ret).fn_type(&param_types, false);
            self.module.add_function(&f.name, fn_type, None);
            self.fn_rets.insert(f.name.clone(), f.ret);
        }

        // 4) 各関数の本体を生成する
        for f in &program.funcs {
            self.gen_function(f);
        }

        // 5) 生成したIRの整合性を検証する（コンパイラ側のバグ検出用）
        if self.module.verify().is_err() {
            return Err(Diagnostic::error(format!(
                "内部エラー: 生成したLLVM IRが不正です\n{}",
                self.ir_string()
            )));
        }
        Ok(())
    }

    fn declare_runtime(&self) {
        let i32t = self.ctx.i32_type();
        let i64t = self.ctx.i64_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());

        // int printf(char*, ...) — 可変長引数なので最後の引数を true にする
        let printf_ty = i32t.fn_type(&[ptr.into()], true);
        self.module
            .add_function("printf", printf_ty, Some(Linkage::External));

        // 文字列ヒープ操作に使う libc 関数
        let ext = Some(Linkage::External);
        self.module
            .add_function("malloc", ptr.fn_type(&[i64t.into()], false), ext);
        self.module
            .add_function("strlen", i64t.fn_type(&[ptr.into()], false), ext);
        self.module
            .add_function("strcpy", ptr.fn_type(&[ptr.into(), ptr.into()], false), ext);
        self.module
            .add_function("strcat", ptr.fn_type(&[ptr.into(), ptr.into()], false), ext);
        self.module.add_function(
            "strcmp",
            i32t.fn_type(&[ptr.into(), ptr.into()], false),
            ext,
        );
        // int snprintf(char* buf, i64 size, char* fmt, ...) — str() の数値→文字列に使う
        self.module.add_function(
            "snprintf",
            i32t.fn_type(&[ptr.into(), i64t.into(), ptr.into()], true),
            ext,
        );

        // ヒープ確保のチョークポイント。今は malloc を呼ぶだけ（回収なし）。
        // 将来ここを arena/region アロケータに差し替える（RFC 0001）。
        let alloc =
            self.module
                .add_function("lumo_alloc", ptr.fn_type(&[i64t.into()], false), None);
        let bb = self.ctx.append_basic_block(alloc, "entry");
        self.builder.position_at_end(bb);
        let size = alloc.get_nth_param(0).unwrap();
        let malloc = self.module.get_function("malloc").unwrap();
        let mem = self
            .builder
            .build_call(malloc, &[size.into()], "mem")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&mem)).unwrap();

        // ランタイム異常終了ハンドラ: stderr にメッセージを書いて exit(101) する。
        // void lumo_panic(char* msg, i64 len)。境界チェックや null チェックが使う。
        // i64 write(i32 fd, char* buf, i64 count) と void exit(i32) を libc から使う。
        self.module.add_function(
            "write",
            i64t.fn_type(&[i32t.into(), ptr.into(), i64t.into()], false),
            ext,
        );
        let void = self.ctx.void_type();
        self.module
            .add_function("exit", void.fn_type(&[i32t.into()], false), ext);

        let panic = self.module.add_function(
            "lumo_panic",
            void.fn_type(&[ptr.into(), i64t.into()], false),
            None,
        );
        let fb = self.ctx.append_basic_block(panic, "entry");
        self.builder.position_at_end(fb);
        let msg_ptr = panic.get_nth_param(0).unwrap();
        let len = panic.get_nth_param(1).unwrap();
        let two = i32t.const_int(2, false); // fd 2 = stderr
        let write_fn = self.module.get_function("write").unwrap();
        self.builder
            .build_call(write_fn, &[two.into(), msg_ptr.into(), len.into()], "")
            .unwrap();
        let exit_fn = self.module.get_function("exit").unwrap();
        self.builder
            .build_call(exit_fn, &[i32t.const_int(101, false).into()], "")
            .unwrap();
        self.builder.build_unreachable().unwrap();
    }

    /// `lumo_panic(msg, len)` を呼んで unreachable で締める（現在のブロックを終端する）。
    fn panic(&mut self, msg: &'static str, name: &'static str) {
        let m = self.global_str(msg, name);
        let len = self.ctx.i64_type().const_int(msg.len() as u64, false);
        let panic_fn = self.module.get_function("lumo_panic").unwrap();
        self.builder
            .build_call(panic_fn, &[m.into(), len.into()], "")
            .unwrap();
        self.builder.build_unreachable().unwrap();
    }

    /// ポインタが null なら "null reference" で異常終了するチェックを差し込む。
    fn null_check(&mut self, p: PointerValue<'ctx>) {
        let isnull = self.builder.build_is_null(p, "isnull").unwrap();
        let function = self.cur_function();
        let fail_bb = self.ctx.append_basic_block(function, "null.fail");
        let ok_bb = self.ctx.append_basic_block(function, "null.ok");
        self.builder
            .build_conditional_branch(isnull, fail_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(fail_bb);
        self.panic("lumo: null reference\n", "null_msg");
        self.builder.position_at_end(ok_bb);
    }

    fn gen_function(&mut self, f: &Function) {
        let function = self.module.get_function(&f.name).unwrap();
        let entry = self.ctx.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        // 関数スコープ（引数とトップレベルのローカル）を1層だけ用意する
        self.scopes = vec![HashMap::new()];

        // 仮引数をスタックにコピーして、ローカル変数として扱えるようにする
        for (i, p) in f.params.iter().enumerate() {
            let param = function.get_nth_param(i as u32).unwrap();
            let alloca = self
                .builder
                .build_alloca(self.basic_ty(p.ty), &p.name)
                .unwrap();
            self.builder.build_store(alloca, param).unwrap();
            self.declare_var(&p.name, alloca, p.ty);
        }

        self.gen_block(&f.body, function);

        // 明示的な return が無いまま関数末尾に達したら戻り値型のゼロを返す
        if self.block_open() {
            let zero = self.zero_of(f.ret);
            self.builder.build_return(Some(&zero)).unwrap();
        }
    }

    /// 文の並びを生成する。終端命令(return/break/continue)が出たら以降は
    /// 到達不能なので生成しない（終端済みブロックへの命令追加を防ぐ）。
    fn gen_block(&mut self, stmts: &[Stmt], function: FunctionValue<'ctx>) {
        for s in stmts {
            if !self.block_open() {
                break;
            }
            self.gen_stmt(s, function);
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
            StmtKind::Let { name, ty, value } => {
                let (v, vty) = self.gen_expr(value);
                // 型注釈があればそれを、無ければ初期値の型を変数の型にする
                // (null を参照型の変数へ入れる場合でも、どちらもポインタなので store 可)
                let decl = ty.unwrap_or(vty);
                let alloca = self
                    .builder
                    .build_alloca(self.basic_ty(decl), name)
                    .unwrap();
                self.declare_var(name, alloca, decl);
                self.builder.build_store(alloca, v).unwrap();
            }
            StmtKind::Assign { target, value } => {
                let (v, _) = self.gen_expr(value);
                match &target.kind {
                    ExprKind::Var(name) => {
                        let (ptr, _) = self.lookup_var(name);
                        self.builder.build_store(ptr, v).unwrap();
                    }
                    ExprKind::Index { array, index } => {
                        let (arr, _) = self.gen_expr(array);
                        let addr = self.elem_addr(arr.into_pointer_value(), index);
                        self.builder.build_store(addr, v).unwrap();
                    }
                    ExprKind::Field { obj, field } => {
                        let (ov, oty) = self.gen_expr(obj);
                        let addr = self.field_addr(ov.into_pointer_value(), oty, field);
                        self.builder.build_store(addr, v).unwrap();
                    }
                    _ => unreachable!("typeck restricts assignment targets"),
                }
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
                    Type::Str => {
                        let fmt = self.global_str("%s\n", "fmt_str");
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
                    Type::Array(_) | Type::Struct(_) | Type::Null => {
                        unreachable!("typeck forbids printing arrays/structs/null")
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
                self.push_scope();
                self.gen_block(then_body, function);
                self.pop_scope();
                if self.block_open() {
                    self.builder.build_unconditional_branch(merge_bb).unwrap();
                }

                // else 節
                self.builder.position_at_end(else_bb);
                self.push_scope();
                self.gen_block(else_body, function);
                self.pop_scope();
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

                // continue は条件へ、break は末尾へ
                self.builder.position_at_end(body_bb);
                self.loop_stack.push((cond_bb, end_bb));
                self.push_scope();
                self.gen_block(body, function);
                self.pop_scope();
                self.loop_stack.pop();
                if self.block_open() {
                    self.builder.build_unconditional_branch(cond_bb).unwrap();
                }

                self.builder.position_at_end(end_bb);
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
                    self.gen_stmt(init, function);
                }
                let cond_bb = self.ctx.append_basic_block(function, "for.cond");
                let body_bb = self.ctx.append_basic_block(function, "for.body");
                let step_bb = self.ctx.append_basic_block(function, "for.step");
                let end_bb = self.ctx.append_basic_block(function, "for.end");

                self.builder.build_unconditional_branch(cond_bb).unwrap();

                self.builder.position_at_end(cond_bb);
                let (cond_val, _) = self.gen_expr(cond);
                self.builder
                    .build_conditional_branch(cond_val.into_int_value(), body_bb, end_bb)
                    .unwrap();

                // continue は step へ、break は末尾へ
                self.builder.position_at_end(body_bb);
                self.loop_stack.push((step_bb, end_bb));
                self.push_scope();
                self.gen_block(body, function);
                self.pop_scope();
                self.loop_stack.pop();
                if self.block_open() {
                    self.builder.build_unconditional_branch(step_bb).unwrap();
                }

                self.builder.position_at_end(step_bb);
                if let Some(step) = step {
                    self.gen_stmt(step, function);
                }
                self.builder.build_unconditional_branch(cond_bb).unwrap();

                self.builder.position_at_end(end_bb);
                self.pop_scope(); // for 自体のスコープ
            }
            StmtKind::Break => {
                let (_, brk) = *self.loop_stack.last().unwrap();
                self.builder.build_unconditional_branch(brk).unwrap();
            }
            StmtKind::Continue => {
                let (cont, _) = *self.loop_stack.last().unwrap();
                self.builder.build_unconditional_branch(cont).unwrap();
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
            ExprKind::Str(s) => {
                // 文字列リテラルは NUL 終端のグローバル定数として置き、そのポインタを値にする
                let g = self
                    .builder
                    .build_global_string_ptr(s, "strlit")
                    .unwrap()
                    .as_pointer_value();
                (g.into(), Type::Str)
            }
            ExprKind::Null => (
                self.ctx
                    .ptr_type(AddressSpace::default())
                    .const_null()
                    .into(),
                Type::Null,
            ),
            ExprKind::Var(name) => {
                let (ptr, ty) = self.lookup_var(name);
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
                    let (r, rty) = self.gen_expr(rhs);
                    // 参照 vs null の == / != はポインタ比較（ptrtoint して整数比較）
                    if matches!(op, BinOp::Eq | BinOp::Ne)
                        && (lty == Type::Null || rty == Type::Null)
                    {
                        let i64t = self.ctx.i64_type();
                        let li = self
                            .builder
                            .build_ptr_to_int(l.into_pointer_value(), i64t, "l2i")
                            .unwrap();
                        let ri = self
                            .builder
                            .build_ptr_to_int(r.into_pointer_value(), i64t, "r2i")
                            .unwrap();
                        let pred = if matches!(op, BinOp::Eq) {
                            IntPredicate::EQ
                        } else {
                            IntPredicate::NE
                        };
                        let res = self
                            .builder
                            .build_int_compare(pred, li, ri, "refcmp")
                            .unwrap();
                        (res.into(), Type::Bool)
                    } else {
                        self.gen_arith_or_cmp(*op, l, r, lty)
                    }
                }
            },
            ExprKind::Call { name, args } if name == "int" => {
                // float -> int は切り捨て、int -> int は恒等
                let (v, ty) = self.gen_expr(&args[0]);
                if ty == Type::Float {
                    let i = self
                        .builder
                        .build_float_to_signed_int(
                            v.into_float_value(),
                            self.ctx.i64_type(),
                            "toint",
                        )
                        .unwrap();
                    (i.into(), Type::Int)
                } else {
                    (v, Type::Int)
                }
            }
            ExprKind::Call { name, args } if name == "float" => {
                // int -> float、float -> float は恒等
                let (v, ty) = self.gen_expr(&args[0]);
                if ty == Type::Int {
                    let f = self
                        .builder
                        .build_signed_int_to_float(
                            v.into_int_value(),
                            self.ctx.f64_type(),
                            "tofloat",
                        )
                        .unwrap();
                    (f.into(), Type::Float)
                } else {
                    (v, Type::Float)
                }
            }
            ExprKind::Call { name, args } if name == "len" => {
                // string は strlen、配列は先頭ヘッダの i64 を読む
                let (v, ty) = self.gen_expr(&args[0]);
                let ptr = v.into_pointer_value();
                match ty {
                    Type::Str => {
                        let strlen = self.module.get_function("strlen").unwrap();
                        let n = self
                            .builder
                            .build_call(strlen, &[ptr.into()], "len")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic();
                        (n, Type::Int)
                    }
                    _ => {
                        let n = self
                            .builder
                            .build_load(self.ctx.i64_type(), ptr, "len")
                            .unwrap();
                        (n, Type::Int)
                    }
                }
            }
            ExprKind::Call { name, args } if name == "str" => {
                let (v, ty) = self.gen_expr(&args[0]);
                match ty {
                    // string はそのまま
                    Type::Str => (v, Type::Str),
                    // bool は "true"/"false" を選ぶ
                    Type::Bool => {
                        let t = self.global_str("true", "str_true");
                        let f = self.global_str("false", "str_false");
                        let s = self
                            .builder
                            .build_select(v.into_int_value(), t, f, "boolstr")
                            .unwrap();
                        (s, Type::Str)
                    }
                    // int/float は snprintf でヒープバッファに書き出す
                    _ => {
                        let i64t = self.ctx.i64_type();
                        let cap = i64t.const_int(32, false);
                        let alloc = self.module.get_function("lumo_alloc").unwrap();
                        let buf = self
                            .builder
                            .build_call(alloc, &[cap.into()], "strbuf")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_pointer_value();
                        let fmt = if ty == Type::Float {
                            self.global_str("%g", "fmt_float_g")
                        } else {
                            self.global_str("%lld", "fmt_int_d")
                        };
                        let snprintf = self.module.get_function("snprintf").unwrap();
                        self.builder
                            .build_call(
                                snprintf,
                                &[buf.into(), cap.into(), fmt.into(), v.into()],
                                "",
                            )
                            .unwrap();
                        (buf.into(), Type::Str)
                    }
                }
            }
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
            ExprKind::Array(elems) => {
                // ヒープに [長さ i64][8byteスロット×N] を確保する
                let i64t = self.ctx.i64_type();
                let n = elems.len() as u64;
                let size = i64t.const_int(8 * (n + 1), false);
                let alloc = self.module.get_function("lumo_alloc").unwrap();
                let buf = self
                    .builder
                    .build_call(alloc, &[size.into()], "arr")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                // 先頭に長さを格納
                self.builder
                    .build_store(buf, i64t.const_int(n, false))
                    .unwrap();
                // 各要素を 8 + 8*i バイト目へ
                let mut elem_type = Type::Int;
                for (i, el) in elems.iter().enumerate() {
                    let (val, ty) = self.gen_expr(el);
                    if i == 0 {
                        elem_type = ty;
                    }
                    let off = i64t.const_int(8 + 8 * (i as u64), false);
                    let addr = unsafe {
                        self.builder
                            .build_in_bounds_gep(self.ctx.i8_type(), buf, &[off], "slot")
                            .unwrap()
                    };
                    self.builder.build_store(addr, val).unwrap();
                }
                (buf.into(), Type::Array(elem_type.as_elem().unwrap()))
            }
            ExprKind::Index { array, index } => {
                let (arr, arr_ty) = self.gen_expr(array);
                let elem = match arr_ty {
                    Type::Array(e) => e,
                    _ => unreachable!("typeck guarantees an array here"),
                };
                let addr = self.elem_addr(arr.into_pointer_value(), index);
                let v = self
                    .builder
                    .build_load(self.basic_ty(elem.to_type()), addr, "idx")
                    .unwrap();
                (v, elem.to_type())
            }
            ExprKind::StructLit { name, fields } => {
                let (st, def) = self.structs[name].clone();
                let size = st.size_of().unwrap();
                let alloc = self.module.get_function("lumo_alloc").unwrap();
                let obj = self
                    .builder
                    .build_call(alloc, &[size.into()], "obj")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                // 各フィールドを定義順のインデックスへ格納する
                for (idx, (fname, _)) in def.iter().enumerate() {
                    let init = fields.iter().find(|fi| &fi.name == fname).unwrap();
                    let (val, _) = self.gen_expr(&init.value);
                    let addr = self
                        .builder
                        .build_struct_gep(st, obj, idx as u32, "field")
                        .unwrap();
                    self.builder.build_store(addr, val).unwrap();
                }
                (obj.into(), Type::Struct(intern(name)))
            }
            ExprKind::Field { obj, field } => {
                let (ov, oty) = self.gen_expr(obj);
                let fty = self.field_type(oty, field);
                let addr = self.field_addr(ov.into_pointer_value(), oty, field);
                let v = self
                    .builder
                    .build_load(self.basic_ty(fty), addr, "fld")
                    .unwrap();
                (v, fty)
            }
        }
    }

    /// 構造体ポインタ `obj` の `field` のアドレスを GEP で計算する。
    fn field_addr(
        &mut self,
        obj: PointerValue<'ctx>,
        obj_ty: Type,
        field: &str,
    ) -> PointerValue<'ctx> {
        // 構造体が null でないことを先に確認する
        self.null_check(obj);
        let sname = match obj_ty {
            Type::Struct(n) => n,
            _ => unreachable!("typeck guarantees a struct here"),
        };
        let (st, def) = &self.structs[sname];
        let idx = def.iter().position(|(n, _)| n == field).unwrap();
        self.builder
            .build_struct_gep(*st, obj, idx as u32, "field")
            .unwrap()
    }

    /// 構造体型 `obj_ty` の `field` の型を返す。
    fn field_type(&self, obj_ty: Type, field: &str) -> Type {
        let sname = match obj_ty {
            Type::Struct(n) => n,
            _ => unreachable!("typeck guarantees a struct here"),
        };
        let (_, def) = &self.structs[sname];
        def.iter().find(|(n, _)| n == field).unwrap().1
    }

    /// 算術・比較演算（論理を除く二項演算）。`ty` は両辺の型（typeck が一致を保証）。
    fn gen_arith_or_cmp(
        &self,
        op: BinOp,
        l: BasicValueEnum<'ctx>,
        r: BasicValueEnum<'ctx>,
        ty: Type,
    ) -> (BasicValueEnum<'ctx>, Type) {
        if ty == Type::Str {
            return self.gen_str_binop(op, l, r);
        }
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

    /// 文字列の二項演算: `+`(連結) と `==`/`!=`(等価)。typeck がこの3つに限定済み。
    fn gen_str_binop(
        &self,
        op: BinOp,
        l: BasicValueEnum<'ctx>,
        r: BasicValueEnum<'ctx>,
    ) -> (BasicValueEnum<'ctx>, Type) {
        let a = l.into_pointer_value();
        let b = r.into_pointer_value();
        let strlen = self.module.get_function("strlen").unwrap();
        match op {
            BinOp::Add => {
                // 連結: lumo_alloc(len(a)+len(b)+1) して strcpy + strcat
                let la = self
                    .builder
                    .build_call(strlen, &[a.into()], "la")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let lb = self
                    .builder
                    .build_call(strlen, &[b.into()], "lb")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let one = self.ctx.i64_type().const_int(1, false);
                let sum = self.builder.build_int_add(la, lb, "lab").unwrap();
                let size = self.builder.build_int_add(sum, one, "size").unwrap();

                let alloc = self.module.get_function("lumo_alloc").unwrap();
                let buf = self
                    .builder
                    .build_call(alloc, &[size.into()], "buf")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();

                let strcpy = self.module.get_function("strcpy").unwrap();
                self.builder
                    .build_call(strcpy, &[buf.into(), a.into()], "cpy")
                    .unwrap();
                let strcat = self.module.get_function("strcat").unwrap();
                self.builder
                    .build_call(strcat, &[buf.into(), b.into()], "cat")
                    .unwrap();
                (buf.into(), Type::Str)
            }
            BinOp::Eq | BinOp::Ne => {
                let strcmp = self.module.get_function("strcmp").unwrap();
                let cmp = self
                    .builder
                    .build_call(strcmp, &[a.into(), b.into()], "scmp")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let zero = self.ctx.i32_type().const_int(0, false);
                let pred = if matches!(op, BinOp::Eq) {
                    IntPredicate::EQ
                } else {
                    IntPredicate::NE
                };
                let res = self
                    .builder
                    .build_int_compare(pred, cmp, zero, "streq")
                    .unwrap();
                (res.into(), Type::Bool)
            }
            _ => unreachable!(),
        }
    }

    /// 配列の i 番目スロットのアドレスを計算する。
    /// レイアウトは [長さ i64][8byte スロット×N] なので、要素は 8 + 8*i バイト目。
    fn elem_addr(&mut self, base: PointerValue<'ctx>, index: &Expr) -> PointerValue<'ctx> {
        let i64t = self.ctx.i64_type();
        // 配列が null でないことを先に確認する
        self.null_check(base);
        let (idx, _) = self.gen_expr(index);
        let idx = idx.into_int_value();

        // 境界チェック: 符号なし比較 idx >= len なら範囲外（負の添字も巨大値として弾く）
        let len = self
            .builder
            .build_load(i64t, base, "len")
            .unwrap()
            .into_int_value();
        let oob = self
            .builder
            .build_int_compare(IntPredicate::UGE, idx, len, "oob")
            .unwrap();
        let function = self.cur_function();
        let fail_bb = self.ctx.append_basic_block(function, "oob.fail");
        let ok_bb = self.ctx.append_basic_block(function, "oob.ok");
        self.builder
            .build_conditional_branch(oob, fail_bb, ok_bb)
            .unwrap();

        self.builder.position_at_end(fail_bb);
        self.panic("lumo: array index out of bounds\n", "oob_msg");

        self.builder.position_at_end(ok_bb);
        let eight = i64t.const_int(8, false);
        let scaled = self.builder.build_int_mul(idx, eight, "off.mul").unwrap();
        let off = self.builder.build_int_add(scaled, eight, "off").unwrap();
        unsafe {
            self.builder
                .build_in_bounds_gep(self.ctx.i8_type(), base, &[off], "slot")
                .unwrap()
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
