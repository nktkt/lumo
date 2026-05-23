//! Lumo コンパイラのエントリポイント。
//!
//! パイプライン:
//!   ソース -> 字句解析(lexer) -> 構文解析(parser) -> コード生成(codegen=LLVM IR)
//!         -> JIT実行 / ネイティブ実行ファイル生成 / IR表示

mod ast;
mod codegen;
mod diagnostics;
mod lexer;
mod parser;
mod span;
mod typeck;
mod types;

use diagnostics::Diagnostic;
use std::process::exit;

fn usage() -> ! {
    eprintln!("Lumo compiler 0.26");
    eprintln!("使い方:");
    eprintln!("  lumo <command> [-O0|-O1|-O2|-O3] <file.lum>");
    eprintln!();
    eprintln!("コマンド:");
    eprintln!("  run       JITでその場で実行する");
    eprintln!("  build     ネイティブ実行ファイルを生成する");
    eprintln!("  emit-ir   生成されるLLVM IRを表示する");
    eprintln!();
    eprintln!("  -O0..-O3  最適化レベル（既定: -O0。emit-ir で最適化前後を比較できる）");
    exit(1);
}

fn main() {
    // 引数を解析する: コマンド・ファイル・最適化レベル(-O0..-O3)。
    let args: Vec<String> = std::env::args().collect();
    let mut cmd: Option<String> = None;
    let mut path: Option<String> = None;
    let mut opt: u8 = 0;
    for arg in &args[1..] {
        match arg.as_str() {
            "-O0" => opt = 0,
            "-O1" => opt = 1,
            "-O2" => opt = 2,
            "-O3" => opt = 3,
            s if cmd.is_none() => cmd = Some(s.to_string()),
            s if path.is_none() => path = Some(s.to_string()),
            _ => usage(),
        }
    }
    let (Some(cmd), Some(path)) = (cmd, path) else {
        usage();
    };
    let cmd = cmd.as_str();

    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("ファイルを読めません {}: {}", path, e);
        exit(1);
    });

    // 字句解析・構文解析・型検査・コード生成。エラーは位置付き診断として表示する。
    let context = inkwell::context::Context::create();
    let mut cg = codegen::CodeGen::new(&context, "lumo");

    let compiled: Result<(), Diagnostic> = (|| {
        let tokens = lexer::lex(&src)?;
        let program = parser::parse(tokens)?;
        typeck::check(&program)?;
        cg.compile(&program)
    })();

    if let Err(diag) = compiled {
        eprint!("{}", diag.render(&src, &path));
        exit(1);
    }

    // 最適化パスを適用する（-O0 のときは何もしない）。
    cg.optimize(opt).unwrap_or_else(|e| {
        eprintln!("最適化エラー: {}", e);
        exit(1);
    });

    match cmd {
        "emit-ir" => {
            print!("{}", cg.ir_string());
        }
        "run" => {
            let code = cg.jit_run().unwrap_or_else(|e| {
                eprintln!("実行エラー: {}", e);
                exit(1);
            });
            exit(code as i32);
        }
        "build" => {
            // 入力パスから拡張子を除いた名前を実行ファイル名にする (fib.lum -> fib)
            let out = std::path::Path::new(&path)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "a.out".to_string());
            cg.build_executable(&out).unwrap_or_else(|e| {
                eprintln!("ビルドエラー: {}", e);
                exit(1);
            });
            eprintln!("生成しました: ./{}", out);
        }
        _ => usage(),
    }
}
