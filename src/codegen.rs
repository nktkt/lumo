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
