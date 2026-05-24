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

use ast::Program;
use diagnostics::Diagnostic;
use span::{SourceMap, Span};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::exit;

/// `import` を再帰的に解決し、全ファイルの structs/funcs を `merged` に統合する。
///
/// 各ファイルは canonical path で一度だけ読み込む（ダイヤモンド import を1回に畳み、
/// 循環があっても終了する）。import は post-order で処理する（依存を先に積む）ので、
/// 統合後の順序は「依存 → それを使う側」になる。`import_span` はこのファイルを引き込んだ
/// `import` 文の位置で、読み込み失敗をその行で報告するために使う（ルートは `None`）。
fn load_module(
    path: &Path,
    import_span: Option<Span>,
    sources: &mut SourceMap,
    visited: &mut HashSet<PathBuf>,
    merged: &mut Program,
) -> Result<(), Diagnostic> {
    let canon = std::fs::canonicalize(path).map_err(|e| {
        import_err(
            format!("import 先が見つかりません {}: {}", path.display(), e),
            import_span,
        )
    })?;
    // 既に読み込み済みなら何もしない（重複排除・循環安全）。
    if !visited.insert(canon.clone()) {
        return Ok(());
    }
    let src = std::fs::read_to_string(&canon).map_err(|e| {
        import_err(
            format!("ファイルを読めません {}: {}", canon.display(), e),
            import_span,
        )
    })?;
    let fid = sources.add(canon.display().to_string(), src);
    let tokens = lexer::lex(&sources.get(fid).src, fid)?;
    let module = parser::parse(tokens)?;

    // import を先に解決（依存を先に積む post-order）。パスは自分のあるディレクトリ基準。
    let dir = canon.parent().map(Path::to_path_buf).unwrap_or_default();
    for imp in &module.imports {
        load_module(
            &dir.join(&imp.path),
            Some(imp.span),
            sources,
            visited,
            merged,
        )?;
    }
    merged.structs.extend(module.structs);
    merged.funcs.extend(module.funcs);
    Ok(())
}

/// import 解決の失敗を診断にする。span があればその `import` 文を指す。
fn import_err(msg: String, span: Option<Span>) -> Diagnostic {
    let d = Diagnostic::error(msg).with_code("E0105");
    match span {
        Some(s) => d.at(s),
        None => d,
    }
}

fn usage() -> ! {
    eprintln!("Lumo compiler 0.43");
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

    // ルートから import を辿って全ファイルを読み込み、1つの Program に統合する。
    // エラーは位置付き診断として（import 先のファイルでも正しい位置で）表示する。
    let mut sources = SourceMap::new();
    let context = inkwell::context::Context::create();
    let mut cg = codegen::CodeGen::new(&context, "lumo");

    let compiled: Result<(), Diagnostic> = (|| {
        let mut merged = Program {
            imports: Vec::new(),
            structs: Vec::new(),
            funcs: Vec::new(),
        };
        let mut visited = HashSet::new();
        load_module(
            Path::new(&path),
            None,
            &mut sources,
            &mut visited,
            &mut merged,
        )?;
        typeck::check(&merged)?;
        cg.compile(&merged)
    })();

    if let Err(diag) = compiled {
        eprint!("{}", diag.render(&sources));
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
