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
    eprintln!("Lumo compiler 0.2");
    eprintln!("使い方:");
    eprintln!("  lumo run     <file.lum>    # JITでその場で実行する");
    eprintln!("  lumo build   <file.lum>    # ネイティブ実行ファイルを生成する");
    eprintln!("  lumo emit-ir <file.lum>    # 生成されるLLVM IRを表示する");
    exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        usage();
    }
    let cmd = args[1].as_str();
    let path = &args[2];

    let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("ファイルを読めません {}: {}", path, e);
        exit(1);
    });

    // 字句解析・構文解析・コード生成。エラーは位置付き診断として表示する。
    let context = inkwell::context::Context::create();
    let mut cg = codegen::CodeGen::new(&context, "lumo");

    let compiled: Result<(), Diagnostic> = (|| {
        let tokens = lexer::lex(&src)?;
        let program = parser::parse(tokens)?;
        typeck::check(&program)?;
        cg.compile(&program)
    })();

    if let Err(diag) = compiled {
        eprint!("{}", diag.render(&src, path));
        exit(1);
    }

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
            let out = std::path::Path::new(path)
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
