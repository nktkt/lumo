//! `lumo` 本体を Boehm GC (bdw-gc) にリンクする。
//!
//! Lumo が生成するコードは `GC_malloc`/`GC_realloc`/`GC_init`（RFC 0001 の決定）を
//! 呼ぶ。JIT (`lumo run`) はこれらのシンボルを **実行中プロセス** から解決するので、
//! コンパイラ本体が libgc にリンクされている必要がある。`pkg-config` で bdw-gc を
//! 探し、見つかれば cargo にリンク指示を出す。
//!
//! 事前に bdw-gc を入れておくこと（macOS: `brew install bdw-gc`、
//! Ubuntu: `apt-get install libgc-dev`）。

fn main() {
    // bdw-gc の .pc 名は環境により `bdw-gc` または `gc`。両方試す。
    let found = pkg_config::probe_library("bdw-gc")
        .or_else(|_| pkg_config::probe_library("gc"))
        .is_ok();
    if !found {
        // pkg-config で見つからなくても、標準パスに libgc があればリンクを試みる。
        println!("cargo:warning=bdw-gc が pkg-config で見つかりません。`brew install bdw-gc` か `apt-get install libgc-dev` を実行してください。-lgc を直接試みます。");
        println!("cargo:rustc-link-lib=dylib=gc");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
