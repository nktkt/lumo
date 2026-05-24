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

/// libc の `stdin` (FILE*) のシンボル名。macOS では `__stdinp`、それ以外は `stdin`。
/// ホスト向けにしかコンパイルしないので、コンパイル時の cfg で正しく選べる。
fn stdin_symbol() -> &'static str {
    if cfg!(target_os = "macos") {
        "__stdinp"
    } else {
        "stdin"
    }
}

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
            Type::Str | Type::Array(_) | Type::Struct(_) | Type::Map(_) | Type::Null => {
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
            Type::Str | Type::Array(_) | Type::Struct(_) | Type::Map(_) | Type::Null => self
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

    fn declare_runtime(&mut self) {
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
        // void* calloc(size_t n, size_t size) — map のゼロ初期化バケット配列に使う
        self.module.add_function(
            "calloc",
            ptr.fn_type(&[i64t.into(), i64t.into()], false),
            ext,
        );
        // void* realloc(void* ptr, size_t size) — 配列を伸ばす push() に使う
        self.module.add_function(
            "realloc",
            ptr.fn_type(&[ptr.into(), i64t.into()], false),
            ext,
        );
        self.module
            .add_function("strlen", i64t.fn_type(&[ptr.into()], false), ext);
        // void* memcpy(void* dst, void* src, size_t n) — substr/split/join に使う
        self.module.add_function(
            "memcpy",
            ptr.fn_type(&[ptr.into(), ptr.into(), i64t.into()], false),
            ext,
        );
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

        // 数学組み込み: LLVM intrinsic は x86-64 ではハードウェア命令に落ちる(libm 不要)。
        // pow だけは libm の関数を呼ぶ(JIT は printf 等と同様にプロセスのシンボルで解決)。
        let f64t = self.ctx.f64_type();
        let unary_f = f64t.fn_type(&[f64t.into()], false);
        let binary_f = f64t.fn_type(&[f64t.into(), f64t.into()], false);
        for name in [
            "llvm.sqrt.f64",
            "llvm.floor.f64",
            "llvm.ceil.f64",
            "llvm.fabs.f64",
        ] {
            self.module.add_function(name, unary_f, None);
        }
        for name in ["llvm.minnum.f64", "llvm.maxnum.f64"] {
            self.module.add_function(name, binary_f, None);
        }
        self.module.add_function("pow", binary_f, ext);
        // read_line() 用: char* fgets(char*, int, FILE*)、i64 strcspn(char*, char*)、
        // および stdin (FILE*) のグローバル（シンボル名は OS で異なる）。
        self.module.add_function(
            "fgets",
            ptr.fn_type(&[ptr.into(), i32t.into(), ptr.into()], false),
            ext,
        );
        self.module.add_function(
            "strcspn",
            i64t.fn_type(&[ptr.into(), ptr.into()], false),
            ext,
        );
        let stdin_g = self.module.add_global(ptr, None, stdin_symbol());
        stdin_g.set_linkage(Linkage::External);

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

        // map(連想配列)のランタイム。lumo_alloc/lumo_panic を使うのでこの後に定義する。
        self.declare_map_runtime();
        // 文字列ツールキット（substr/split/join）のランタイム。
        self.declare_string_runtime();
        // 文字列→数値パース（int()/float() の string 版、is_int/is_float）のランタイム。
        self.declare_parse_runtime();
        // ファイル I/O（read_file/write_file）のランタイム。
        self.declare_io_runtime();
        // 配列スライス（seq[lo:hi]）のランタイム。
        self.declare_array_runtime();
    }

    /// 配列ランタイム: スライス・ソート(libc qsort + 型別コンパレータ)・反転。
    fn declare_array_runtime(&mut self) {
        let i64t = self.ctx.i64_type();
        let i32t = self.ctx.i32_type();
        let f64t = self.ctx.f64_type();
        let i8t = self.ctx.i8_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());

        // --- ptr lumo_array_slice(ptr hdr, i64 lo, i64 hi): hdr[lo..hi] の新規配列 ---
        // 呼び出し側が 0<=lo<=hi<=len を保証する。スロットは 8byte 固定なので
        // memcpy 1 回で要素型を問わずコピーできる。
        let slice_fn = self.module.add_function(
            "lumo_array_slice",
            ptr.fn_type(&[ptr.into(), i64t.into(), i64t.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(slice_fn, "entry");
            let copy_bb = self.ctx.append_basic_block(slice_fn, "copy");
            let fin_bb = self.ctx.append_basic_block(slice_fn, "fin");
            let hdr = slice_fn.get_nth_param(0).unwrap().into_pointer_value();
            let lo = slice_fn.get_nth_param(1).unwrap().into_int_value();
            let hi = slice_fn.get_nth_param(2).unwrap().into_int_value();
            self.builder.position_at_end(entry);
            let alloc = self.module.get_function("lumo_alloc").unwrap();
            let memcpy = self.module.get_function("memcpy").unwrap();
            let n = self.builder.build_int_sub(hi, lo, "n").unwrap();
            let bytes = self
                .builder
                .build_int_mul(n, i64t.const_int(8, false), "bytes")
                .unwrap();
            // 新ヘッダ(24byte)を確保し len=cap=n、data を確保
            let newhdr = self
                .builder
                .build_call(alloc, &[i64t.const_int(24, false).into()], "newhdr")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let newdata = self
                .builder
                .build_call(alloc, &[bytes.into()], "newdata")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder.build_store(newhdr, n).unwrap();
            self.builder
                .build_store(self.hdr_field(newhdr, 8), n)
                .unwrap();
            self.builder
                .build_store(self.hdr_field(newhdr, 16), newdata)
                .unwrap();
            // n>0 のときだけ memcpy（null+0 の UB を避ける）
            let has = self
                .builder
                .build_int_compare(IntPredicate::SGT, n, i64t.const_zero(), "has")
                .unwrap();
            self.builder
                .build_conditional_branch(has, copy_bb, fin_bb)
                .unwrap();
            self.builder.position_at_end(copy_bb);
            let olddata = self
                .builder
                .build_load(ptr, self.hdr_field(hdr, 16), "olddata")
                .unwrap()
                .into_pointer_value();
            let off = self
                .builder
                .build_int_mul(lo, i64t.const_int(8, false), "off")
                .unwrap();
            let src = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, olddata, &[off], "src")
                    .unwrap()
            };
            self.builder
                .build_call(memcpy, &[newdata.into(), src.into(), bytes.into()], "")
                .unwrap();
            self.builder.build_unconditional_branch(fin_bb).unwrap();
            self.builder.position_at_end(fin_bb);
            self.builder.build_return(Some(&newhdr)).unwrap();
        }

        // libc: void qsort(void* base, size_t n, size_t size, int(*cmp)(const void*, const void*))
        self.module.add_function(
            "qsort",
            self.ctx
                .void_type()
                .fn_type(&[ptr.into(), i64t.into(), i64t.into(), ptr.into()], false),
            Some(Linkage::External),
        );

        // 型別コンパレータ。qsort は 8byte スロット2つ(へのポインタ)を渡す。
        // int: i64 を符号付き比較して -1/0/1 を返す。
        let cmp_int = self.module.add_function(
            "lumo_cmp_int",
            i32t.fn_type(&[ptr.into(), ptr.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(cmp_int, "entry");
            self.builder.position_at_end(entry);
            let pa = cmp_int.get_nth_param(0).unwrap().into_pointer_value();
            let pb = cmp_int.get_nth_param(1).unwrap().into_pointer_value();
            let a = self
                .builder
                .build_load(i64t, pa, "a")
                .unwrap()
                .into_int_value();
            let b = self
                .builder
                .build_load(i64t, pb, "b")
                .unwrap()
                .into_int_value();
            let lt = self
                .builder
                .build_int_compare(IntPredicate::SLT, a, b, "lt")
                .unwrap();
            let gt = self
                .builder
                .build_int_compare(IntPredicate::SGT, a, b, "gt")
                .unwrap();
            let pos = self
                .builder
                .build_select(gt, i32t.const_int(1, false), i32t.const_zero(), "pos")
                .unwrap()
                .into_int_value();
            let r = self
                .builder
                .build_select(lt, i32t.const_all_ones(), pos, "r")
                .unwrap();
            self.builder.build_return(Some(&r)).unwrap();
        }

        // float: f64 を比較して -1/0/1。
        let cmp_float = self.module.add_function(
            "lumo_cmp_float",
            i32t.fn_type(&[ptr.into(), ptr.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(cmp_float, "entry");
            self.builder.position_at_end(entry);
            let pa = cmp_float.get_nth_param(0).unwrap().into_pointer_value();
            let pb = cmp_float.get_nth_param(1).unwrap().into_pointer_value();
            let a = self
                .builder
                .build_load(f64t, pa, "a")
                .unwrap()
                .into_float_value();
            let b = self
                .builder
                .build_load(f64t, pb, "b")
                .unwrap()
                .into_float_value();
            let lt = self
                .builder
                .build_float_compare(inkwell::FloatPredicate::OLT, a, b, "lt")
                .unwrap();
            let gt = self
                .builder
                .build_float_compare(inkwell::FloatPredicate::OGT, a, b, "gt")
                .unwrap();
            let pos = self
                .builder
                .build_select(gt, i32t.const_int(1, false), i32t.const_zero(), "pos")
                .unwrap()
                .into_int_value();
            let r = self
                .builder
                .build_select(lt, i32t.const_all_ones(), pos, "r")
                .unwrap();
            self.builder.build_return(Some(&r)).unwrap();
        }

        // string: スロットは文字列ポインタ。それぞれをロードして strcmp。
        let cmp_str = self.module.add_function(
            "lumo_cmp_str",
            i32t.fn_type(&[ptr.into(), ptr.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(cmp_str, "entry");
            self.builder.position_at_end(entry);
            let pa = cmp_str.get_nth_param(0).unwrap().into_pointer_value();
            let pb = cmp_str.get_nth_param(1).unwrap().into_pointer_value();
            let sa = self
                .builder
                .build_load(ptr, pa, "sa")
                .unwrap()
                .into_pointer_value();
            let sb = self
                .builder
                .build_load(ptr, pb, "sb")
                .unwrap()
                .into_pointer_value();
            let strcmp = self.module.get_function("strcmp").unwrap();
            let r = self
                .builder
                .build_call(strcmp, &[sa.into(), sb.into()], "r")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
            self.builder.build_return(Some(&r)).unwrap();
        }

        // --- ptr lumo_array_sort(ptr hdr, ptr cmp): 昇順に並べ替えた新規配列 ---
        // 元配列をコピーしてから data ブロックを qsort する（非破壊）。
        let sort_fn = self.module.add_function(
            "lumo_array_sort",
            ptr.fn_type(&[ptr.into(), ptr.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(sort_fn, "entry");
            self.builder.position_at_end(entry);
            let hdr = sort_fn.get_nth_param(0).unwrap().into_pointer_value();
            let cmp = sort_fn.get_nth_param(1).unwrap().into_pointer_value();
            let len = self
                .builder
                .build_load(i64t, hdr, "len")
                .unwrap()
                .into_int_value();
            let slice = self.module.get_function("lumo_array_slice").unwrap();
            let copy = self
                .builder
                .build_call(
                    slice,
                    &[hdr.into(), i64t.const_zero().into(), len.into()],
                    "copy",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let data = self
                .builder
                .build_load(ptr, self.hdr_field(copy, 16), "data")
                .unwrap()
                .into_pointer_value();
            let qsort = self.module.get_function("qsort").unwrap();
            self.builder
                .build_call(
                    qsort,
                    &[
                        data.into(),
                        len.into(),
                        i64t.const_int(8, false).into(),
                        cmp.into(),
                    ],
                    "",
                )
                .unwrap();
            self.builder.build_return(Some(&copy)).unwrap();
        }

        // --- ptr lumo_array_reverse(ptr hdr): 要素を逆順にした新規配列 ---
        // コピーしてから data の 8byte スロットを両端から入れ替える。
        let rev_fn = self.module.add_function(
            "lumo_array_reverse",
            ptr.fn_type(&[ptr.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(rev_fn, "entry");
            let loop_bb = self.ctx.append_basic_block(rev_fn, "loop");
            let body_bb = self.ctx.append_basic_block(rev_fn, "body");
            let fin_bb = self.ctx.append_basic_block(rev_fn, "fin");
            self.builder.position_at_end(entry);
            let hdr = rev_fn.get_nth_param(0).unwrap().into_pointer_value();
            let len = self
                .builder
                .build_load(i64t, hdr, "len")
                .unwrap()
                .into_int_value();
            let slice = self.module.get_function("lumo_array_slice").unwrap();
            let copy = self
                .builder
                .build_call(
                    slice,
                    &[hdr.into(), i64t.const_zero().into(), len.into()],
                    "copy",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let data = self
                .builder
                .build_load(ptr, self.hdr_field(copy, 16), "data")
                .unwrap()
                .into_pointer_value();
            // i=0, j=len-1; while i<j swap slots; i++, j--
            let i_ptr = self.builder.build_alloca(i64t, "i").unwrap();
            let j_ptr = self.builder.build_alloca(i64t, "j").unwrap();
            self.builder.build_store(i_ptr, i64t.const_zero()).unwrap();
            let lenm1 = self
                .builder
                .build_int_sub(len, i64t.const_int(1, false), "lenm1")
                .unwrap();
            self.builder.build_store(j_ptr, lenm1).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            self.builder.position_at_end(loop_bb);
            let iv = self
                .builder
                .build_load(i64t, i_ptr, "i")
                .unwrap()
                .into_int_value();
            let jv = self
                .builder
                .build_load(i64t, j_ptr, "j")
                .unwrap()
                .into_int_value();
            let go = self
                .builder
                .build_int_compare(IntPredicate::SLT, iv, jv, "go")
                .unwrap();
            self.builder
                .build_conditional_branch(go, body_bb, fin_bb)
                .unwrap();
            self.builder.position_at_end(body_bb);
            let ioff = self
                .builder
                .build_int_mul(iv, i64t.const_int(8, false), "ioff")
                .unwrap();
            let joff = self
                .builder
                .build_int_mul(jv, i64t.const_int(8, false), "joff")
                .unwrap();
            let ia = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, data, &[ioff], "ia")
                    .unwrap()
            };
            let ja = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, data, &[joff], "ja")
                    .unwrap()
            };
            let vi = self
                .builder
                .build_load(i64t, ia, "vi")
                .unwrap()
                .into_int_value();
            let vj = self
                .builder
                .build_load(i64t, ja, "vj")
                .unwrap()
                .into_int_value();
            self.builder.build_store(ia, vj).unwrap();
            self.builder.build_store(ja, vi).unwrap();
            let inext = self
                .builder
                .build_int_add(iv, i64t.const_int(1, false), "inext")
                .unwrap();
            let jprev = self
                .builder
                .build_int_sub(jv, i64t.const_int(1, false), "jprev")
                .unwrap();
            self.builder.build_store(i_ptr, inext).unwrap();
            self.builder.build_store(j_ptr, jprev).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            self.builder.position_at_end(fin_bb);
            self.builder.build_return(Some(&copy)).unwrap();
        }
    }

    /// ファイル I/O のランタイム（read_file/write_file）。libc の fopen 系を使う。
    /// read_file は開けなければ null（read_line と同じ nullable string 方式）、
    /// write_file は成功で true。どちらも JIT でプロセスの libc シンボルで解決される。
    fn declare_io_runtime(&mut self) {
        let i32t = self.ctx.i32_type();
        let i64t = self.ctx.i64_type();
        let i1t = self.ctx.bool_type();
        let i8t = self.ctx.i8_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let ext = Some(Linkage::External);

        // libc: FILE* fopen(char*, char*); int fclose(FILE*);
        //       int fseek(FILE*, long, int); long ftell(FILE*);
        //       size_t fread(void*, size_t, size_t, FILE*); size_t fwrite(...);
        self.module
            .add_function("fopen", ptr.fn_type(&[ptr.into(), ptr.into()], false), ext);
        self.module
            .add_function("fclose", i32t.fn_type(&[ptr.into()], false), ext);
        self.module.add_function(
            "fseek",
            i32t.fn_type(&[ptr.into(), i64t.into(), i32t.into()], false),
            ext,
        );
        self.module
            .add_function("ftell", i64t.fn_type(&[ptr.into()], false), ext);
        self.module.add_function(
            "fread",
            i64t.fn_type(&[ptr.into(), i64t.into(), i64t.into(), ptr.into()], false),
            ext,
        );
        self.module.add_function(
            "fwrite",
            i64t.fn_type(&[ptr.into(), i64t.into(), i64t.into(), ptr.into()], false),
            ext,
        );

        // --- ptr lumo_read_file(ptr path): 全内容を string で返す。開けなければ null ---
        let read_fn =
            self.module
                .add_function("lumo_read_file", ptr.fn_type(&[ptr.into()], false), None);
        {
            let entry = self.ctx.append_basic_block(read_fn, "entry");
            let ok_bb = self.ctx.append_basic_block(read_fn, "ok");
            let fail_bb = self.ctx.append_basic_block(read_fn, "fail");
            let path = read_fn.get_nth_param(0).unwrap().into_pointer_value();
            self.builder.position_at_end(entry);
            let mode = self.global_str("rb", "io_mode_rb");
            let fopen = self.module.get_function("fopen").unwrap();
            let f = self
                .builder
                .build_call(fopen, &[path.into(), mode.into()], "f")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let is_null = self.builder.build_is_null(f, "isnull").unwrap();
            self.builder
                .build_conditional_branch(is_null, fail_bb, ok_bb)
                .unwrap();
            // fail: return null
            self.builder.position_at_end(fail_bb);
            self.builder.build_return(Some(&ptr.const_null())).unwrap();
            // ok: サイズを測り、確保して読み込む
            self.builder.position_at_end(ok_bb);
            let fseek = self.module.get_function("fseek").unwrap();
            let ftell = self.module.get_function("ftell").unwrap();
            // fseek(f, 0, SEEK_END=2)
            self.builder
                .build_call(
                    fseek,
                    &[
                        f.into(),
                        i64t.const_zero().into(),
                        i32t.const_int(2, false).into(),
                    ],
                    "",
                )
                .unwrap();
            let raw_size = self
                .builder
                .build_call(ftell, &[f.into()], "size")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            // 非シーク等で負なら 0 にする
            let neg = self
                .builder
                .build_int_compare(IntPredicate::SLT, raw_size, i64t.const_zero(), "neg")
                .unwrap();
            let size = self
                .builder
                .build_select(neg, i64t.const_zero(), raw_size, "size")
                .unwrap()
                .into_int_value();
            // fseek(f, 0, SEEK_SET=0)
            self.builder
                .build_call(
                    fseek,
                    &[f.into(), i64t.const_zero().into(), i32t.const_zero().into()],
                    "",
                )
                .unwrap();
            let alloc = self.module.get_function("lumo_alloc").unwrap();
            let cap = self
                .builder
                .build_int_add(size, i64t.const_int(1, false), "cap")
                .unwrap();
            let buf = self
                .builder
                .build_call(alloc, &[cap.into()], "buf")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let fread = self.module.get_function("fread").unwrap();
            let nread = self
                .builder
                .build_call(
                    fread,
                    &[
                        buf.into(),
                        i64t.const_int(1, false).into(),
                        size.into(),
                        f.into(),
                    ],
                    "nread",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            // buf[nread] = 0
            let end = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, buf, &[nread], "end")
                    .unwrap()
            };
            self.builder.build_store(end, i8t.const_zero()).unwrap();
            let fclose = self.module.get_function("fclose").unwrap();
            self.builder.build_call(fclose, &[f.into()], "").unwrap();
            self.builder.build_return(Some(&buf)).unwrap();
        }

        // --- i1 lumo_write_file(ptr path, ptr content): 成功で true ---
        let write_fn = self.module.add_function(
            "lumo_write_file",
            i1t.fn_type(&[ptr.into(), ptr.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(write_fn, "entry");
            let ok_bb = self.ctx.append_basic_block(write_fn, "ok");
            let fail_bb = self.ctx.append_basic_block(write_fn, "fail");
            let path = write_fn.get_nth_param(0).unwrap().into_pointer_value();
            let content = write_fn.get_nth_param(1).unwrap().into_pointer_value();
            self.builder.position_at_end(entry);
            let mode = self.global_str("wb", "io_mode_wb");
            let fopen = self.module.get_function("fopen").unwrap();
            let f = self
                .builder
                .build_call(fopen, &[path.into(), mode.into()], "f")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let is_null = self.builder.build_is_null(f, "isnull").unwrap();
            self.builder
                .build_conditional_branch(is_null, fail_bb, ok_bb)
                .unwrap();
            self.builder.position_at_end(fail_bb);
            self.builder.build_return(Some(&i1t.const_zero())).unwrap();
            self.builder.position_at_end(ok_bb);
            let strlen = self.module.get_function("strlen").unwrap();
            let n = self
                .builder
                .build_call(strlen, &[content.into()], "n")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let fwrite = self.module.get_function("fwrite").unwrap();
            let nw = self
                .builder
                .build_call(
                    fwrite,
                    &[
                        content.into(),
                        i64t.const_int(1, false).into(),
                        n.into(),
                        f.into(),
                    ],
                    "nw",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let fclose = self.module.get_function("fclose").unwrap();
            self.builder.build_call(fclose, &[f.into()], "").unwrap();
            let ok = self
                .builder
                .build_int_compare(IntPredicate::EQ, nw, n, "ok")
                .unwrap();
            self.builder.build_return(Some(&ok)).unwrap();
        }
    }

    /// 文字列→数値パースのランタイム。libc の strtol/strtod を endptr 付きで呼び、
    /// 「文字列全体を消費したか」で妥当性を判定する（前後のゴミは不正扱い）。
    /// parse は不正なら panic、is_* は bool を返す。
    fn declare_parse_runtime(&mut self) {
        let i64t = self.ctx.i64_type();
        let i32t = self.ctx.i32_type();
        let f64t = self.ctx.f64_type();
        let i1t = self.ctx.bool_type();
        let i8t = self.ctx.i8_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let ext = Some(Linkage::External);

        // long strtol(char* nptr, char** endptr, int base) / double strtod(char*, char**)
        self.module.add_function(
            "strtol",
            i64t.fn_type(&[ptr.into(), ptr.into(), i32t.into()], false),
            ext,
        );
        self.module.add_function(
            "strtod",
            f64t.fn_type(&[ptr.into(), ptr.into()], false),
            ext,
        );

        // s 全体が数値として消費されたか（end が NUL を指し、かつ何か読めた）を i1 で返す。
        let validity = |cg: &Self, s: PointerValue<'ctx>, end: PointerValue<'ctx>| {
            let si = cg.builder.build_ptr_to_int(s, i64t, "si").unwrap();
            let ei = cg.builder.build_ptr_to_int(end, i64t, "ei").unwrap();
            let parsed = cg
                .builder
                .build_int_compare(IntPredicate::NE, ei, si, "parsed")
                .unwrap();
            let ec = cg
                .builder
                .build_load(i8t, end, "ec")
                .unwrap()
                .into_int_value();
            let consumed = cg
                .builder
                .build_int_compare(IntPredicate::EQ, ec, i8t.const_zero(), "consumed")
                .unwrap();
            cg.builder.build_and(parsed, consumed, "ok").unwrap()
        };

        // --- i1 lumo_is_int(ptr s) ---
        let is_int_fn =
            self.module
                .add_function("lumo_is_int", i1t.fn_type(&[ptr.into()], false), None);
        {
            let entry = self.ctx.append_basic_block(is_int_fn, "entry");
            self.builder.position_at_end(entry);
            let s = is_int_fn.get_nth_param(0).unwrap().into_pointer_value();
            let endp = self.builder.build_alloca(ptr, "endp").unwrap();
            let strtol = self.module.get_function("strtol").unwrap();
            self.builder
                .build_call(
                    strtol,
                    &[s.into(), endp.into(), i32t.const_int(10, false).into()],
                    "",
                )
                .unwrap();
            let end = self
                .builder
                .build_load(ptr, endp, "end")
                .unwrap()
                .into_pointer_value();
            let ok = validity(self, s, end);
            self.builder.build_return(Some(&ok)).unwrap();
        }

        // --- i64 lumo_parse_int(ptr s): 不正なら panic ---
        let parse_int_fn =
            self.module
                .add_function("lumo_parse_int", i64t.fn_type(&[ptr.into()], false), None);
        {
            let entry = self.ctx.append_basic_block(parse_int_fn, "entry");
            let ok_bb = self.ctx.append_basic_block(parse_int_fn, "ok");
            let fail_bb = self.ctx.append_basic_block(parse_int_fn, "fail");
            self.builder.position_at_end(entry);
            let s = parse_int_fn.get_nth_param(0).unwrap().into_pointer_value();
            let endp = self.builder.build_alloca(ptr, "endp").unwrap();
            let strtol = self.module.get_function("strtol").unwrap();
            let v = self
                .builder
                .build_call(
                    strtol,
                    &[s.into(), endp.into(), i32t.const_int(10, false).into()],
                    "v",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let end = self
                .builder
                .build_load(ptr, endp, "end")
                .unwrap()
                .into_pointer_value();
            let ok = validity(self, s, end);
            self.builder
                .build_conditional_branch(ok, ok_bb, fail_bb)
                .unwrap();
            self.builder.position_at_end(fail_bb);
            self.panic("lumo: int() got a non-integer string\n", "parse_int_msg");
            self.builder.position_at_end(ok_bb);
            self.builder.build_return(Some(&v)).unwrap();
        }

        // --- i1 lumo_is_float(ptr s) ---
        let is_float_fn =
            self.module
                .add_function("lumo_is_float", i1t.fn_type(&[ptr.into()], false), None);
        {
            let entry = self.ctx.append_basic_block(is_float_fn, "entry");
            self.builder.position_at_end(entry);
            let s = is_float_fn.get_nth_param(0).unwrap().into_pointer_value();
            let endp = self.builder.build_alloca(ptr, "endp").unwrap();
            let strtod = self.module.get_function("strtod").unwrap();
            self.builder
                .build_call(strtod, &[s.into(), endp.into()], "")
                .unwrap();
            let end = self
                .builder
                .build_load(ptr, endp, "end")
                .unwrap()
                .into_pointer_value();
            let ok = validity(self, s, end);
            self.builder.build_return(Some(&ok)).unwrap();
        }

        // --- f64 lumo_parse_float(ptr s): 不正なら panic ---
        let parse_float_fn =
            self.module
                .add_function("lumo_parse_float", f64t.fn_type(&[ptr.into()], false), None);
        {
            let entry = self.ctx.append_basic_block(parse_float_fn, "entry");
            let ok_bb = self.ctx.append_basic_block(parse_float_fn, "ok");
            let fail_bb = self.ctx.append_basic_block(parse_float_fn, "fail");
            self.builder.position_at_end(entry);
            let s = parse_float_fn
                .get_nth_param(0)
                .unwrap()
                .into_pointer_value();
            let endp = self.builder.build_alloca(ptr, "endp").unwrap();
            let strtod = self.module.get_function("strtod").unwrap();
            let v = self
                .builder
                .build_call(strtod, &[s.into(), endp.into()], "v")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_float_value();
            let end = self
                .builder
                .build_load(ptr, endp, "end")
                .unwrap()
                .into_pointer_value();
            let ok = validity(self, s, end);
            self.builder
                .build_conditional_branch(ok, ok_bb, fail_bb)
                .unwrap();
            self.builder.position_at_end(fail_bb);
            self.panic("lumo: float() got a non-number string\n", "parse_float_msg");
            self.builder.position_at_end(ok_bb);
            self.builder.build_return(Some(&v)).unwrap();
        }
    }

    /// 文字列ツールキットのランタイム（substr/split/join）を IR で定義する。
    ///
    /// 文字列は NUL 終端のヒープ確保バイト列。split は結果を [string] 配列
    /// （v0.22 のレイアウト {len,cap,data}）で返し、join はその逆。
    fn declare_string_runtime(&mut self) {
        let i64t = self.ctx.i64_type();
        let i1t = self.ctx.bool_type();
        let i8t = self.ctx.i8_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());

        // --- ptr lumo_substr(ptr s, i64 start, i64 n): s[start..start+n] の新規文字列 ---
        // 呼び出し側が start/n の範囲を保証する。
        let substr_fn = self.module.add_function(
            "lumo_substr",
            ptr.fn_type(&[ptr.into(), i64t.into(), i64t.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(substr_fn, "entry");
            self.builder.position_at_end(entry);
            let s = substr_fn.get_nth_param(0).unwrap().into_pointer_value();
            let start = substr_fn.get_nth_param(1).unwrap().into_int_value();
            let n = substr_fn.get_nth_param(2).unwrap().into_int_value();
            let alloc = self.module.get_function("lumo_alloc").unwrap();
            let size = self
                .builder
                .build_int_add(n, i64t.const_int(1, false), "size")
                .unwrap();
            let buf = self
                .builder
                .build_call(alloc, &[size.into()], "buf")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let src = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, s, &[start], "src")
                    .unwrap()
            };
            let memcpy = self.module.get_function("memcpy").unwrap();
            self.builder
                .build_call(memcpy, &[buf.into(), src.into(), n.into()], "")
                .unwrap();
            // NUL 終端
            let end = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, buf, &[n], "end")
                    .unwrap()
            };
            self.builder.build_store(end, i8t.const_zero()).unwrap();
            self.builder.build_return(Some(&buf)).unwrap();
        }

        // --- i1 lumo_str_eq_at(ptr s, i64 i, ptr sep, i64 m): s[i..i+m]==sep[0..m] ---
        let eqat_fn = self.module.add_function(
            "lumo_str_eq_at",
            i1t.fn_type(&[ptr.into(), i64t.into(), ptr.into(), i64t.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(eqat_fn, "entry");
            let loop_bb = self.ctx.append_basic_block(eqat_fn, "loop");
            let body_bb = self.ctx.append_basic_block(eqat_fn, "body");
            let neq_bb = self.ctx.append_basic_block(eqat_fn, "neq");
            let yes_bb = self.ctx.append_basic_block(eqat_fn, "yes");
            let s = eqat_fn.get_nth_param(0).unwrap().into_pointer_value();
            let i0 = eqat_fn.get_nth_param(1).unwrap().into_int_value();
            let sep = eqat_fn.get_nth_param(2).unwrap().into_pointer_value();
            let m = eqat_fn.get_nth_param(3).unwrap().into_int_value();
            self.builder.position_at_end(entry);
            let j_ptr = self.builder.build_alloca(i64t, "j").unwrap();
            self.builder.build_store(j_ptr, i64t.const_zero()).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            // loop: if j>=m -> yes
            self.builder.position_at_end(loop_bb);
            let jv = self
                .builder
                .build_load(i64t, j_ptr, "j")
                .unwrap()
                .into_int_value();
            let done = self
                .builder
                .build_int_compare(IntPredicate::UGE, jv, m, "done")
                .unwrap();
            self.builder
                .build_conditional_branch(done, yes_bb, body_bb)
                .unwrap();
            // body: a=s[i+j]; b=sep[j]; if a!=b -> neq
            self.builder.position_at_end(body_bb);
            let ij = self.builder.build_int_add(i0, jv, "ij").unwrap();
            let sa = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, s, &[ij], "sa")
                    .unwrap()
            };
            let a = self
                .builder
                .build_load(i8t, sa, "a")
                .unwrap()
                .into_int_value();
            let sb = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, sep, &[jv], "sb")
                    .unwrap()
            };
            let b = self
                .builder
                .build_load(i8t, sb, "b")
                .unwrap()
                .into_int_value();
            let eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, a, b, "eq")
                .unwrap();
            let cont_bb = self.ctx.append_basic_block(eqat_fn, "cont");
            self.builder
                .build_conditional_branch(eq, cont_bb, neq_bb)
                .unwrap();
            self.builder.position_at_end(cont_bb);
            let j1 = self
                .builder
                .build_int_add(jv, i64t.const_int(1, false), "j1")
                .unwrap();
            self.builder.build_store(j_ptr, j1).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            self.builder.position_at_end(neq_bb);
            self.builder.build_return(Some(&i1t.const_zero())).unwrap();
            self.builder.position_at_end(yes_bb);
            self.builder
                .build_return(Some(&i1t.const_int(1, false)))
                .unwrap();
        }

        // --- ptr lumo_split(ptr s, ptr sep): [string] を返す ---
        // sep が空なら [s]。それ以外は非重複でsepを境に分割（連続/端のsepは空文字列を生む）。
        let split_fn = self.module.add_function(
            "lumo_split",
            ptr.fn_type(&[ptr.into(), ptr.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(split_fn, "entry");
            let s = split_fn.get_nth_param(0).unwrap().into_pointer_value();
            let sep = split_fn.get_nth_param(1).unwrap().into_pointer_value();
            self.builder.position_at_end(entry);
            let strlen = self.module.get_function("strlen").unwrap();
            let n = self
                .builder
                .build_call(strlen, &[s.into()], "n")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let m = self
                .builder
                .build_call(strlen, &[sep.into()], "m")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let alloc = self.module.get_function("lumo_alloc").unwrap();
            // sep が空なら [s] を返す
            let sep_empty = self
                .builder
                .build_int_compare(IntPredicate::EQ, m, i64t.const_zero(), "sepempty")
                .unwrap();
            let one_bb = self.ctx.append_basic_block(split_fn, "single");
            let multi_bb = self.ctx.append_basic_block(split_fn, "multi");
            self.builder
                .build_conditional_branch(sep_empty, one_bb, multi_bb)
                .unwrap();
            // single: 配列[ s ]
            self.builder.position_at_end(one_bb);
            let arr1 = self
                .builder
                .build_call(alloc, &[i64t.const_int(24, false).into()], "arr1")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder
                .build_store(arr1, i64t.const_int(1, false))
                .unwrap();
            self.builder
                .build_store(self.hdr_field(arr1, 8), i64t.const_int(1, false))
                .unwrap();
            let d1 = self
                .builder
                .build_call(alloc, &[i64t.const_int(8, false).into()], "d1")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder.build_store(d1, s).unwrap();
            self.builder
                .build_store(self.hdr_field(arr1, 16), d1)
                .unwrap();
            self.builder.build_return(Some(&arr1)).unwrap();
            // multi: 2パス（数える→詰める）
            self.builder.position_at_end(multi_bb);
            // pass1: count separators
            let cnt_ptr = self.builder.build_alloca(i64t, "cnt").unwrap();
            let i_ptr = self.builder.build_alloca(i64t, "i").unwrap();
            self.builder
                .build_store(cnt_ptr, i64t.const_zero())
                .unwrap();
            self.builder.build_store(i_ptr, i64t.const_zero()).unwrap();
            let p1 = self.ctx.append_basic_block(split_fn, "p1");
            let p1b = self.ctx.append_basic_block(split_fn, "p1b");
            let p1m = self.ctx.append_basic_block(split_fn, "p1match");
            let p1n = self.ctx.append_basic_block(split_fn, "p1next");
            let p1done = self.ctx.append_basic_block(split_fn, "p1done");
            self.builder.build_unconditional_branch(p1).unwrap();
            // p1: while i+m <= n
            self.builder.position_at_end(p1);
            let iv = self
                .builder
                .build_load(i64t, i_ptr, "i")
                .unwrap()
                .into_int_value();
            let im = self.builder.build_int_add(iv, m, "im").unwrap();
            let in_range = self
                .builder
                .build_int_compare(IntPredicate::ULE, im, n, "inr")
                .unwrap();
            self.builder
                .build_conditional_branch(in_range, p1b, p1done)
                .unwrap();
            // p1b: if eq_at -> match else i++
            self.builder.position_at_end(p1b);
            let eq = self
                .builder
                .build_call(eqat_fn, &[s.into(), iv.into(), sep.into(), m.into()], "eq")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            self.builder.build_conditional_branch(eq, p1m, p1n).unwrap();
            // p1match: cnt++; i+=m
            self.builder.position_at_end(p1m);
            let c = self
                .builder
                .build_load(i64t, cnt_ptr, "c")
                .unwrap()
                .into_int_value();
            let c1 = self
                .builder
                .build_int_add(c, i64t.const_int(1, false), "c1")
                .unwrap();
            self.builder.build_store(cnt_ptr, c1).unwrap();
            self.builder.build_store(i_ptr, im).unwrap();
            self.builder.build_unconditional_branch(p1).unwrap();
            // p1next: i++
            self.builder.position_at_end(p1n);
            let i1 = self
                .builder
                .build_int_add(iv, i64t.const_int(1, false), "i1")
                .unwrap();
            self.builder.build_store(i_ptr, i1).unwrap();
            self.builder.build_unconditional_branch(p1).unwrap();
            // p1done: pieces = cnt+1; alloc array
            self.builder.position_at_end(p1done);
            let cnt = self
                .builder
                .build_load(i64t, cnt_ptr, "cnt")
                .unwrap()
                .into_int_value();
            let pieces = self
                .builder
                .build_int_add(cnt, i64t.const_int(1, false), "pieces")
                .unwrap();
            let arr = self
                .builder
                .build_call(alloc, &[i64t.const_int(24, false).into()], "arr")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder.build_store(arr, pieces).unwrap();
            self.builder
                .build_store(self.hdr_field(arr, 8), pieces)
                .unwrap();
            let dbytes = self
                .builder
                .build_int_mul(pieces, i64t.const_int(8, false), "dbytes")
                .unwrap();
            let data = self
                .builder
                .build_call(alloc, &[dbytes.into()], "data")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder
                .build_store(self.hdr_field(arr, 16), data)
                .unwrap();
            // pass2: extract pieces. start=0, i=0, w=0
            let start_ptr = self.builder.build_alloca(i64t, "start").unwrap();
            let w_ptr = self.builder.build_alloca(i64t, "w").unwrap();
            self.builder
                .build_store(start_ptr, i64t.const_zero())
                .unwrap();
            self.builder.build_store(i_ptr, i64t.const_zero()).unwrap();
            self.builder.build_store(w_ptr, i64t.const_zero()).unwrap();
            let p2 = self.ctx.append_basic_block(split_fn, "p2");
            let p2b = self.ctx.append_basic_block(split_fn, "p2b");
            let p2m = self.ctx.append_basic_block(split_fn, "p2match");
            let p2n = self.ctx.append_basic_block(split_fn, "p2next");
            let p2done = self.ctx.append_basic_block(split_fn, "p2done");
            self.builder.build_unconditional_branch(p2).unwrap();
            self.builder.position_at_end(p2);
            let iv2 = self
                .builder
                .build_load(i64t, i_ptr, "i")
                .unwrap()
                .into_int_value();
            let im2 = self.builder.build_int_add(iv2, m, "im2").unwrap();
            let inr2 = self
                .builder
                .build_int_compare(IntPredicate::ULE, im2, n, "inr2")
                .unwrap();
            self.builder
                .build_conditional_branch(inr2, p2b, p2done)
                .unwrap();
            self.builder.position_at_end(p2b);
            let eq2 = self
                .builder
                .build_call(
                    eqat_fn,
                    &[s.into(), iv2.into(), sep.into(), m.into()],
                    "eq2",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            self.builder
                .build_conditional_branch(eq2, p2m, p2n)
                .unwrap();
            // p2match: piece = substr(s, start, i-start); store; i+=m; start=i
            self.builder.position_at_end(p2m);
            let st = self
                .builder
                .build_load(i64t, start_ptr, "st")
                .unwrap()
                .into_int_value();
            let plen = self.builder.build_int_sub(iv2, st, "plen").unwrap();
            let piece = self
                .builder
                .build_call(substr_fn, &[s.into(), st.into(), plen.into()], "piece")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let wv = self
                .builder
                .build_load(i64t, w_ptr, "w")
                .unwrap()
                .into_int_value();
            let woff = self
                .builder
                .build_int_mul(wv, i64t.const_int(8, false), "woff")
                .unwrap();
            let waddr = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, data, &[woff], "waddr")
                    .unwrap()
            };
            self.builder.build_store(waddr, piece).unwrap();
            self.builder
                .build_store(
                    w_ptr,
                    self.builder
                        .build_int_add(wv, i64t.const_int(1, false), "w1")
                        .unwrap(),
                )
                .unwrap();
            self.builder.build_store(i_ptr, im2).unwrap();
            self.builder.build_store(start_ptr, im2).unwrap();
            self.builder.build_unconditional_branch(p2).unwrap();
            // p2next: i++
            self.builder.position_at_end(p2n);
            let i1b = self
                .builder
                .build_int_add(iv2, i64t.const_int(1, false), "i1b")
                .unwrap();
            self.builder.build_store(i_ptr, i1b).unwrap();
            self.builder.build_unconditional_branch(p2).unwrap();
            // p2done: last piece = substr(s, start, n-start)
            self.builder.position_at_end(p2done);
            let st2 = self
                .builder
                .build_load(i64t, start_ptr, "st2")
                .unwrap()
                .into_int_value();
            let lastlen = self.builder.build_int_sub(n, st2, "lastlen").unwrap();
            let last = self
                .builder
                .build_call(substr_fn, &[s.into(), st2.into(), lastlen.into()], "last")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let wv2 = self
                .builder
                .build_load(i64t, w_ptr, "w2")
                .unwrap()
                .into_int_value();
            let woff2 = self
                .builder
                .build_int_mul(wv2, i64t.const_int(8, false), "woff2")
                .unwrap();
            let waddr2 = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, data, &[woff2], "waddr2")
                    .unwrap()
            };
            self.builder.build_store(waddr2, last).unwrap();
            self.builder.build_return(Some(&arr)).unwrap();
        }

        // --- ptr lumo_join(ptr arr, ptr sep): [string] を sep で連結した文字列 ---
        let join_fn = self.module.add_function(
            "lumo_join",
            ptr.fn_type(&[ptr.into(), ptr.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(join_fn, "entry");
            let arr = join_fn.get_nth_param(0).unwrap().into_pointer_value();
            let sep = join_fn.get_nth_param(1).unwrap().into_pointer_value();
            self.builder.position_at_end(entry);
            let alloc = self.module.get_function("lumo_alloc").unwrap();
            let strlen = self.module.get_function("strlen").unwrap();
            let memcpy = self.module.get_function("memcpy").unwrap();
            let cnt = self
                .builder
                .build_load(i64t, arr, "cnt")
                .unwrap()
                .into_int_value();
            let data = self
                .builder
                .build_load(ptr, self.hdr_field(arr, 16), "data")
                .unwrap()
                .into_pointer_value();
            let m = self
                .builder
                .build_call(strlen, &[sep.into()], "m")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            // total = sum(strlen(piece)) + m*(cnt-1)（cnt==0 のときは別扱い）
            // pass1
            let total_ptr = self.builder.build_alloca(i64t, "total").unwrap();
            let k_ptr = self.builder.build_alloca(i64t, "k").unwrap();
            self.builder
                .build_store(total_ptr, i64t.const_zero())
                .unwrap();
            self.builder.build_store(k_ptr, i64t.const_zero()).unwrap();
            let s1 = self.ctx.append_basic_block(join_fn, "s1");
            let s1b = self.ctx.append_basic_block(join_fn, "s1b");
            let s1d = self.ctx.append_basic_block(join_fn, "s1d");
            self.builder.build_unconditional_branch(s1).unwrap();
            self.builder.position_at_end(s1);
            let kv = self
                .builder
                .build_load(i64t, k_ptr, "k")
                .unwrap()
                .into_int_value();
            let kdone = self
                .builder
                .build_int_compare(IntPredicate::UGE, kv, cnt, "kdone")
                .unwrap();
            self.builder
                .build_conditional_branch(kdone, s1d, s1b)
                .unwrap();
            self.builder.position_at_end(s1b);
            let koff = self
                .builder
                .build_int_mul(kv, i64t.const_int(8, false), "koff")
                .unwrap();
            let kaddr = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, data, &[koff], "kaddr")
                    .unwrap()
            };
            let piece = self
                .builder
                .build_load(ptr, kaddr, "piece")
                .unwrap()
                .into_pointer_value();
            let pl = self
                .builder
                .build_call(strlen, &[piece.into()], "pl")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let tv = self
                .builder
                .build_load(i64t, total_ptr, "t")
                .unwrap()
                .into_int_value();
            self.builder
                .build_store(total_ptr, self.builder.build_int_add(tv, pl, "t1").unwrap())
                .unwrap();
            self.builder
                .build_store(
                    k_ptr,
                    self.builder
                        .build_int_add(kv, i64t.const_int(1, false), "k1")
                        .unwrap(),
                )
                .unwrap();
            self.builder.build_unconditional_branch(s1).unwrap();
            // s1d: add separators総量 m*(cnt-1)（cnt>0 のとき）。cnt==0 は (cnt-1) が巨大化するので分岐。
            self.builder.position_at_end(s1d);
            let has_any = self
                .builder
                .build_int_compare(IntPredicate::UGT, cnt, i64t.const_zero(), "hasany")
                .unwrap();
            let seps_bb = self.ctx.append_basic_block(join_fn, "seps");
            let alloc_bb = self.ctx.append_basic_block(join_fn, "alloc");
            self.builder
                .build_conditional_branch(has_any, seps_bb, alloc_bb)
                .unwrap();
            self.builder.position_at_end(seps_bb);
            let cm1 = self
                .builder
                .build_int_sub(cnt, i64t.const_int(1, false), "cm1")
                .unwrap();
            let sepsum = self.builder.build_int_mul(m, cm1, "sepsum").unwrap();
            let tv2 = self
                .builder
                .build_load(i64t, total_ptr, "t2")
                .unwrap()
                .into_int_value();
            self.builder
                .build_store(
                    total_ptr,
                    self.builder.build_int_add(tv2, sepsum, "t3").unwrap(),
                )
                .unwrap();
            self.builder.build_unconditional_branch(alloc_bb).unwrap();
            // alloc: buf = lumo_alloc(total+1)
            self.builder.position_at_end(alloc_bb);
            let total = self
                .builder
                .build_load(i64t, total_ptr, "total")
                .unwrap()
                .into_int_value();
            let bufsize = self
                .builder
                .build_int_add(total, i64t.const_int(1, false), "bufsize")
                .unwrap();
            let buf = self
                .builder
                .build_call(alloc, &[bufsize.into()], "buf")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            // pass2: copy pieces with separators; pos tracked
            let pos_ptr = self.builder.build_alloca(i64t, "pos").unwrap();
            self.builder
                .build_store(pos_ptr, i64t.const_zero())
                .unwrap();
            self.builder.build_store(k_ptr, i64t.const_zero()).unwrap();
            let s2 = self.ctx.append_basic_block(join_fn, "s2");
            let s2b = self.ctx.append_basic_block(join_fn, "s2b");
            let s2sep = self.ctx.append_basic_block(join_fn, "s2sep");
            let s2cp = self.ctx.append_basic_block(join_fn, "s2cp");
            let s2d = self.ctx.append_basic_block(join_fn, "s2d");
            self.builder.build_unconditional_branch(s2).unwrap();
            self.builder.position_at_end(s2);
            let kv2 = self
                .builder
                .build_load(i64t, k_ptr, "k")
                .unwrap()
                .into_int_value();
            let kdone2 = self
                .builder
                .build_int_compare(IntPredicate::UGE, kv2, cnt, "kdone2")
                .unwrap();
            self.builder
                .build_conditional_branch(kdone2, s2d, s2b)
                .unwrap();
            // s2b: if k>0 prepend sep
            self.builder.position_at_end(s2b);
            let kpos = self
                .builder
                .build_int_compare(IntPredicate::UGT, kv2, i64t.const_zero(), "kpos")
                .unwrap();
            self.builder
                .build_conditional_branch(kpos, s2sep, s2cp)
                .unwrap();
            self.builder.position_at_end(s2sep);
            let posv = self
                .builder
                .build_load(i64t, pos_ptr, "pos")
                .unwrap()
                .into_int_value();
            let dst = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, buf, &[posv], "dst")
                    .unwrap()
            };
            self.builder
                .build_call(memcpy, &[dst.into(), sep.into(), m.into()], "")
                .unwrap();
            self.builder
                .build_store(
                    pos_ptr,
                    self.builder.build_int_add(posv, m, "pos1").unwrap(),
                )
                .unwrap();
            self.builder.build_unconditional_branch(s2cp).unwrap();
            // s2cp: copy piece
            self.builder.position_at_end(s2cp);
            let koff2 = self
                .builder
                .build_int_mul(kv2, i64t.const_int(8, false), "koff2")
                .unwrap();
            let kaddr2 = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, data, &[koff2], "kaddr2")
                    .unwrap()
            };
            let piece2 = self
                .builder
                .build_load(ptr, kaddr2, "piece2")
                .unwrap()
                .into_pointer_value();
            let pl2 = self
                .builder
                .build_call(strlen, &[piece2.into()], "pl2")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let posv2 = self
                .builder
                .build_load(i64t, pos_ptr, "pos2")
                .unwrap()
                .into_int_value();
            let dst2 = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, buf, &[posv2], "dst2")
                    .unwrap()
            };
            self.builder
                .build_call(memcpy, &[dst2.into(), piece2.into(), pl2.into()], "")
                .unwrap();
            self.builder
                .build_store(
                    pos_ptr,
                    self.builder.build_int_add(posv2, pl2, "pos2b").unwrap(),
                )
                .unwrap();
            self.builder
                .build_store(
                    k_ptr,
                    self.builder
                        .build_int_add(kv2, i64t.const_int(1, false), "k2")
                        .unwrap(),
                )
                .unwrap();
            self.builder.build_unconditional_branch(s2).unwrap();
            // s2d: NUL terminate, return
            self.builder.position_at_end(s2d);
            let endpos = self
                .builder
                .build_load(i64t, pos_ptr, "endpos")
                .unwrap()
                .into_int_value();
            let bend = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, buf, &[endpos], "bend")
                    .unwrap()
            };
            self.builder.build_store(bend, i8t.const_zero()).unwrap();
            self.builder.build_return(Some(&buf)).unwrap();
        }

        // --- ptr lumo_to_case(ptr s, i1 upper): ASCII で大文字/小文字化した新規文字列 ---
        // upper=1 なら a-z を A-Z に、0 なら A-Z を a-z に。他のバイトは素通し。
        let case_fn = self.module.add_function(
            "lumo_to_case",
            ptr.fn_type(&[ptr.into(), i1t.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(case_fn, "entry");
            let loop_bb = self.ctx.append_basic_block(case_fn, "loop");
            let body_bb = self.ctx.append_basic_block(case_fn, "body");
            let fin_bb = self.ctx.append_basic_block(case_fn, "fin");
            let s = case_fn.get_nth_param(0).unwrap().into_pointer_value();
            let upper = case_fn.get_nth_param(1).unwrap().into_int_value();
            self.builder.position_at_end(entry);
            let strlen = self.module.get_function("strlen").unwrap();
            let alloc = self.module.get_function("lumo_alloc").unwrap();
            let len = self
                .builder
                .build_call(strlen, &[s.into()], "len")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let size = self
                .builder
                .build_int_add(len, i64t.const_int(1, false), "size")
                .unwrap();
            let buf = self
                .builder
                .build_call(alloc, &[size.into()], "buf")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let i_ptr = self.builder.build_alloca(i64t, "i").unwrap();
            self.builder.build_store(i_ptr, i64t.const_zero()).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            self.builder.position_at_end(loop_bb);
            let iv = self
                .builder
                .build_load(i64t, i_ptr, "i")
                .unwrap()
                .into_int_value();
            let done = self
                .builder
                .build_int_compare(IntPredicate::UGE, iv, len, "done")
                .unwrap();
            self.builder
                .build_conditional_branch(done, fin_bb, body_bb)
                .unwrap();
            self.builder.position_at_end(body_bb);
            let sa = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, s, &[iv], "sa")
                    .unwrap()
            };
            let c = self
                .builder
                .build_load(i8t, sa, "c")
                .unwrap()
                .into_int_value();
            let d32 = i8t.const_int(32, false);
            // lower->upper: 'a'(97)..='z'(122) なら -32
            let ge_lo = self
                .builder
                .build_int_compare(IntPredicate::UGE, c, i8t.const_int(97, false), "ge_lo")
                .unwrap();
            let le_lo = self
                .builder
                .build_int_compare(IntPredicate::ULE, c, i8t.const_int(122, false), "le_lo")
                .unwrap();
            let is_lower = self.builder.build_and(ge_lo, le_lo, "is_lower").unwrap();
            let upped = self.builder.build_int_sub(c, d32, "upped").unwrap();
            let c_up = self
                .builder
                .build_select(is_lower, upped, c, "c_up")
                .unwrap()
                .into_int_value();
            // upper->lower: 'A'(65)..='Z'(90) なら +32
            let ge_up = self
                .builder
                .build_int_compare(IntPredicate::UGE, c, i8t.const_int(65, false), "ge_up")
                .unwrap();
            let le_up = self
                .builder
                .build_int_compare(IntPredicate::ULE, c, i8t.const_int(90, false), "le_up")
                .unwrap();
            let is_upper = self.builder.build_and(ge_up, le_up, "is_upper").unwrap();
            let lowed = self.builder.build_int_add(c, d32, "lowed").unwrap();
            let c_lo = self
                .builder
                .build_select(is_upper, lowed, c, "c_lo")
                .unwrap()
                .into_int_value();
            let c2 = self
                .builder
                .build_select(upper, c_up, c_lo, "c2")
                .unwrap()
                .into_int_value();
            let ba = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, buf, &[iv], "ba")
                    .unwrap()
            };
            self.builder.build_store(ba, c2).unwrap();
            let inext = self
                .builder
                .build_int_add(iv, i64t.const_int(1, false), "inext")
                .unwrap();
            self.builder.build_store(i_ptr, inext).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            self.builder.position_at_end(fin_bb);
            let bend = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, buf, &[len], "bend")
                    .unwrap()
            };
            self.builder.build_store(bend, i8t.const_zero()).unwrap();
            self.builder.build_return(Some(&buf)).unwrap();
        }

        // --- i64 lumo_find(ptr s, ptr sub): sub の最初の出現バイト位置、無ければ -1 ---
        // 空の sub は 0 を返す（位置 0 で一致）。
        let find_fn = self.module.add_function(
            "lumo_find",
            i64t.fn_type(&[ptr.into(), ptr.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(find_fn, "entry");
            let loop_bb = self.ctx.append_basic_block(find_fn, "loop");
            let body_bb = self.ctx.append_basic_block(find_fn, "body");
            let found_bb = self.ctx.append_basic_block(find_fn, "found");
            let next_bb = self.ctx.append_basic_block(find_fn, "next");
            let nf_bb = self.ctx.append_basic_block(find_fn, "notfound");
            let s = find_fn.get_nth_param(0).unwrap().into_pointer_value();
            let sub = find_fn.get_nth_param(1).unwrap().into_pointer_value();
            self.builder.position_at_end(entry);
            let strlen = self.module.get_function("strlen").unwrap();
            let slen = self
                .builder
                .build_call(strlen, &[s.into()], "slen")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let sublen = self
                .builder
                .build_call(strlen, &[sub.into()], "sublen")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            // limit = slen - sublen（符号付き。負なら走査しない＝見つからない）
            let limit = self.builder.build_int_sub(slen, sublen, "limit").unwrap();
            let i_ptr = self.builder.build_alloca(i64t, "i").unwrap();
            self.builder.build_store(i_ptr, i64t.const_zero()).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            self.builder.position_at_end(loop_bb);
            let iv = self
                .builder
                .build_load(i64t, i_ptr, "i")
                .unwrap()
                .into_int_value();
            let cont = self
                .builder
                .build_int_compare(IntPredicate::SLE, iv, limit, "cont")
                .unwrap();
            self.builder
                .build_conditional_branch(cont, body_bb, nf_bb)
                .unwrap();
            self.builder.position_at_end(body_bb);
            let eqat = self.module.get_function("lumo_str_eq_at").unwrap();
            let eq = self
                .builder
                .build_call(
                    eqat,
                    &[s.into(), iv.into(), sub.into(), sublen.into()],
                    "eq",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            self.builder
                .build_conditional_branch(eq, found_bb, next_bb)
                .unwrap();
            self.builder.position_at_end(found_bb);
            self.builder.build_return(Some(&iv)).unwrap();
            self.builder.position_at_end(next_bb);
            let inext = self
                .builder
                .build_int_add(iv, i64t.const_int(1, false), "inext")
                .unwrap();
            self.builder.build_store(i_ptr, inext).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            self.builder.position_at_end(nf_bb);
            self.builder
                .build_return(Some(&i64t.const_all_ones()))
                .unwrap();
        }

        // --- i1 lumo_str_has_affix(ptr s, ptr affix, i1 at_end) ---
        // at_end=0 で前方一致(starts_with)、1 で後方一致(ends_with)。
        // affix が s より長ければ false。さもなくば eq_at で照合する。
        let affix_fn = self.module.add_function(
            "lumo_str_has_affix",
            i1t.fn_type(&[ptr.into(), ptr.into(), i1t.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(affix_fn, "entry");
            let check_bb = self.ctx.append_basic_block(affix_fn, "check");
            let no_bb = self.ctx.append_basic_block(affix_fn, "no");
            let s = affix_fn.get_nth_param(0).unwrap().into_pointer_value();
            let affix = affix_fn.get_nth_param(1).unwrap().into_pointer_value();
            let at_end = affix_fn.get_nth_param(2).unwrap().into_int_value();
            self.builder.position_at_end(entry);
            let strlen = self.module.get_function("strlen").unwrap();
            let slen = self
                .builder
                .build_call(strlen, &[s.into()], "slen")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let alen = self
                .builder
                .build_call(strlen, &[affix.into()], "alen")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            // affix が s より長ければ即 false（範囲外参照も防ぐ）
            let fits = self
                .builder
                .build_int_compare(IntPredicate::ULE, alen, slen, "fits")
                .unwrap();
            self.builder
                .build_conditional_branch(fits, check_bb, no_bb)
                .unwrap();
            self.builder.position_at_end(no_bb);
            self.builder.build_return(Some(&i1t.const_zero())).unwrap();
            self.builder.position_at_end(check_bb);
            // off = at_end ? slen - alen : 0
            let tail = self.builder.build_int_sub(slen, alen, "tail").unwrap();
            let off = self
                .builder
                .build_select(at_end, tail, i64t.const_zero(), "off")
                .unwrap()
                .into_int_value();
            let eqat = self.module.get_function("lumo_str_eq_at").unwrap();
            let eq = self
                .builder
                .build_call(
                    eqat,
                    &[s.into(), off.into(), affix.into(), alen.into()],
                    "eq",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
            self.builder.build_return(Some(&eq)).unwrap();
        }

        // --- ptr lumo_trim(ptr s): 前後の ASCII 空白(空白/タブ/改行/復帰)を除いた新規文字列 ---
        let trim_fn =
            self.module
                .add_function("lumo_trim", ptr.fn_type(&[ptr.into()], false), None);
        {
            let entry = self.ctx.append_basic_block(trim_fn, "entry");
            let sloop = self.ctx.append_basic_block(trim_fn, "sloop");
            let scheck = self.ctx.append_basic_block(trim_fn, "scheck");
            let sinc = self.ctx.append_basic_block(trim_fn, "sinc");
            let sdone = self.ctx.append_basic_block(trim_fn, "sdone");
            let eloop = self.ctx.append_basic_block(trim_fn, "eloop");
            let echeck = self.ctx.append_basic_block(trim_fn, "echeck");
            let edec = self.ctx.append_basic_block(trim_fn, "edec");
            let edone = self.ctx.append_basic_block(trim_fn, "edone");
            let s = trim_fn.get_nth_param(0).unwrap().into_pointer_value();
            self.builder.position_at_end(entry);
            let strlen = self.module.get_function("strlen").unwrap();
            let len = self
                .builder
                .build_call(strlen, &[s.into()], "len")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let start_ptr = self.builder.build_alloca(i64t, "start").unwrap();
            let end_ptr = self.builder.build_alloca(i64t, "end").unwrap();
            self.builder
                .build_store(start_ptr, i64t.const_zero())
                .unwrap();
            self.builder.build_unconditional_branch(sloop).unwrap();
            // sloop: while start < len && is_ws(s[start]) start++
            self.builder.position_at_end(sloop);
            let st = self
                .builder
                .build_load(i64t, start_ptr, "st")
                .unwrap()
                .into_int_value();
            let in_range = self
                .builder
                .build_int_compare(IntPredicate::ULT, st, len, "in_range")
                .unwrap();
            self.builder
                .build_conditional_branch(in_range, scheck, sdone)
                .unwrap();
            self.builder.position_at_end(scheck);
            let ca = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, s, &[st], "ca")
                    .unwrap()
            };
            let c = self
                .builder
                .build_load(i8t, ca, "c")
                .unwrap()
                .into_int_value();
            let isws = self.byte_is_ws(c);
            self.builder
                .build_conditional_branch(isws, sinc, sdone)
                .unwrap();
            self.builder.position_at_end(sinc);
            let st1 = self
                .builder
                .build_int_add(st, i64t.const_int(1, false), "st1")
                .unwrap();
            self.builder.build_store(start_ptr, st1).unwrap();
            self.builder.build_unconditional_branch(sloop).unwrap();
            // sdone: end = len; while end > start && is_ws(s[end-1]) end--
            self.builder.position_at_end(sdone);
            let start = self
                .builder
                .build_load(i64t, start_ptr, "start_v")
                .unwrap()
                .into_int_value();
            self.builder.build_store(end_ptr, len).unwrap();
            self.builder.build_unconditional_branch(eloop).unwrap();
            self.builder.position_at_end(eloop);
            let en = self
                .builder
                .build_load(i64t, end_ptr, "en")
                .unwrap()
                .into_int_value();
            let gt = self
                .builder
                .build_int_compare(IntPredicate::UGT, en, start, "gt")
                .unwrap();
            self.builder
                .build_conditional_branch(gt, echeck, edone)
                .unwrap();
            self.builder.position_at_end(echeck);
            let enm1 = self
                .builder
                .build_int_sub(en, i64t.const_int(1, false), "enm1")
                .unwrap();
            let cb = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, s, &[enm1], "cb")
                    .unwrap()
            };
            let c2 = self
                .builder
                .build_load(i8t, cb, "c2")
                .unwrap()
                .into_int_value();
            let isws2 = self.byte_is_ws(c2);
            self.builder
                .build_conditional_branch(isws2, edec, edone)
                .unwrap();
            self.builder.position_at_end(edec);
            self.builder.build_store(end_ptr, enm1).unwrap();
            self.builder.build_unconditional_branch(eloop).unwrap();
            // edone: count = end - start; return lumo_substr(s, start, count)
            self.builder.position_at_end(edone);
            let end = self
                .builder
                .build_load(i64t, end_ptr, "end_v")
                .unwrap()
                .into_int_value();
            let count = self.builder.build_int_sub(end, start, "count").unwrap();
            let substr = self.module.get_function("lumo_substr").unwrap();
            let r = self
                .builder
                .build_call(substr, &[s.into(), start.into(), count.into()], "trimmed")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
            self.builder.build_return(Some(&r)).unwrap();
        }

        // --- ptr lumo_repeat(ptr s, i64 n): s を n 回連結した新規文字列（n<=0 は空）---
        let rep_fn = self.module.add_function(
            "lumo_repeat",
            ptr.fn_type(&[ptr.into(), i64t.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(rep_fn, "entry");
            let loop_bb = self.ctx.append_basic_block(rep_fn, "loop");
            let body_bb = self.ctx.append_basic_block(rep_fn, "body");
            let fin_bb = self.ctx.append_basic_block(rep_fn, "fin");
            let s = rep_fn.get_nth_param(0).unwrap().into_pointer_value();
            let n = rep_fn.get_nth_param(1).unwrap().into_int_value();
            self.builder.position_at_end(entry);
            let strlen = self.module.get_function("strlen").unwrap();
            let alloc = self.module.get_function("lumo_alloc").unwrap();
            let memcpy = self.module.get_function("memcpy").unwrap();
            let slen = self
                .builder
                .build_call(strlen, &[s.into()], "slen")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            // count = max(n, 0)
            let neg = self
                .builder
                .build_int_compare(IntPredicate::SLT, n, i64t.const_zero(), "neg")
                .unwrap();
            let count = self
                .builder
                .build_select(neg, i64t.const_zero(), n, "count")
                .unwrap()
                .into_int_value();
            let total = self.builder.build_int_mul(slen, count, "total").unwrap();
            let size = self
                .builder
                .build_int_add(total, i64t.const_int(1, false), "size")
                .unwrap();
            let buf = self
                .builder
                .build_call(alloc, &[size.into()], "buf")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let k_ptr = self.builder.build_alloca(i64t, "k").unwrap();
            self.builder.build_store(k_ptr, i64t.const_zero()).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            self.builder.position_at_end(loop_bb);
            let kv = self
                .builder
                .build_load(i64t, k_ptr, "k")
                .unwrap()
                .into_int_value();
            let done = self
                .builder
                .build_int_compare(IntPredicate::UGE, kv, count, "done")
                .unwrap();
            self.builder
                .build_conditional_branch(done, fin_bb, body_bb)
                .unwrap();
            self.builder.position_at_end(body_bb);
            let off = self.builder.build_int_mul(kv, slen, "off").unwrap();
            let dst = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, buf, &[off], "dst")
                    .unwrap()
            };
            self.builder
                .build_call(memcpy, &[dst.into(), s.into(), slen.into()], "")
                .unwrap();
            let knext = self
                .builder
                .build_int_add(kv, i64t.const_int(1, false), "knext")
                .unwrap();
            self.builder.build_store(k_ptr, knext).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            self.builder.position_at_end(fin_bb);
            let bend = unsafe {
                self.builder
                    .build_in_bounds_gep(i8t, buf, &[total], "bend")
                    .unwrap()
            };
            self.builder.build_store(bend, i8t.const_zero()).unwrap();
            self.builder.build_return(Some(&buf)).unwrap();
        }
    }

    /// あるバイト値が ASCII 空白(空白/タブ/改行/復帰)かどうかを表す i1 を作る。
    fn byte_is_ws(&self, c: inkwell::values::IntValue<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let i8t = self.ctx.i8_type();
        let eq = |v: u64, name: &str| {
            self.builder
                .build_int_compare(IntPredicate::EQ, c, i8t.const_int(v, false), name)
                .unwrap()
        };
        let e_sp = eq(32, "ws_sp");
        let e_tab = eq(9, "ws_tab");
        let e_nl = eq(10, "ws_nl");
        let e_cr = eq(13, "ws_cr");
        let o1 = self.builder.build_or(e_sp, e_tab, "ws1").unwrap();
        let o2 = self.builder.build_or(o1, e_nl, "ws2").unwrap();
        self.builder.build_or(o2, e_cr, "ws3").unwrap()
    }

    /// map のランタイム（分離連鎖ハッシュ表、RFC 0002）を IR で定義する。
    ///
    /// レイアウト:
    ///   ヘッダ {i64 count@0, i64 nbuckets@8, ptr buckets@16}（map 値はこのポインタ）
    ///   バケット = nbuckets 個の ptr（各チェーンの先頭、null 終端）
    ///   エントリ {ptr key@0, i64 hash@8, i64 value@16, ptr next@24}
    /// 値は配列スロットと同じく 8byte（int/float のビット列か参照ポインタ）なので、
    /// この 1 つのランタイムが全ての値型に使える。v1 は nbuckets 固定（resize なし）。
    fn declare_map_runtime(&mut self) {
        let i64t = self.ctx.i64_type();
        let i1t = self.ctx.bool_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        let nbuckets: u64 = 64;

        // --- i64 lumo_str_hash(ptr key): FNV-1a ---
        let hash_fn =
            self.module
                .add_function("lumo_str_hash", i64t.fn_type(&[ptr.into()], false), None);
        {
            let entry = self.ctx.append_basic_block(hash_fn, "entry");
            let loop_bb = self.ctx.append_basic_block(hash_fn, "loop");
            let body_bb = self.ctx.append_basic_block(hash_fn, "body");
            let done_bb = self.ctx.append_basic_block(hash_fn, "done");
            let key = hash_fn.get_nth_param(0).unwrap().into_pointer_value();
            // h と i を alloca で持つ（mem2reg がレジスタ化する）
            self.builder.position_at_end(entry);
            let h = self.builder.build_alloca(i64t, "h").unwrap();
            let i = self.builder.build_alloca(i64t, "i").unwrap();
            self.builder
                .build_store(h, i64t.const_int(14695981039346656037, false))
                .unwrap();
            self.builder.build_store(i, i64t.const_zero()).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            // loop: c = key[i]; if c==0 -> done
            self.builder.position_at_end(loop_bb);
            let iv = self
                .builder
                .build_load(i64t, i, "i")
                .unwrap()
                .into_int_value();
            let caddr = unsafe {
                self.builder
                    .build_in_bounds_gep(self.ctx.i8_type(), key, &[iv], "caddr")
                    .unwrap()
            };
            let c = self
                .builder
                .build_load(self.ctx.i8_type(), caddr, "c")
                .unwrap()
                .into_int_value();
            let is_nul = self
                .builder
                .build_int_compare(IntPredicate::EQ, c, self.ctx.i8_type().const_zero(), "nul")
                .unwrap();
            self.builder
                .build_conditional_branch(is_nul, done_bb, body_bb)
                .unwrap();
            // body: h = (h ^ zext c) * prime; i++
            self.builder.position_at_end(body_bb);
            let hv = self
                .builder
                .build_load(i64t, h, "h")
                .unwrap()
                .into_int_value();
            let c64 = self.builder.build_int_z_extend(c, i64t, "c64").unwrap();
            let xored = self.builder.build_xor(hv, c64, "xor").unwrap();
            let mixed = self
                .builder
                .build_int_mul(xored, i64t.const_int(1099511628211, false), "mix")
                .unwrap();
            self.builder.build_store(h, mixed).unwrap();
            let inc = self
                .builder
                .build_int_add(iv, i64t.const_int(1, false), "inc")
                .unwrap();
            self.builder.build_store(i, inc).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            // done: return h
            self.builder.position_at_end(done_bb);
            let hv = self.builder.build_load(i64t, h, "h").unwrap();
            self.builder.build_return(Some(&hv)).unwrap();
        }

        // --- ptr lumo_map_new() ---
        let new_fn = self
            .module
            .add_function("lumo_map_new", ptr.fn_type(&[], false), None);
        {
            let entry = self.ctx.append_basic_block(new_fn, "entry");
            self.builder.position_at_end(entry);
            let alloc = self.module.get_function("lumo_alloc").unwrap();
            let hdr = self
                .builder
                .build_call(alloc, &[i64t.const_int(24, false).into()], "hdr")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder.build_store(hdr, i64t.const_zero()).unwrap();
            self.builder
                .build_store(self.hdr_field(hdr, 8), i64t.const_int(nbuckets, false))
                .unwrap();
            let calloc = self.module.get_function("calloc").unwrap();
            let buckets = self
                .builder
                .build_call(
                    calloc,
                    &[
                        i64t.const_int(nbuckets, false).into(),
                        i64t.const_int(8, false).into(),
                    ],
                    "buckets",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder
                .build_store(self.hdr_field(hdr, 16), buckets)
                .unwrap();
            self.builder.build_return(Some(&hdr)).unwrap();
        }

        // --- ptr lumo_map_find(ptr hdr, ptr key, i64 hash): エントリ or null ---
        let find_fn = self.module.add_function(
            "lumo_map_find",
            ptr.fn_type(&[ptr.into(), ptr.into(), i64t.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(find_fn, "entry");
            let loop_bb = self.ctx.append_basic_block(find_fn, "loop");
            let check_bb = self.ctx.append_basic_block(find_fn, "check");
            let hit_bb = self.ctx.append_basic_block(find_fn, "hit");
            let next_bb = self.ctx.append_basic_block(find_fn, "next");
            let miss_bb = self.ctx.append_basic_block(find_fn, "miss");
            let hdr = find_fn.get_nth_param(0).unwrap().into_pointer_value();
            let key = find_fn.get_nth_param(1).unwrap().into_pointer_value();
            let hash = find_fn.get_nth_param(2).unwrap().into_int_value();
            self.builder.position_at_end(entry);
            // idx = hash u% nbuckets; e = buckets[idx]
            let nb = self
                .builder
                .build_load(i64t, self.hdr_field(hdr, 8), "nb")
                .unwrap()
                .into_int_value();
            let idx = self
                .builder
                .build_int_unsigned_rem(hash, nb, "idx")
                .unwrap();
            let buckets = self
                .builder
                .build_load(ptr, self.hdr_field(hdr, 16), "buckets")
                .unwrap()
                .into_pointer_value();
            let off = self
                .builder
                .build_int_mul(idx, i64t.const_int(8, false), "off")
                .unwrap();
            let slot = unsafe {
                self.builder
                    .build_in_bounds_gep(self.ctx.i8_type(), buckets, &[off], "slot")
                    .unwrap()
            };
            let e_ptr = self.builder.build_alloca(ptr, "e").unwrap();
            let head = self.builder.build_load(ptr, slot, "head").unwrap();
            self.builder.build_store(e_ptr, head).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            // loop: if e==null -> miss
            self.builder.position_at_end(loop_bb);
            let e = self
                .builder
                .build_load(ptr, e_ptr, "e")
                .unwrap()
                .into_pointer_value();
            let is_null = self.builder.build_is_null(e, "isnull").unwrap();
            self.builder
                .build_conditional_branch(is_null, miss_bb, check_bb)
                .unwrap();
            // check: if e.hash==hash && strcmp(e.key,key)==0 -> hit
            self.builder.position_at_end(check_bb);
            let eh = self
                .builder
                .build_load(i64t, self.hdr_field(e, 8), "eh")
                .unwrap()
                .into_int_value();
            let hash_eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, eh, hash, "heq")
                .unwrap();
            let strcmp_bb = self.ctx.append_basic_block(find_fn, "strcmp");
            self.builder
                .build_conditional_branch(hash_eq, strcmp_bb, next_bb)
                .unwrap();
            self.builder.position_at_end(strcmp_bb);
            let ek = self
                .builder
                .build_load(ptr, self.hdr_field(e, 0), "ek")
                .unwrap();
            let strcmp = self.module.get_function("strcmp").unwrap();
            let cmp = self
                .builder
                .build_call(strcmp, &[ek.into(), key.into()], "cmp")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let key_eq = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    cmp,
                    self.ctx.i32_type().const_zero(),
                    "keq",
                )
                .unwrap();
            self.builder
                .build_conditional_branch(key_eq, hit_bb, next_bb)
                .unwrap();
            // hit: return e
            self.builder.position_at_end(hit_bb);
            self.builder.build_return(Some(&e)).unwrap();
            // next: e = e.next; loop
            self.builder.position_at_end(next_bb);
            let nx = self
                .builder
                .build_load(ptr, self.hdr_field(e, 24), "nx")
                .unwrap();
            self.builder.build_store(e_ptr, nx).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            // miss: return null
            self.builder.position_at_end(miss_bb);
            self.builder.build_return(Some(&ptr.const_null())).unwrap();
        }

        // --- void lumo_map_resize(ptr hdr): バケット数を倍にして全エントリを付け替える ---
        // load factor が閾値を超えたとき put から呼ぶ。エントリ節点は再利用し、
        // next を繋ぎ替えるだけ（古いバケット配列は解放しない＝他と同様）。
        let resize_fn = self.module.add_function(
            "lumo_map_resize",
            self.ctx.void_type().fn_type(&[ptr.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(resize_fn, "entry");
            let bloop = self.ctx.append_basic_block(resize_fn, "bloop");
            let bbody = self.ctx.append_basic_block(resize_fn, "bbody");
            let cloop = self.ctx.append_basic_block(resize_fn, "cloop");
            let cbody = self.ctx.append_basic_block(resize_fn, "cbody");
            let bnext = self.ctx.append_basic_block(resize_fn, "bnext");
            let finish = self.ctx.append_basic_block(resize_fn, "finish");
            let hdr = resize_fn.get_nth_param(0).unwrap().into_pointer_value();
            self.builder.position_at_end(entry);
            let old_nb = self
                .builder
                .build_load(i64t, self.hdr_field(hdr, 8), "old_nb")
                .unwrap()
                .into_int_value();
            let new_nb = self
                .builder
                .build_int_mul(old_nb, i64t.const_int(2, false), "new_nb")
                .unwrap();
            let old_buckets = self
                .builder
                .build_load(ptr, self.hdr_field(hdr, 16), "old_buckets")
                .unwrap()
                .into_pointer_value();
            let calloc = self.module.get_function("calloc").unwrap();
            let new_buckets = self
                .builder
                .build_call(
                    calloc,
                    &[new_nb.into(), i64t.const_int(8, false).into()],
                    "new_b",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let b_ptr = self.builder.build_alloca(i64t, "b").unwrap();
            let e_ptr = self.builder.build_alloca(ptr, "e").unwrap();
            self.builder.build_store(b_ptr, i64t.const_zero()).unwrap();
            self.builder.build_unconditional_branch(bloop).unwrap();
            // bloop: if b >= old_nb -> finish
            self.builder.position_at_end(bloop);
            let bv = self
                .builder
                .build_load(i64t, b_ptr, "b")
                .unwrap()
                .into_int_value();
            let b_done = self
                .builder
                .build_int_compare(IntPredicate::UGE, bv, old_nb, "bdone")
                .unwrap();
            self.builder
                .build_conditional_branch(b_done, finish, bbody)
                .unwrap();
            // bbody: e = old_buckets[b]
            self.builder.position_at_end(bbody);
            let boff = self
                .builder
                .build_int_mul(bv, i64t.const_int(8, false), "boff")
                .unwrap();
            let bslot = unsafe {
                self.builder
                    .build_in_bounds_gep(self.ctx.i8_type(), old_buckets, &[boff], "bslot")
                    .unwrap()
            };
            let head = self.builder.build_load(ptr, bslot, "head").unwrap();
            self.builder.build_store(e_ptr, head).unwrap();
            self.builder.build_unconditional_branch(cloop).unwrap();
            // cloop: if e==null -> bnext
            self.builder.position_at_end(cloop);
            let e = self
                .builder
                .build_load(ptr, e_ptr, "e")
                .unwrap()
                .into_pointer_value();
            let e_null = self.builder.build_is_null(e, "enull").unwrap();
            self.builder
                .build_conditional_branch(e_null, bnext, cbody)
                .unwrap();
            // cbody: next=e.next; idx=e.hash % new_nb; prepend e to new_buckets[idx]; e=next
            self.builder.position_at_end(cbody);
            let nx = self
                .builder
                .build_load(ptr, self.hdr_field(e, 24), "nx")
                .unwrap();
            let eh = self
                .builder
                .build_load(i64t, self.hdr_field(e, 8), "eh")
                .unwrap()
                .into_int_value();
            let idx = self
                .builder
                .build_int_unsigned_rem(eh, new_nb, "idx")
                .unwrap();
            let noff = self
                .builder
                .build_int_mul(idx, i64t.const_int(8, false), "noff")
                .unwrap();
            let nslot = unsafe {
                self.builder
                    .build_in_bounds_gep(self.ctx.i8_type(), new_buckets, &[noff], "nslot")
                    .unwrap()
            };
            let nhead = self.builder.build_load(ptr, nslot, "nhead").unwrap();
            self.builder
                .build_store(self.hdr_field(e, 24), nhead)
                .unwrap();
            self.builder.build_store(nslot, e).unwrap();
            self.builder.build_store(e_ptr, nx).unwrap();
            self.builder.build_unconditional_branch(cloop).unwrap();
            // bnext: b++
            self.builder.position_at_end(bnext);
            let b1 = self
                .builder
                .build_int_add(bv, i64t.const_int(1, false), "b1")
                .unwrap();
            self.builder.build_store(b_ptr, b1).unwrap();
            self.builder.build_unconditional_branch(bloop).unwrap();
            // finish: hdr.nbuckets = new_nb; hdr.buckets = new_buckets
            self.builder.position_at_end(finish);
            self.builder
                .build_store(self.hdr_field(hdr, 8), new_nb)
                .unwrap();
            self.builder
                .build_store(self.hdr_field(hdr, 16), new_buckets)
                .unwrap();
            self.builder.build_return(None).unwrap();
        }

        // --- void lumo_map_put(ptr hdr, ptr key, i64 hash, i64 val) ---
        let put_fn = self.module.add_function(
            "lumo_map_put",
            self.ctx
                .void_type()
                .fn_type(&[ptr.into(), ptr.into(), i64t.into(), i64t.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(put_fn, "entry");
            let update_bb = self.ctx.append_basic_block(put_fn, "update");
            let insert_bb = self.ctx.append_basic_block(put_fn, "insert");
            let hdr = put_fn.get_nth_param(0).unwrap().into_pointer_value();
            let key = put_fn.get_nth_param(1).unwrap().into_pointer_value();
            let hash = put_fn.get_nth_param(2).unwrap().into_int_value();
            let val = put_fn.get_nth_param(3).unwrap().into_int_value();
            self.builder.position_at_end(entry);
            let e = self
                .builder
                .build_call(find_fn, &[hdr.into(), key.into(), hash.into()], "e")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let found = self.builder.build_is_not_null(e, "found").unwrap();
            self.builder
                .build_conditional_branch(found, update_bb, insert_bb)
                .unwrap();
            // update: e.value = val; ret
            self.builder.position_at_end(update_bb);
            self.builder
                .build_store(self.hdr_field(e, 16), val)
                .unwrap();
            self.builder.build_return(None).unwrap();
            // insert: ne = alloc(32); fill; prepend to bucket; count++
            self.builder.position_at_end(insert_bb);
            let alloc = self.module.get_function("lumo_alloc").unwrap();
            let ne = self
                .builder
                .build_call(alloc, &[i64t.const_int(32, false).into()], "ne")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder
                .build_store(self.hdr_field(ne, 0), key)
                .unwrap();
            self.builder
                .build_store(self.hdr_field(ne, 8), hash)
                .unwrap();
            self.builder
                .build_store(self.hdr_field(ne, 16), val)
                .unwrap();
            let nb = self
                .builder
                .build_load(i64t, self.hdr_field(hdr, 8), "nb")
                .unwrap()
                .into_int_value();
            let idx = self
                .builder
                .build_int_unsigned_rem(hash, nb, "idx")
                .unwrap();
            let buckets = self
                .builder
                .build_load(ptr, self.hdr_field(hdr, 16), "buckets")
                .unwrap()
                .into_pointer_value();
            let off = self
                .builder
                .build_int_mul(idx, i64t.const_int(8, false), "off")
                .unwrap();
            let slot = unsafe {
                self.builder
                    .build_in_bounds_gep(self.ctx.i8_type(), buckets, &[off], "slot")
                    .unwrap()
            };
            let head = self.builder.build_load(ptr, slot, "head").unwrap();
            self.builder
                .build_store(self.hdr_field(ne, 24), head)
                .unwrap();
            self.builder.build_store(slot, ne).unwrap();
            let cnt = self
                .builder
                .build_load(i64t, hdr, "cnt")
                .unwrap()
                .into_int_value();
            let cnt1 = self
                .builder
                .build_int_add(cnt, i64t.const_int(1, false), "cnt1")
                .unwrap();
            self.builder.build_store(hdr, cnt1).unwrap();
            // load factor 0.75 を超えたら resize: count*4 > nbuckets*3
            let nb2 = self
                .builder
                .build_load(i64t, self.hdr_field(hdr, 8), "nb2")
                .unwrap()
                .into_int_value();
            let lhs = self
                .builder
                .build_int_mul(cnt1, i64t.const_int(4, false), "lf_lhs")
                .unwrap();
            let rhs = self
                .builder
                .build_int_mul(nb2, i64t.const_int(3, false), "lf_rhs")
                .unwrap();
            let need = self
                .builder
                .build_int_compare(IntPredicate::UGT, lhs, rhs, "need_resize")
                .unwrap();
            let resize_bb = self.ctx.append_basic_block(put_fn, "resize");
            let ret_bb = self.ctx.append_basic_block(put_fn, "ret");
            self.builder
                .build_conditional_branch(need, resize_bb, ret_bb)
                .unwrap();
            self.builder.position_at_end(resize_bb);
            self.builder
                .build_call(resize_fn, &[hdr.into()], "")
                .unwrap();
            self.builder.build_unconditional_branch(ret_bb).unwrap();
            self.builder.position_at_end(ret_bb);
            self.builder.build_return(None).unwrap();
        }

        // --- i64 lumo_map_get(ptr hdr, ptr key, i64 hash): 無ければ panic ---
        let get_fn = self.module.add_function(
            "lumo_map_get",
            i64t.fn_type(&[ptr.into(), ptr.into(), i64t.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(get_fn, "entry");
            let ok_bb = self.ctx.append_basic_block(get_fn, "ok");
            let miss_bb = self.ctx.append_basic_block(get_fn, "miss");
            let hdr = get_fn.get_nth_param(0).unwrap().into_pointer_value();
            let key = get_fn.get_nth_param(1).unwrap().into_pointer_value();
            let hash = get_fn.get_nth_param(2).unwrap().into_int_value();
            self.builder.position_at_end(entry);
            let e = self
                .builder
                .build_call(find_fn, &[hdr.into(), key.into(), hash.into()], "e")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let found = self.builder.build_is_not_null(e, "found").unwrap();
            self.builder
                .build_conditional_branch(found, ok_bb, miss_bb)
                .unwrap();
            self.builder.position_at_end(ok_bb);
            let v = self
                .builder
                .build_load(i64t, self.hdr_field(e, 16), "v")
                .unwrap();
            self.builder.build_return(Some(&v)).unwrap();
            self.builder.position_at_end(miss_bb);
            self.panic("lumo: key not found\n", "key_not_found_msg");
        }

        // --- i1 lumo_map_has(ptr hdr, ptr key, i64 hash) ---
        let has_fn = self.module.add_function(
            "lumo_map_has",
            i1t.fn_type(&[ptr.into(), ptr.into(), i64t.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(has_fn, "entry");
            let hdr = has_fn.get_nth_param(0).unwrap().into_pointer_value();
            let key = has_fn.get_nth_param(1).unwrap().into_pointer_value();
            let hash = has_fn.get_nth_param(2).unwrap().into_int_value();
            self.builder.position_at_end(entry);
            let e = self
                .builder
                .build_call(find_fn, &[hdr.into(), key.into(), hash.into()], "e")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let found = self.builder.build_is_not_null(e, "found").unwrap();
            self.builder.build_return(Some(&found)).unwrap();
        }

        // --- i64 lumo_map_len(ptr hdr) ---
        let len_fn =
            self.module
                .add_function("lumo_map_len", i64t.fn_type(&[ptr.into()], false), None);
        {
            let entry = self.ctx.append_basic_block(len_fn, "entry");
            let hdr = len_fn.get_nth_param(0).unwrap().into_pointer_value();
            self.builder.position_at_end(entry);
            let cnt = self.builder.build_load(i64t, hdr, "cnt").unwrap();
            self.builder.build_return(Some(&cnt)).unwrap();
        }

        // --- void lumo_map_del(ptr hdr, ptr key, i64 hash): あれば連結から外す ---
        let del_fn = self.module.add_function(
            "lumo_map_del",
            self.ctx
                .void_type()
                .fn_type(&[ptr.into(), ptr.into(), i64t.into()], false),
            None,
        );
        {
            let entry = self.ctx.append_basic_block(del_fn, "entry");
            let loop_bb = self.ctx.append_basic_block(del_fn, "loop");
            let check_bb = self.ctx.append_basic_block(del_fn, "check");
            let strcmp_bb = self.ctx.append_basic_block(del_fn, "strcmp");
            let unlink_bb = self.ctx.append_basic_block(del_fn, "unlink");
            let first_bb = self.ctx.append_basic_block(del_fn, "first");
            let mid_bb = self.ctx.append_basic_block(del_fn, "mid");
            let next_bb = self.ctx.append_basic_block(del_fn, "next");
            let ret_bb = self.ctx.append_basic_block(del_fn, "ret");
            let hdr = del_fn.get_nth_param(0).unwrap().into_pointer_value();
            let key = del_fn.get_nth_param(1).unwrap().into_pointer_value();
            let hash = del_fn.get_nth_param(2).unwrap().into_int_value();
            self.builder.position_at_end(entry);
            let nb = self
                .builder
                .build_load(i64t, self.hdr_field(hdr, 8), "nb")
                .unwrap()
                .into_int_value();
            let idx = self
                .builder
                .build_int_unsigned_rem(hash, nb, "idx")
                .unwrap();
            let buckets = self
                .builder
                .build_load(ptr, self.hdr_field(hdr, 16), "buckets")
                .unwrap()
                .into_pointer_value();
            let off = self
                .builder
                .build_int_mul(idx, i64t.const_int(8, false), "off")
                .unwrap();
            let slot = unsafe {
                self.builder
                    .build_in_bounds_gep(self.ctx.i8_type(), buckets, &[off], "slot")
                    .unwrap()
            };
            let prev_ptr = self.builder.build_alloca(ptr, "prev").unwrap();
            let e_ptr = self.builder.build_alloca(ptr, "e").unwrap();
            self.builder
                .build_store(prev_ptr, ptr.const_null())
                .unwrap();
            let head = self.builder.build_load(ptr, slot, "head").unwrap();
            self.builder.build_store(e_ptr, head).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            // loop: if e==null -> ret
            self.builder.position_at_end(loop_bb);
            let e = self
                .builder
                .build_load(ptr, e_ptr, "e")
                .unwrap()
                .into_pointer_value();
            let is_null = self.builder.build_is_null(e, "isnull").unwrap();
            self.builder
                .build_conditional_branch(is_null, ret_bb, check_bb)
                .unwrap();
            // check: hash match?
            self.builder.position_at_end(check_bb);
            let eh = self
                .builder
                .build_load(i64t, self.hdr_field(e, 8), "eh")
                .unwrap()
                .into_int_value();
            let hash_eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, eh, hash, "heq")
                .unwrap();
            self.builder
                .build_conditional_branch(hash_eq, strcmp_bb, next_bb)
                .unwrap();
            // strcmp
            self.builder.position_at_end(strcmp_bb);
            let ek = self
                .builder
                .build_load(ptr, self.hdr_field(e, 0), "ek")
                .unwrap();
            let strcmp = self.module.get_function("strcmp").unwrap();
            let cmp = self
                .builder
                .build_call(strcmp, &[ek.into(), key.into()], "cmp")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let key_eq = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    cmp,
                    self.ctx.i32_type().const_zero(),
                    "keq",
                )
                .unwrap();
            self.builder
                .build_conditional_branch(key_eq, unlink_bb, next_bb)
                .unwrap();
            // unlink: prev==null ? bucket head = e.next : prev.next = e.next ; count--
            self.builder.position_at_end(unlink_bb);
            let nx = self
                .builder
                .build_load(ptr, self.hdr_field(e, 24), "nx")
                .unwrap();
            let prev = self
                .builder
                .build_load(ptr, prev_ptr, "prev")
                .unwrap()
                .into_pointer_value();
            let prev_null = self.builder.build_is_null(prev, "prevnull").unwrap();
            let dec_bb = self.ctx.append_basic_block(del_fn, "dec");
            self.builder
                .build_conditional_branch(prev_null, first_bb, mid_bb)
                .unwrap();
            self.builder.position_at_end(first_bb);
            self.builder.build_store(slot, nx).unwrap();
            self.builder.build_unconditional_branch(dec_bb).unwrap();
            self.builder.position_at_end(mid_bb);
            self.builder
                .build_store(self.hdr_field(prev, 24), nx)
                .unwrap();
            self.builder.build_unconditional_branch(dec_bb).unwrap();
            // dec: count-- then return（取り除いたときだけ通る）
            self.builder.position_at_end(dec_bb);
            let cnt = self
                .builder
                .build_load(i64t, hdr, "cnt")
                .unwrap()
                .into_int_value();
            let cnt1 = self
                .builder
                .build_int_sub(cnt, i64t.const_int(1, false), "cnt1")
                .unwrap();
            self.builder.build_store(hdr, cnt1).unwrap();
            self.builder.build_unconditional_branch(ret_bb).unwrap();
            // next: prev=e; e=e.next; loop
            self.builder.position_at_end(next_bb);
            self.builder.build_store(prev_ptr, e).unwrap();
            let nx2 = self
                .builder
                .build_load(ptr, self.hdr_field(e, 24), "nx2")
                .unwrap();
            self.builder.build_store(e_ptr, nx2).unwrap();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
            // ret: 見つからなかった場合もここに来る
            self.builder.position_at_end(ret_bb);
            self.builder.build_return(None).unwrap();
        }

        // --- ptr lumo_map_keys(ptr hdr): 全キーを [string] 配列で返す ---
        let keys_fn =
            self.module
                .add_function("lumo_map_keys", ptr.fn_type(&[ptr.into()], false), None);
        {
            let entry = self.ctx.append_basic_block(keys_fn, "entry");
            let bloop = self.ctx.append_basic_block(keys_fn, "bloop");
            let bbody = self.ctx.append_basic_block(keys_fn, "bbody");
            let cloop = self.ctx.append_basic_block(keys_fn, "cloop");
            let cbody = self.ctx.append_basic_block(keys_fn, "cbody");
            let bnext = self.ctx.append_basic_block(keys_fn, "bnext");
            let done = self.ctx.append_basic_block(keys_fn, "done");
            let hdr = keys_fn.get_nth_param(0).unwrap().into_pointer_value();
            self.builder.position_at_end(entry);
            let cnt = self
                .builder
                .build_load(i64t, hdr, "cnt")
                .unwrap()
                .into_int_value();
            let alloc = self.module.get_function("lumo_alloc").unwrap();
            // 配列ヘッダ {len,cap,data}（v0.22 と同じレイアウト）
            let arr = self
                .builder
                .build_call(alloc, &[i64t.const_int(24, false).into()], "arr")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            self.builder.build_store(arr, cnt).unwrap();
            self.builder
                .build_store(self.hdr_field(arr, 8), cnt)
                .unwrap();
            let is_empty = self
                .builder
                .build_int_compare(IntPredicate::EQ, cnt, i64t.const_zero(), "empty")
                .unwrap();
            let bytes = self
                .builder
                .build_int_mul(cnt, i64t.const_int(8, false), "bytes")
                .unwrap();
            let data_alloc = self
                .builder
                .build_call(alloc, &[bytes.into()], "data")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let data = self
                .builder
                .build_select(is_empty, ptr.const_null(), data_alloc, "data")
                .unwrap()
                .into_pointer_value();
            self.builder
                .build_store(self.hdr_field(arr, 16), data)
                .unwrap();
            // b=0, w=0
            let b_ptr = self.builder.build_alloca(i64t, "b").unwrap();
            let w_ptr = self.builder.build_alloca(i64t, "w").unwrap();
            let e_ptr = self.builder.build_alloca(ptr, "e").unwrap();
            self.builder.build_store(b_ptr, i64t.const_zero()).unwrap();
            self.builder.build_store(w_ptr, i64t.const_zero()).unwrap();
            let nb = self
                .builder
                .build_load(i64t, self.hdr_field(hdr, 8), "nb")
                .unwrap()
                .into_int_value();
            let buckets = self
                .builder
                .build_load(ptr, self.hdr_field(hdr, 16), "buckets")
                .unwrap()
                .into_pointer_value();
            self.builder.build_unconditional_branch(bloop).unwrap();
            // bloop: if b>=nb done
            self.builder.position_at_end(bloop);
            let bv = self
                .builder
                .build_load(i64t, b_ptr, "b")
                .unwrap()
                .into_int_value();
            let b_done = self
                .builder
                .build_int_compare(IntPredicate::UGE, bv, nb, "bdone")
                .unwrap();
            self.builder
                .build_conditional_branch(b_done, done, bbody)
                .unwrap();
            // bbody: e = buckets[b]
            self.builder.position_at_end(bbody);
            let boff = self
                .builder
                .build_int_mul(bv, i64t.const_int(8, false), "boff")
                .unwrap();
            let bslot = unsafe {
                self.builder
                    .build_in_bounds_gep(self.ctx.i8_type(), buckets, &[boff], "bslot")
                    .unwrap()
            };
            let head = self.builder.build_load(ptr, bslot, "head").unwrap();
            self.builder.build_store(e_ptr, head).unwrap();
            self.builder.build_unconditional_branch(cloop).unwrap();
            // cloop: if e==null -> bnext
            self.builder.position_at_end(cloop);
            let e = self
                .builder
                .build_load(ptr, e_ptr, "e")
                .unwrap()
                .into_pointer_value();
            let e_null = self.builder.build_is_null(e, "enull").unwrap();
            self.builder
                .build_conditional_branch(e_null, bnext, cbody)
                .unwrap();
            // cbody: data[w] = e.key; w++; e = e.next
            self.builder.position_at_end(cbody);
            let wv = self
                .builder
                .build_load(i64t, w_ptr, "w")
                .unwrap()
                .into_int_value();
            let woff = self
                .builder
                .build_int_mul(wv, i64t.const_int(8, false), "woff")
                .unwrap();
            let waddr = unsafe {
                self.builder
                    .build_in_bounds_gep(self.ctx.i8_type(), data, &[woff], "waddr")
                    .unwrap()
            };
            let k = self
                .builder
                .build_load(ptr, self.hdr_field(e, 0), "k")
                .unwrap();
            self.builder.build_store(waddr, k).unwrap();
            let w1 = self
                .builder
                .build_int_add(wv, i64t.const_int(1, false), "w1")
                .unwrap();
            self.builder.build_store(w_ptr, w1).unwrap();
            let nx = self
                .builder
                .build_load(ptr, self.hdr_field(e, 24), "nx")
                .unwrap();
            self.builder.build_store(e_ptr, nx).unwrap();
            self.builder.build_unconditional_branch(cloop).unwrap();
            // bnext: b++
            self.builder.position_at_end(bnext);
            let b1 = self
                .builder
                .build_int_add(bv, i64t.const_int(1, false), "b1")
                .unwrap();
            self.builder.build_store(b_ptr, b1).unwrap();
            self.builder.build_unconditional_branch(bloop).unwrap();
            // done: return arr
            self.builder.position_at_end(done);
            self.builder.build_return(Some(&arr)).unwrap();
        }
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
                let (v, vty) = self.gen_expr(value);
                match &target.kind {
                    ExprKind::Var(name) => {
                        let (ptr, _) = self.lookup_var(name);
                        self.builder.build_store(ptr, v).unwrap();
                    }
                    ExprKind::Index { array, index } => {
                        let (arr, arr_ty) = self.gen_expr(array);
                        match arr_ty {
                            // map への代入: m[k] = v -> lumo_map_put
                            Type::Map(_) => {
                                let hdr = arr.into_pointer_value();
                                self.null_check(hdr);
                                let (kv, _) = self.gen_expr(index);
                                let key = kv.into_pointer_value();
                                let h = self.str_hash(key);
                                let raw = self.to_slot_i64(v, vty);
                                let put = self.module.get_function("lumo_map_put").unwrap();
                                self.builder
                                    .build_call(
                                        put,
                                        &[hdr.into(), key.into(), h.into(), raw.into()],
                                        "",
                                    )
                                    .unwrap();
                            }
                            _ => {
                                let addr = self.elem_addr(arr.into_pointer_value(), index);
                                self.builder.build_store(addr, v).unwrap();
                            }
                        }
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
                    Type::Array(_) | Type::Struct(_) | Type::Map(_) | Type::Null => {
                        unreachable!("typeck forbids printing arrays/structs/maps/null")
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
            StmtKind::ForIn { var, iter, body } => {
                // 反復対象を配列ヘッダに正規化する（map はキー配列を作る）。要素型も決める。
                let (iv, ity) = self.gen_expr(iter);
                let (arr, elem_ty) = match ity {
                    Type::Array(e) => (iv.into_pointer_value(), e.to_type()),
                    Type::Map(_) => {
                        let keysf = self.module.get_function("lumo_map_keys").unwrap();
                        let ks = self
                            .builder
                            .build_call(keysf, &[iv.into()], "keys")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_pointer_value();
                        (ks, Type::Str)
                    }
                    _ => unreachable!("typeck guarantees an array or map here"),
                };
                self.null_check(arr);
                let i64t = self.ctx.i64_type();
                let ptrt = self.ctx.ptr_type(AddressSpace::default());
                // len / data はループ開始時に1度だけ読む（長さスナップショット）
                let len = self
                    .builder
                    .build_load(i64t, arr, "len")
                    .unwrap()
                    .into_int_value();
                let data = self
                    .builder
                    .build_load(ptrt, self.hdr_field(arr, 16), "data")
                    .unwrap()
                    .into_pointer_value();
                let i_ptr = self.builder.build_alloca(i64t, "i").unwrap();
                self.builder.build_store(i_ptr, i64t.const_zero()).unwrap();
                // ループ変数を専用スコープに束縛する
                self.push_scope();
                let var_alloca = self
                    .builder
                    .build_alloca(self.basic_ty(elem_ty), var)
                    .unwrap();
                self.declare_var(var, var_alloca, elem_ty);

                let cond_bb = self.ctx.append_basic_block(function, "forin.cond");
                let body_bb = self.ctx.append_basic_block(function, "forin.body");
                let step_bb = self.ctx.append_basic_block(function, "forin.step");
                let end_bb = self.ctx.append_basic_block(function, "forin.end");
                self.builder.build_unconditional_branch(cond_bb).unwrap();

                // cond: i < len
                self.builder.position_at_end(cond_bb);
                let iv = self
                    .builder
                    .build_load(i64t, i_ptr, "i")
                    .unwrap()
                    .into_int_value();
                let c = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, iv, len, "cmp")
                    .unwrap();
                self.builder
                    .build_conditional_branch(c, body_bb, end_bb)
                    .unwrap();

                // body: var = data[i]; 本体（continue は step へ、break は末尾へ）
                self.builder.position_at_end(body_bb);
                let off = self
                    .builder
                    .build_int_mul(iv, i64t.const_int(8, false), "off")
                    .unwrap();
                let addr = unsafe {
                    self.builder
                        .build_in_bounds_gep(self.ctx.i8_type(), data, &[off], "slot")
                        .unwrap()
                };
                let el = self
                    .builder
                    .build_load(self.basic_ty(elem_ty), addr, "el")
                    .unwrap();
                self.builder.build_store(var_alloca, el).unwrap();
                self.loop_stack.push((step_bb, end_bb));
                self.push_scope();
                self.gen_block(body, function);
                self.pop_scope();
                self.loop_stack.pop();
                if self.block_open() {
                    self.builder.build_unconditional_branch(step_bb).unwrap();
                }

                // step: i++
                self.builder.position_at_end(step_bb);
                let iv2 = self
                    .builder
                    .build_load(i64t, i_ptr, "i")
                    .unwrap()
                    .into_int_value();
                let i1 = self
                    .builder
                    .build_int_add(iv2, i64t.const_int(1, false), "i1")
                    .unwrap();
                self.builder.build_store(i_ptr, i1).unwrap();
                self.builder.build_unconditional_branch(cond_bb).unwrap();

                self.builder.position_at_end(end_bb);
                self.pop_scope(); // ループ変数のスコープ
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
                // float -> int は切り捨て、string -> int はパース（不正なら panic）、int は恒等
                let (v, ty) = self.gen_expr(&args[0]);
                match ty {
                    Type::Float => {
                        let i = self
                            .builder
                            .build_float_to_signed_int(
                                v.into_float_value(),
                                self.ctx.i64_type(),
                                "toint",
                            )
                            .unwrap();
                        (i.into(), Type::Int)
                    }
                    Type::Str => {
                        let f = self.module.get_function("lumo_parse_int").unwrap();
                        let r = self
                            .builder
                            .build_call(f, &[v.into()], "parseint")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic();
                        (r, Type::Int)
                    }
                    _ => (v, Type::Int),
                }
            }
            ExprKind::Call { name, args } if name == "float" => {
                // int -> float、string -> float はパース（不正なら panic）、float は恒等
                let (v, ty) = self.gen_expr(&args[0]);
                match ty {
                    Type::Int => {
                        let f = self
                            .builder
                            .build_signed_int_to_float(
                                v.into_int_value(),
                                self.ctx.f64_type(),
                                "tofloat",
                            )
                            .unwrap();
                        (f.into(), Type::Float)
                    }
                    Type::Str => {
                        let f = self.module.get_function("lumo_parse_float").unwrap();
                        let r = self
                            .builder
                            .build_call(f, &[v.into()], "parsefloat")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic();
                        (r, Type::Float)
                    }
                    _ => (v, Type::Float),
                }
            }
            ExprKind::Call { name, args } if name == "is_int" || name == "is_float" => {
                let (v, _) = self.gen_expr(&args[0]);
                let f = self
                    .module
                    .get_function(if name == "is_int" {
                        "lumo_is_int"
                    } else {
                        "lumo_is_float"
                    })
                    .unwrap();
                let r = self
                    .builder
                    .build_call(f, &[v.into()], "isnum")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Bool)
            }
            ExprKind::Call { name, args } if matches!(name.as_str(), "sqrt" | "floor" | "ceil") => {
                // float -> float の単項数学関数（intrinsic 呼び出し）
                let (v, _) = self.gen_expr(&args[0]);
                let intr = match name.as_str() {
                    "sqrt" => "llvm.sqrt.f64",
                    "floor" => "llvm.floor.f64",
                    "ceil" => "llvm.ceil.f64",
                    _ => unreachable!(),
                };
                let f = self.module.get_function(intr).unwrap();
                let r = self
                    .builder
                    .build_call(f, &[v.into_float_value().into()], "m")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Float)
            }
            ExprKind::Call { name, args } if name == "pow" => {
                // pow(base, exp): libm の pow を呼ぶ
                let (b, _) = self.gen_expr(&args[0]);
                let (e, _) = self.gen_expr(&args[1]);
                let f = self.module.get_function("pow").unwrap();
                let r = self
                    .builder
                    .build_call(
                        f,
                        &[b.into_float_value().into(), e.into_float_value().into()],
                        "pow",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Float)
            }
            ExprKind::Call { name, args } if name == "abs" => {
                // abs(x): float は llvm.fabs、int は select(x<0, -x, x)
                let (v, ty) = self.gen_expr(&args[0]);
                if ty == Type::Float {
                    let f = self.module.get_function("llvm.fabs.f64").unwrap();
                    let r = self
                        .builder
                        .build_call(f, &[v.into_float_value().into()], "abs")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic();
                    (r, Type::Float)
                } else {
                    let x = v.into_int_value();
                    let neg = self.builder.build_int_neg(x, "neg").unwrap();
                    let isneg = self
                        .builder
                        .build_int_compare(
                            IntPredicate::SLT,
                            x,
                            self.ctx.i64_type().const_zero(),
                            "isneg",
                        )
                        .unwrap();
                    let r = self.builder.build_select(isneg, neg, x, "abs").unwrap();
                    (r, Type::Int)
                }
            }
            ExprKind::Call { name, args } if name == "min" || name == "max" => {
                // min/max(a, b): float は llvm.minnum/maxnum、int は select
                let (a, ty) = self.gen_expr(&args[0]);
                let (b, _) = self.gen_expr(&args[1]);
                let want_min = name == "min";
                if ty == Type::Float {
                    let intr = if want_min {
                        "llvm.minnum.f64"
                    } else {
                        "llvm.maxnum.f64"
                    };
                    let f = self.module.get_function(intr).unwrap();
                    let r = self
                        .builder
                        .build_call(
                            f,
                            &[a.into_float_value().into(), b.into_float_value().into()],
                            "mm",
                        )
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic();
                    (r, Type::Float)
                } else {
                    let av = a.into_int_value();
                    let bv = b.into_int_value();
                    let pred = if want_min {
                        IntPredicate::SLT
                    } else {
                        IntPredicate::SGT
                    };
                    let cmp = self
                        .builder
                        .build_int_compare(pred, av, bv, "mmcmp")
                        .unwrap();
                    let r = self.builder.build_select(cmp, av, bv, "mm").unwrap();
                    (r, Type::Int)
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
                    // 配列・map とも長さ/要素数は先頭ヘッダの i64（同じオフセット0）
                    _ => {
                        let n = self
                            .builder
                            .build_load(self.ctx.i64_type(), ptr, "len")
                            .unwrap();
                        (n, Type::Int)
                    }
                }
            }
            ExprKind::Call { name, args } if name == "has" => {
                let (mv, _) = self.gen_expr(&args[0]);
                let hdr = mv.into_pointer_value();
                self.null_check(hdr);
                let (kv, _) = self.gen_expr(&args[1]);
                let key = kv.into_pointer_value();
                let h = self.str_hash(key);
                let f = self.module.get_function("lumo_map_has").unwrap();
                let r = self
                    .builder
                    .build_call(f, &[hdr.into(), key.into(), h.into()], "has")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Bool)
            }
            ExprKind::Call { name, args } if name == "delete" => {
                let (mv, _) = self.gen_expr(&args[0]);
                let hdr = mv.into_pointer_value();
                self.null_check(hdr);
                let (kv, _) = self.gen_expr(&args[1]);
                let key = kv.into_pointer_value();
                let h = self.str_hash(key);
                let f = self.module.get_function("lumo_map_del").unwrap();
                self.builder
                    .build_call(f, &[hdr.into(), key.into(), h.into()], "")
                    .unwrap();
                // 文として使う想定。式の値は便宜上 int 0。
                (self.ctx.i64_type().const_zero().into(), Type::Int)
            }
            ExprKind::Call { name, args } if name == "keys" => {
                let (mv, _) = self.gen_expr(&args[0]);
                let hdr = mv.into_pointer_value();
                self.null_check(hdr);
                let f = self.module.get_function("lumo_map_keys").unwrap();
                let arr = self
                    .builder
                    .build_call(f, &[hdr.into()], "keys")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (arr, Type::Array(crate::types::Elem::Str))
            }
            ExprKind::Call { name, args } if name == "substr" => {
                // substr(s, start, count): 範囲を検査してから lumo_substr を呼ぶ
                let i64t = self.ctx.i64_type();
                let (sv, _) = self.gen_expr(&args[0]);
                let s = sv.into_pointer_value();
                let (startv, _) = self.gen_expr(&args[1]);
                let start = startv.into_int_value();
                let (countv, _) = self.gen_expr(&args[2]);
                let count = countv.into_int_value();
                let strlen = self.module.get_function("strlen").unwrap();
                let len = self
                    .builder
                    .build_call(strlen, &[s.into()], "len")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let zero = i64t.const_zero();
                let c1 = self
                    .builder
                    .build_int_compare(IntPredicate::SGE, start, zero, "c1")
                    .unwrap();
                let c2 = self
                    .builder
                    .build_int_compare(IntPredicate::SGE, count, zero, "c2")
                    .unwrap();
                let sc = self.builder.build_int_add(start, count, "sc").unwrap();
                let c3 = self
                    .builder
                    .build_int_compare(IntPredicate::SLE, sc, len, "c3")
                    .unwrap();
                let ok12 = self.builder.build_and(c1, c2, "ok12").unwrap();
                let ok = self.builder.build_and(ok12, c3, "ok").unwrap();
                let function = self.cur_function();
                let fail_bb = self.ctx.append_basic_block(function, "substr.fail");
                let ok_bb = self.ctx.append_basic_block(function, "substr.ok");
                self.builder
                    .build_conditional_branch(ok, ok_bb, fail_bb)
                    .unwrap();
                self.builder.position_at_end(fail_bb);
                self.panic("lumo: substr out of range\n", "substr_oob_msg");
                self.builder.position_at_end(ok_bb);
                let f = self.module.get_function("lumo_substr").unwrap();
                let r = self
                    .builder
                    .build_call(f, &[s.into(), start.into(), count.into()], "substr")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Str)
            }
            ExprKind::Call { name, args } if name == "split" => {
                let (sv, _) = self.gen_expr(&args[0]);
                let (sepv, _) = self.gen_expr(&args[1]);
                let f = self.module.get_function("lumo_split").unwrap();
                let r = self
                    .builder
                    .build_call(f, &[sv.into(), sepv.into()], "split")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Array(crate::types::Elem::Str))
            }
            ExprKind::Call { name, args } if name == "join" => {
                let (av, _) = self.gen_expr(&args[0]);
                let arr = av.into_pointer_value();
                self.null_check(arr);
                let (sepv, _) = self.gen_expr(&args[1]);
                let f = self.module.get_function("lumo_join").unwrap();
                let r = self
                    .builder
                    .build_call(f, &[arr.into(), sepv.into()], "join")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Str)
            }
            ExprKind::Call { name, args } if name == "to_upper" || name == "to_lower" => {
                // to_upper/to_lower: lumo_to_case を upper フラグ付きで呼ぶ
                let (sv, _) = self.gen_expr(&args[0]);
                let upper = self
                    .ctx
                    .bool_type()
                    .const_int((name == "to_upper") as u64, false);
                let f = self.module.get_function("lumo_to_case").unwrap();
                let r = self
                    .builder
                    .build_call(f, &[sv.into(), upper.into()], "tocase")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Str)
            }
            ExprKind::Call { name, args } if name == "trim" => {
                let (sv, _) = self.gen_expr(&args[0]);
                let f = self.module.get_function("lumo_trim").unwrap();
                let r = self
                    .builder
                    .build_call(f, &[sv.into()], "trim")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Str)
            }
            ExprKind::Call { name, args } if name == "find" || name == "contains" => {
                // find は位置(int)、contains は find>=0 の bool
                let (sv, _) = self.gen_expr(&args[0]);
                let (subv, _) = self.gen_expr(&args[1]);
                let f = self.module.get_function("lumo_find").unwrap();
                let pos = self
                    .builder
                    .build_call(f, &[sv.into(), subv.into()], "find")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                if name == "find" {
                    (pos.into(), Type::Int)
                } else {
                    let found = self
                        .builder
                        .build_int_compare(
                            IntPredicate::SGE,
                            pos,
                            self.ctx.i64_type().const_zero(),
                            "contains",
                        )
                        .unwrap();
                    (found.into(), Type::Bool)
                }
            }
            ExprKind::Call { name, args } if name == "starts_with" || name == "ends_with" => {
                let (sv, _) = self.gen_expr(&args[0]);
                let (av, _) = self.gen_expr(&args[1]);
                let at_end = self
                    .ctx
                    .bool_type()
                    .const_int((name == "ends_with") as u64, false);
                let f = self.module.get_function("lumo_str_has_affix").unwrap();
                let r = self
                    .builder
                    .build_call(f, &[sv.into(), av.into(), at_end.into()], "affix")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Bool)
            }
            ExprKind::Call { name, args } if name == "replace" => {
                // replace(s, from, to) == join(split(s, from), to)。split は空 from で
                // [s] を返す（無限ループにならない）ので、全ケースを既存実装が処理する。
                let (sv, _) = self.gen_expr(&args[0]);
                let (fromv, _) = self.gen_expr(&args[1]);
                let (tov, _) = self.gen_expr(&args[2]);
                let split = self.module.get_function("lumo_split").unwrap();
                let parts = self
                    .builder
                    .build_call(split, &[sv.into(), fromv.into()], "rsplit")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                let join = self.module.get_function("lumo_join").unwrap();
                let r = self
                    .builder
                    .build_call(join, &[parts.into(), tov.into()], "rjoin")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Str)
            }
            ExprKind::Call { name, args } if name == "repeat" => {
                let (sv, _) = self.gen_expr(&args[0]);
                let (nv, _) = self.gen_expr(&args[1]);
                let f = self.module.get_function("lumo_repeat").unwrap();
                let r = self
                    .builder
                    .build_call(f, &[sv.into(), nv.into()], "repeat")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Str)
            }
            ExprKind::Call { name, args } if name == "read_file" => {
                // read_file(path): ファイル全体を文字列で返す。開けなければ null。
                let (pv, _) = self.gen_expr(&args[0]);
                let f = self.module.get_function("lumo_read_file").unwrap();
                let r = self
                    .builder
                    .build_call(f, &[pv.into()], "rdfile")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Str)
            }
            ExprKind::Call { name, args } if name == "write_file" => {
                // write_file(path, content): 書き込み成功なら true。
                let (pv, _) = self.gen_expr(&args[0]);
                let (cv, _) = self.gen_expr(&args[1]);
                let f = self.module.get_function("lumo_write_file").unwrap();
                let r = self
                    .builder
                    .build_call(f, &[pv.into(), cv.into()], "wrfile")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, Type::Bool)
            }
            ExprKind::Call { name, args } if name == "push" => {
                // push(arr, x): 末尾に x を追加し、配列(ヘッダ)を返す。容量が足りなければ
                // data ブロックを realloc で倍増する。ヘッダのポインタは不変。
                let i64t = self.ctx.i64_type();
                let ptr = self.ctx.ptr_type(AddressSpace::default());
                let (arr_v, arr_ty) = self.gen_expr(&args[0]);
                let hdr = arr_v.into_pointer_value();
                self.null_check(hdr);
                let (val, _) = self.gen_expr(&args[1]);
                // 現在の len / cap を読む
                let len = self
                    .builder
                    .build_load(i64t, hdr, "len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64t, self.hdr_field(hdr, 8), "cap")
                    .unwrap()
                    .into_int_value();
                // len < cap なら容量十分。そうでなければ grow する。
                let enough = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, len, cap, "enough")
                    .unwrap();
                let function = self.cur_function();
                let grow_bb = self.ctx.append_basic_block(function, "push.grow");
                let store_bb = self.ctx.append_basic_block(function, "push.store");
                self.builder
                    .build_conditional_branch(enough, store_bb, grow_bb)
                    .unwrap();
                // grow: newcap = if cap==0 {4} else {cap*2}; data = realloc(data, 8*newcap)
                self.builder.position_at_end(grow_bb);
                let data_old = self
                    .builder
                    .build_load(ptr, self.hdr_field(hdr, 16), "data.old")
                    .unwrap()
                    .into_pointer_value();
                let is_zero = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, cap, i64t.const_zero(), "cap0")
                    .unwrap();
                let doubled = self
                    .builder
                    .build_int_mul(cap, i64t.const_int(2, false), "cap2")
                    .unwrap();
                let newcap = self
                    .builder
                    .build_select(is_zero, i64t.const_int(4, false), doubled, "newcap")
                    .unwrap()
                    .into_int_value();
                let newsize = self
                    .builder
                    .build_int_mul(newcap, i64t.const_int(8, false), "newsize")
                    .unwrap();
                let realloc = self.module.get_function("realloc").unwrap();
                let newdata = self
                    .builder
                    .build_call(realloc, &[data_old.into(), newsize.into()], "rdata")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                self.builder
                    .build_store(self.hdr_field(hdr, 16), newdata)
                    .unwrap();
                self.builder
                    .build_store(self.hdr_field(hdr, 8), newcap)
                    .unwrap();
                self.builder.build_unconditional_branch(store_bb).unwrap();
                // store: data[len] = val; len = len + 1
                self.builder.position_at_end(store_bb);
                let data = self
                    .builder
                    .build_load(ptr, self.hdr_field(hdr, 16), "data")
                    .unwrap()
                    .into_pointer_value();
                let off = self
                    .builder
                    .build_int_mul(len, i64t.const_int(8, false), "off")
                    .unwrap();
                let slot = unsafe {
                    self.builder
                        .build_in_bounds_gep(self.ctx.i8_type(), data, &[off], "slot")
                        .unwrap()
                };
                self.builder.build_store(slot, val).unwrap();
                let newlen = self
                    .builder
                    .build_int_add(len, i64t.const_int(1, false), "newlen")
                    .unwrap();
                self.builder.build_store(hdr, newlen).unwrap();
                (hdr.into(), arr_ty)
            }
            ExprKind::Call { name, args } if name == "pop" => {
                // pop(a): 末尾要素を取り除いて返す。len を1減らすだけ（data はそのまま）。
                let i64t = self.ctx.i64_type();
                let ptr = self.ctx.ptr_type(AddressSpace::default());
                let (arr_v, arr_ty) = self.gen_expr(&args[0]);
                let Type::Array(elem) = arr_ty else {
                    unreachable!("typeck guarantees an array here");
                };
                let hdr = arr_v.into_pointer_value();
                self.null_check(hdr);
                let len = self
                    .builder
                    .build_load(i64t, hdr, "len")
                    .unwrap()
                    .into_int_value();
                // 空配列からの pop は実行時エラー
                let empty = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, len, i64t.const_zero(), "empty")
                    .unwrap();
                let function = self.cur_function();
                let fail_bb = self.ctx.append_basic_block(function, "pop.empty");
                let ok_bb = self.ctx.append_basic_block(function, "pop.ok");
                self.builder
                    .build_conditional_branch(empty, fail_bb, ok_bb)
                    .unwrap();
                self.builder.position_at_end(fail_bb);
                self.panic("lumo: pop from empty array\n", "pop_empty_msg");
                self.builder.position_at_end(ok_bb);
                let newlen = self
                    .builder
                    .build_int_sub(len, i64t.const_int(1, false), "newlen")
                    .unwrap();
                self.builder.build_store(hdr, newlen).unwrap();
                let data = self
                    .builder
                    .build_load(ptr, self.hdr_field(hdr, 16), "data")
                    .unwrap()
                    .into_pointer_value();
                let off = self
                    .builder
                    .build_int_mul(newlen, i64t.const_int(8, false), "off")
                    .unwrap();
                let slot = unsafe {
                    self.builder
                        .build_in_bounds_gep(self.ctx.i8_type(), data, &[off], "slot")
                        .unwrap()
                };
                let v = self
                    .builder
                    .build_load(self.basic_ty(elem.to_type()), slot, "popped")
                    .unwrap();
                (v, elem.to_type())
            }
            ExprKind::Call { name, args } if name == "sorted" => {
                // sorted(a): コピーして qsort。コンパレータは要素型で選ぶ。
                let (av, arr_ty) = self.gen_expr(&args[0]);
                let hdr = av.into_pointer_value();
                self.null_check(hdr);
                let Type::Array(elem) = arr_ty else {
                    unreachable!("typeck guarantees an array here");
                };
                let cmp_name = match elem.to_type() {
                    Type::Int => "lumo_cmp_int",
                    Type::Float => "lumo_cmp_float",
                    Type::Str => "lumo_cmp_str",
                    _ => unreachable!("typeck restricts sorted to int/float/string"),
                };
                let cmp = self
                    .module
                    .get_function(cmp_name)
                    .unwrap()
                    .as_global_value()
                    .as_pointer_value();
                let f = self.module.get_function("lumo_array_sort").unwrap();
                let r = self
                    .builder
                    .build_call(f, &[hdr.into(), cmp.into()], "sorted")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, arr_ty)
            }
            ExprKind::Call { name, args } if name == "reversed" => {
                let (av, arr_ty) = self.gen_expr(&args[0]);
                let hdr = av.into_pointer_value();
                self.null_check(hdr);
                let f = self.module.get_function("lumo_array_reverse").unwrap();
                let r = self
                    .builder
                    .build_call(f, &[hdr.into()], "reversed")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                (r, arr_ty)
            }
            ExprKind::Call { name, .. } if name == "read_line" => {
                // stdin から1行読む。EOF なら null、そうでなければ末尾改行を取り除いた文字列。
                let n = 4096u64;
                let alloc = self.module.get_function("lumo_alloc").unwrap();
                let buf = self
                    .builder
                    .build_call(
                        alloc,
                        &[self.ctx.i64_type().const_int(n, false).into()],
                        "linebuf",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let stdin_g = self.module.get_global(stdin_symbol()).unwrap();
                let stdin_val = self
                    .builder
                    .build_load(
                        self.ctx.ptr_type(AddressSpace::default()),
                        stdin_g.as_pointer_value(),
                        "stdin",
                    )
                    .unwrap();
                let fgets = self.module.get_function("fgets").unwrap();
                let r = self
                    .builder
                    .build_call(
                        fgets,
                        &[
                            buf.into(),
                            self.ctx.i32_type().const_int(n, false).into(),
                            stdin_val.into(),
                        ],
                        "line",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                // r が null でなければ末尾改行を strcspn で切り落とす。r 自体(buf or null)を返す。
                let isnull = self.builder.build_is_null(r, "eof").unwrap();
                let function = self.cur_function();
                let strip_bb = self.ctx.append_basic_block(function, "rl.strip");
                let after_bb = self.ctx.append_basic_block(function, "rl.after");
                self.builder
                    .build_conditional_branch(isnull, after_bb, strip_bb)
                    .unwrap();
                self.builder.position_at_end(strip_bb);
                let nl = self.global_str("\n", "nl_str");
                let strcspn = self.module.get_function("strcspn").unwrap();
                let idx = self
                    .builder
                    .build_call(strcspn, &[buf.into(), nl.into()], "nlpos")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let addr = unsafe {
                    self.builder
                        .build_in_bounds_gep(self.ctx.i8_type(), buf, &[idx], "nladdr")
                        .unwrap()
                };
                self.builder
                    .build_store(addr, self.ctx.i8_type().const_int(0, false))
                    .unwrap();
                self.builder.build_unconditional_branch(after_bb).unwrap();
                self.builder.position_at_end(after_bb);
                (r.into(), Type::Str)
            }
            ExprKind::Call { name, args } if name == "chr" => {
                // バイト値を 1 文字（NUL 終端）のヒープ文字列にする
                let (v, _) = self.gen_expr(&args[0]);
                let byte = self
                    .builder
                    .build_int_truncate(v.into_int_value(), self.ctx.i8_type(), "chrbyte")
                    .unwrap();
                let alloc = self.module.get_function("lumo_alloc").unwrap();
                let buf = self
                    .builder
                    .build_call(
                        alloc,
                        &[self.ctx.i64_type().const_int(2, false).into()],
                        "chrbuf",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                self.builder.build_store(buf, byte).unwrap();
                let nul_addr = unsafe {
                    self.builder
                        .build_in_bounds_gep(
                            self.ctx.i8_type(),
                            buf,
                            &[self.ctx.i64_type().const_int(1, false)],
                            "nuladdr",
                        )
                        .unwrap()
                };
                self.builder
                    .build_store(nul_addr, self.ctx.i8_type().const_int(0, false))
                    .unwrap();
                (buf.into(), Type::Str)
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
                // 配列は二重間接: 値はヘッダ {len i64, cap i64, data ptr} を指す。
                // data は cap 個の 8byte スロットを持つ別ブロック（push が realloc で伸ばす）。
                // ヘッダのポインタは不変なので、push 後もエイリアスは新しい data を見られる。
                let i64t = self.ctx.i64_type();
                let n = elems.len() as u64;
                let alloc = self.module.get_function("lumo_alloc").unwrap();
                let hdr = self
                    .builder
                    .build_call(alloc, &[i64t.const_int(24, false).into()], "arr.hdr")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                // data ブロックを確保（空配列なら null）
                let data = if n == 0 {
                    self.ctx.ptr_type(AddressSpace::default()).const_null()
                } else {
                    self.builder
                        .build_call(alloc, &[i64t.const_int(8 * n, false).into()], "arr.data")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_pointer_value()
                };
                // ヘッダに len / cap / data を書き込む（生成直後は len==cap==n）
                self.builder
                    .build_store(hdr, i64t.const_int(n, false))
                    .unwrap();
                self.builder
                    .build_store(self.hdr_field(hdr, 8), i64t.const_int(n, false))
                    .unwrap();
                self.builder
                    .build_store(self.hdr_field(hdr, 16), data)
                    .unwrap();
                // 各要素を data の 8*i バイト目へ
                let mut elem_type = Type::Int;
                for (i, el) in elems.iter().enumerate() {
                    let (val, ty) = self.gen_expr(el);
                    if i == 0 {
                        elem_type = ty;
                    }
                    let off = i64t.const_int(8 * (i as u64), false);
                    let addr = unsafe {
                        self.builder
                            .build_in_bounds_gep(self.ctx.i8_type(), data, &[off], "slot")
                            .unwrap()
                    };
                    self.builder.build_store(addr, val).unwrap();
                }
                // 空配列の要素型は注釈から決まる（typeck が保証）。ここでは Int を仮置き。
                (hdr.into(), Type::Array(elem_type.as_elem().unwrap()))
            }
            ExprKind::Index { array, index } => {
                let (arr, arr_ty) = self.gen_expr(array);
                match arr_ty {
                    Type::Array(elem) => {
                        let addr = self.elem_addr(arr.into_pointer_value(), index);
                        let v = self
                            .builder
                            .build_load(self.basic_ty(elem.to_type()), addr, "idx")
                            .unwrap();
                        (v, elem.to_type())
                    }
                    // 文字列の添字: バイトを読み i64 にゼロ拡張する
                    Type::Str => {
                        let addr = self.str_byte_addr(arr.into_pointer_value(), index);
                        let byte = self
                            .builder
                            .build_load(self.ctx.i8_type(), addr, "byte")
                            .unwrap()
                            .into_int_value();
                        let v = self
                            .builder
                            .build_int_z_extend(byte, self.ctx.i64_type(), "byte64")
                            .unwrap();
                        (v.into(), Type::Int)
                    }
                    // map の添字: キーで lumo_map_get（無ければ実行時 panic）
                    Type::Map(velem) => {
                        let hdr = arr.into_pointer_value();
                        self.null_check(hdr);
                        let (kv, _) = self.gen_expr(index);
                        let key = kv.into_pointer_value();
                        let h = self.str_hash(key);
                        let getf = self.module.get_function("lumo_map_get").unwrap();
                        let raw = self
                            .builder
                            .build_call(getf, &[hdr.into(), key.into(), h.into()], "mget")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_int_value();
                        (self.slot_to_value(raw, velem.to_type()), velem.to_type())
                    }
                    _ => unreachable!("typeck guarantees an array, string, or map here"),
                }
            }
            ExprKind::Slice { seq, lo, hi } => {
                let i64t = self.ctx.i64_type();
                let (sv, sty) = self.gen_expr(seq);
                let base = sv.into_pointer_value();
                self.null_check(base);
                // 長さ: 配列はヘッダ先頭の i64、文字列は strlen
                let len = match sty {
                    Type::Array(_) => self
                        .builder
                        .build_load(i64t, base, "len")
                        .unwrap()
                        .into_int_value(),
                    Type::Str => {
                        let strlen = self.module.get_function("strlen").unwrap();
                        self.builder
                            .build_call(strlen, &[base.into()], "len")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_int_value()
                    }
                    _ => unreachable!("typeck guarantees array or string here"),
                };
                // lo 省略は 0、hi 省略は長さ
                let lo_v = match lo {
                    Some(e) => self.gen_expr(e).0.into_int_value(),
                    None => i64t.const_zero(),
                };
                let hi_v = match hi {
                    Some(e) => self.gen_expr(e).0.into_int_value(),
                    None => len,
                };
                // 0 <= lo <= hi <= len を検査（外れたら panic）
                let c1 = self
                    .builder
                    .build_int_compare(IntPredicate::SGE, lo_v, i64t.const_zero(), "c1")
                    .unwrap();
                let c2 = self
                    .builder
                    .build_int_compare(IntPredicate::SLE, lo_v, hi_v, "c2")
                    .unwrap();
                let c3 = self
                    .builder
                    .build_int_compare(IntPredicate::SLE, hi_v, len, "c3")
                    .unwrap();
                let ok12 = self.builder.build_and(c1, c2, "ok12").unwrap();
                let ok = self.builder.build_and(ok12, c3, "ok").unwrap();
                let function = self.cur_function();
                let fail_bb = self.ctx.append_basic_block(function, "slice.fail");
                let ok_bb = self.ctx.append_basic_block(function, "slice.ok");
                self.builder
                    .build_conditional_branch(ok, ok_bb, fail_bb)
                    .unwrap();
                self.builder.position_at_end(fail_bb);
                self.panic("lumo: slice out of range\n", "slice_oob_msg");
                self.builder.position_at_end(ok_bb);
                match sty {
                    Type::Array(_) => {
                        let f = self.module.get_function("lumo_array_slice").unwrap();
                        let r = self
                            .builder
                            .build_call(f, &[base.into(), lo_v.into(), hi_v.into()], "slice")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic();
                        (r, sty)
                    }
                    Type::Str => {
                        let count = self.builder.build_int_sub(hi_v, lo_v, "count").unwrap();
                        let f = self.module.get_function("lumo_substr").unwrap();
                        let r = self
                            .builder
                            .build_call(f, &[base.into(), lo_v.into(), count.into()], "sslice")
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic();
                        (r, Type::Str)
                    }
                    _ => unreachable!("typeck guarantees array or string here"),
                }
            }
            ExprKind::MapLit(pairs) => {
                // 空ヘッダを作り、各ペアを put する。値型は最初のペアから（空は注釈で決まる）
                let new = self.module.get_function("lumo_map_new").unwrap();
                let hdr = self
                    .builder
                    .build_call(new, &[], "map")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let put = self.module.get_function("lumo_map_put").unwrap();
                let mut velem = Type::Int; // 空のときの仮置き（Let が注釈型で上書き）
                for (i, (k, v)) in pairs.iter().enumerate() {
                    let (kv, _) = self.gen_expr(k);
                    let key = kv.into_pointer_value();
                    let (vv, vty) = self.gen_expr(v);
                    if i == 0 {
                        velem = vty;
                    }
                    let h = self.str_hash(key);
                    let raw = self.to_slot_i64(vv, vty);
                    self.builder
                        .build_call(put, &[hdr.into(), key.into(), h.into(), raw.into()], "")
                        .unwrap();
                }
                (hdr.into(), Type::Map(velem.as_elem().unwrap()))
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

    /// 0 除算チェック: 除数が 0 なら異常終了する。整数の `/` `%` のみで使う
    /// (float の 0 除算は IEEE で inf/nan になり UB ではないので対象外)。
    fn divide_check(&mut self, divisor: inkwell::values::IntValue<'ctx>) {
        let zero = self.ctx.i64_type().const_int(0, false);
        let is_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, divisor, zero, "divzero")
            .unwrap();
        let function = self.cur_function();
        let fail_bb = self.ctx.append_basic_block(function, "div.fail");
        let ok_bb = self.ctx.append_basic_block(function, "div.ok");
        self.builder
            .build_conditional_branch(is_zero, fail_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(fail_bb);
        self.panic("lumo: division by zero\n", "divzero_msg");
        self.builder.position_at_end(ok_bb);
    }

    /// 算術・比較演算（論理を除く二項演算）。`ty` は両辺の型（typeck が一致を保証）。
    fn gen_arith_or_cmp(
        &mut self,
        op: BinOp,
        l: BasicValueEnum<'ctx>,
        r: BasicValueEnum<'ctx>,
        ty: Type,
    ) -> (BasicValueEnum<'ctx>, Type) {
        if ty == Type::Str {
            return self.gen_str_binop(op, l, r);
        }
        if ty == Type::Float {
            let b = &self.builder;
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
                BinOp::Add => (
                    self.builder.build_int_add(l, r, "add").unwrap().into(),
                    Type::Int,
                ),
                BinOp::Sub => (
                    self.builder.build_int_sub(l, r, "sub").unwrap().into(),
                    Type::Int,
                ),
                BinOp::Mul => (
                    self.builder.build_int_mul(l, r, "mul").unwrap().into(),
                    Type::Int,
                ),
                BinOp::Div => {
                    self.divide_check(r);
                    (
                        self.builder
                            .build_int_signed_div(l, r, "div")
                            .unwrap()
                            .into(),
                        Type::Int,
                    )
                }
                BinOp::Mod => {
                    self.divide_check(r);
                    (
                        self.builder
                            .build_int_signed_rem(l, r, "rem")
                            .unwrap()
                            .into(),
                        Type::Int,
                    )
                }
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
                        self.builder
                            .build_int_compare(pred, l, r, "cmp")
                            .unwrap()
                            .into(),
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
            // 比較は strcmp の符号で行う（== != < <= > >=）
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let strcmp = self.module.get_function("strcmp").unwrap();
                let cmp = self
                    .builder
                    .build_call(strcmp, &[a.into(), b.into()], "scmp")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let zero = self.ctx.i32_type().const_int(0, false);
                let pred = match op {
                    BinOp::Eq => IntPredicate::EQ,
                    BinOp::Ne => IntPredicate::NE,
                    BinOp::Lt => IntPredicate::SLT,
                    BinOp::Le => IntPredicate::SLE,
                    BinOp::Gt => IntPredicate::SGT,
                    BinOp::Ge => IntPredicate::SGE,
                    _ => unreachable!(),
                };
                let res = self
                    .builder
                    .build_int_compare(pred, cmp, zero, "strcmp_res")
                    .unwrap();
                (res.into(), Type::Bool)
            }
            _ => unreachable!(),
        }
    }

    /// 配列の i 番目スロットのアドレスを計算する。
    /// レイアウトは [長さ i64][8byte スロット×N] なので、要素は 8 + 8*i バイト目。
    /// 符号なし比較 idx >= len なら範囲外として異常終了する（負の添字も巨大値として弾く）。
    /// 失敗ブロックを作り、呼び出し後はビルダを "範囲内" ブロックに置く。
    fn bounds_check(
        &mut self,
        idx: inkwell::values::IntValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
    ) {
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
        self.panic("lumo: index out of bounds\n", "oob_msg");
        self.builder.position_at_end(ok_bb);
    }

    /// 型付きの値を map ランタイムの 8byte スロット(i64)へ詰める。
    /// int=そのまま, bool=zext, float=bitcast, 参照=ptrtoint。
    fn to_slot_i64(&self, v: BasicValueEnum<'ctx>, ty: Type) -> inkwell::values::IntValue<'ctx> {
        let i64t = self.ctx.i64_type();
        match ty {
            Type::Int => v.into_int_value(),
            Type::Bool => self
                .builder
                .build_int_z_extend(v.into_int_value(), i64t, "b2i")
                .unwrap(),
            Type::Float => self
                .builder
                .build_bit_cast(v.into_float_value(), i64t, "f2i")
                .unwrap()
                .into_int_value(),
            _ => self
                .builder
                .build_ptr_to_int(v.into_pointer_value(), i64t, "p2i")
                .unwrap(),
        }
    }

    /// 8byte スロット(i64)を型 `ty` の値へ戻す（[`to_slot_i64`] の逆）。
    fn slot_to_value(
        &self,
        raw: inkwell::values::IntValue<'ctx>,
        ty: Type,
    ) -> BasicValueEnum<'ctx> {
        match ty {
            Type::Int => raw.into(),
            Type::Bool => self
                .builder
                .build_int_truncate(raw, self.ctx.bool_type(), "i2b")
                .unwrap()
                .into(),
            Type::Float => self
                .builder
                .build_bit_cast(raw, self.ctx.f64_type(), "i2f")
                .unwrap(),
            _ => self
                .builder
                .build_int_to_ptr(raw, self.ctx.ptr_type(AddressSpace::default()), "i2p")
                .unwrap()
                .into(),
        }
    }

    /// 文字列ポインタのハッシュ値（map のキー用、`lumo_str_hash` 呼び出し）。
    fn str_hash(&self, key: PointerValue<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let hashf = self.module.get_function("lumo_str_hash").unwrap();
        self.builder
            .build_call(hashf, &[key.into()], "h")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value()
    }

    /// 配列ヘッダ `{len@0, cap@8, data@16}` の `byte_off` バイト目のフィールドアドレス。
    fn hdr_field(&self, hdr: PointerValue<'ctx>, byte_off: u64) -> PointerValue<'ctx> {
        let off = self.ctx.i64_type().const_int(byte_off, false);
        unsafe {
            self.builder
                .build_in_bounds_gep(self.ctx.i8_type(), hdr, &[off], "hdr.f")
                .unwrap()
        }
    }

    fn elem_addr(&mut self, base: PointerValue<'ctx>, index: &Expr) -> PointerValue<'ctx> {
        let i64t = self.ctx.i64_type();
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        // 配列(ヘッダ)が null でないことを先に確認する
        self.null_check(base);
        let (idx, _) = self.gen_expr(index);
        let idx = idx.into_int_value();

        // 長さはヘッダ先頭の i64。境界チェックする。
        let len = self
            .builder
            .build_load(i64t, base, "len")
            .unwrap()
            .into_int_value();
        self.bounds_check(idx, len);

        // data ポインタ（ヘッダ +16）を読み、その 8*idx バイト目を指す
        let data = self
            .builder
            .build_load(ptr, self.hdr_field(base, 16), "data")
            .unwrap()
            .into_pointer_value();
        let eight = i64t.const_int(8, false);
        let off = self.builder.build_int_mul(idx, eight, "off").unwrap();
        unsafe {
            self.builder
                .build_in_bounds_gep(self.ctx.i8_type(), data, &[off], "slot")
                .unwrap()
        }
    }

    /// 文字列 `s` の i バイト目のアドレスを計算する（null・境界チェック付き）。
    fn str_byte_addr(&mut self, s: PointerValue<'ctx>, index: &Expr) -> PointerValue<'ctx> {
        self.null_check(s);
        let (idx, _) = self.gen_expr(index);
        let idx = idx.into_int_value();
        let strlen = self.module.get_function("strlen").unwrap();
        let len = self
            .builder
            .build_call(strlen, &[s.into()], "slen")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.bounds_check(idx, len);
        unsafe {
            self.builder
                .build_in_bounds_gep(self.ctx.i8_type(), s, &[idx], "byteaddr")
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
