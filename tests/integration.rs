//! End-to-end tests: compile + run each program in `tests/cases/` and check
//! its output, or — for error cases — that it fails with the expected
//! diagnostic code.
//!
//! Success case `foo` needs `tests/cases/foo.lum` and `tests/cases/foo.out`
//! (exact stdout). Error cases assert a non-zero exit and that stderr mentions
//! the expected `Exxxx` diagnostic code.

use std::io::Write;
use std::process::{Command, Stdio};

/// Path to the `lumo` binary built by Cargo for this test run.
fn lumo() -> &'static str {
    env!("CARGO_BIN_EXE_lumo")
}

/// Run a case with `stdin` piped in, and compare stdout to the golden file.
fn run_ok_stdin(name: &str, stdin_data: &str) {
    let lum = format!("tests/cases/{}.lum", name);
    let out = format!("tests/cases/{}.out", name);
    let expected = std::fs::read_to_string(&out)
        .unwrap_or_else(|_| panic!("expected output file missing: {}", out));

    let mut child = Command::new(lumo())
        .args(["run", &lum])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn lumo");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin_data.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("failed to wait for lumo");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "case `{}` exited with failure\nstderr:\n{}",
        name,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout, expected,
        "case `{}`: stdout did not match golden file",
        name
    );
}

fn run_ok(name: &str) {
    let lum = format!("tests/cases/{}.lum", name);
    let out = format!("tests/cases/{}.out", name);
    let expected = std::fs::read_to_string(&out)
        .unwrap_or_else(|_| panic!("expected output file missing: {}", out));

    let output = Command::new(lumo())
        .args(["run", &lum])
        .output()
        .expect("failed to spawn lumo");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "case `{}` exited with failure\nstderr:\n{}",
        name,
        stderr
    );
    assert_eq!(
        stdout, expected,
        "case `{}`: stdout did not match golden file",
        name
    );
}

/// Emit LLVM IR for a case, optionally at an optimization level (e.g. "-O2").
fn emit_ir(name: &str, opt: Option<&str>) -> String {
    let lum = format!("tests/cases/{}.lum", name);
    let mut cmd = Command::new(lumo());
    cmd.arg("emit-ir");
    if let Some(o) = opt {
        cmd.arg(o);
    }
    cmd.arg(&lum);
    let output = cmd.output().expect("failed to spawn lumo");
    assert!(
        output.status.success(),
        "emit-ir for `{}` failed:\n{}",
        name,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run_err(name: &str, expected_code: &str) {
    let lum = format!("tests/cases/{}.lum", name);
    let output = Command::new(lumo())
        .args(["run", &lum])
        .output()
        .expect("failed to spawn lumo");

    assert!(
        !output.status.success(),
        "case `{}` was expected to fail but succeeded",
        name
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected_code),
        "case `{}`: stderr did not contain `{}`\nstderr:\n{}",
        name,
        expected_code,
        stderr
    );
}

#[test]
fn arithmetic() {
    run_ok("arithmetic");
}

#[test]
fn fib() {
    run_ok("fib");
}

#[test]
fn control_flow() {
    run_ok("control");
}

#[test]
fn gcd() {
    run_ok("gcd");
}

#[test]
fn bool_and_logic() {
    run_ok("bool");
}

#[test]
fn short_circuit() {
    run_ok("short_circuit");
}

#[test]
fn typed_fn() {
    run_ok("typed_fn");
}

#[test]
fn float_math() {
    run_ok("float");
}

#[test]
fn loops() {
    run_ok("loops");
}

#[test]
fn strings() {
    run_ok("string");
}

#[test]
fn conversions() {
    run_ok("conversions");
}

#[test]
fn scope() {
    run_ok("scope");
}

#[test]
fn string_concat() {
    run_ok("string_concat");
}

#[test]
fn arrays() {
    run_ok("arrays");
}

#[test]
fn structs() {
    run_ok("structs");
}

#[test]
fn linked_list() {
    run_ok("linked_list");
}

#[test]
fn array_structs() {
    run_ok("array_structs");
}

#[test]
fn stringify() {
    run_ok("stringify");
}

#[test]
fn string_index() {
    run_ok("string_index");
}

#[test]
fn string_sort() {
    run_ok("string_sort");
}

#[test]
fn chr_builtin() {
    run_ok("chr");
}

#[test]
fn read_line_stdin() {
    // 4 numbers, one per line; expect count=4 then sum=65
    run_ok_stdin("read_line", "10\n20\n30\n5\n");
}

#[test]
fn growable_arrays_push() {
    run_ok("push");
}

#[test]
fn math_builtins() {
    run_ok("math");
}

#[test]
fn maps() {
    run_ok("map");
}

#[test]
fn map_keys_and_collisions() {
    run_ok("map_keys");
}

#[test]
fn map_missing_key_aborts() {
    run_err("map_missing", "key not found");
}

#[test]
fn for_in_loops() {
    run_ok("for_in");
}

#[test]
fn string_toolkit() {
    run_ok("string_ops");
}

#[test]
fn substr_out_of_range_aborts() {
    run_err("substr_oob", "substr out of range");
}

#[test]
fn parse_numbers() {
    run_ok("parse_num");
}

#[test]
fn parse_bad_int_aborts() {
    run_err("parse_bad", "non-integer string");
}

#[test]
fn file_io_roundtrip() {
    run_ok("file_io");
}

#[test]
fn nested_collections() {
    run_ok("nested");
}

#[test]
fn string_methods() {
    run_ok("string_methods");
}

#[test]
fn string_replace_repeat() {
    run_ok("replace_repeat");
}

#[test]
fn slice_and_pop() {
    run_ok("slice_pop");
}

#[test]
fn slice_out_of_range_aborts() {
    run_err("slice_oob", "slice out of range");
}

#[test]
fn pop_empty_aborts() {
    run_err("pop_empty", "pop from empty array");
}

/// Capstone: read an unknown number of lines, collect them with `push`, sort.
#[test]
fn sort_lines_stdin() {
    run_ok_stdin("sort_lines", "pear\nbanana\napple\ncherry\nfig\nkiwi\n");
}

/// Extract the body of `define ... @<name>(...) { ... }` from LLVM IR text.
/// (We scope the alloca check to one function: runtime helpers like
/// `lumo_parse_int` legitimately keep an address-taken alloca for `strtol`'s
/// `endptr`, which mem2reg cannot promote.)
fn function_body(ir: &str, name: &str) -> String {
    let needle = format!("@{}(", name);
    let start = ir
        .lines()
        .position(|l| l.starts_with("define") && l.contains(&needle))
        .unwrap_or_else(|| panic!("function @{} not found in IR", name));
    let mut body = String::new();
    for line in ir.lines().skip(start) {
        body.push_str(line);
        body.push('\n');
        if line == "}" {
            break;
        }
    }
    body
}

/// `-O2` must promote stack slots to SSA registers (mem2reg): `fib`'s
/// unoptimized body has `alloca`s, its optimized body should not.
#[test]
fn optimization_promotes_allocas() {
    let unopt = function_body(&emit_ir("fib", None), "fib");
    let opt = function_body(&emit_ir("fib", Some("-O2")), "fib");
    assert!(
        unopt.contains("alloca"),
        "unoptimized `fib` was expected to contain allocas"
    );
    assert!(
        !opt.contains("alloca"),
        "`-O2` should remove `fib`'s allocas via mem2reg, but some remain:\n{}",
        opt
    );
}

/// Optimization must not change observable behavior.
#[test]
fn optimization_preserves_behavior() {
    let expected = std::fs::read_to_string("tests/cases/fib.out").unwrap();
    let output = Command::new(lumo())
        .args(["run", "-O2", "tests/cases/fib.lum"])
        .output()
        .expect("failed to spawn lumo");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

#[test]
fn err_undefined_var() {
    run_err("err_undefined_var", "E0101");
}

#[test]
fn err_undefined_fn() {
    run_err("err_undefined_fn", "E0102");
}

#[test]
fn err_arity() {
    run_err("err_arity", "E0104");
}

#[test]
fn err_parse() {
    run_err("err_parse", "E0002");
}

#[test]
fn err_no_main() {
    run_err("err_no_main", "E0100");
}

#[test]
fn err_cond_type() {
    run_err("err_cond_type", "E0201");
}

#[test]
fn err_type_mismatch() {
    run_err("err_type_mismatch", "E0200");
}

#[test]
fn err_return_bool() {
    run_err("err_return_bool", "E0202");
}

#[test]
fn err_arg_type() {
    run_err("err_arg_type", "E0200");
}

#[test]
fn err_unknown_type() {
    run_err("err_unknown_type", "E0300");
}

#[test]
fn err_dup_param() {
    run_err("err_dup_param", "E0301");
}

#[test]
fn err_mix_types() {
    run_err("err_mix_types", "E0200");
}

#[test]
fn err_break_outside() {
    run_err("err_break_outside", "E0203");
}

#[test]
fn err_str_arith() {
    run_err("err_str_arith", "E0200");
}

#[test]
fn err_unterminated_string() {
    run_err("err_unterminated_string", "E0004");
}

#[test]
fn err_conv_type() {
    run_err("err_conv_type", "E0200");
}

#[test]
fn err_reserved_name() {
    run_err("err_reserved_name", "E0302");
}

#[test]
fn err_out_of_scope() {
    run_err("err_out_of_scope", "E0101");
}

#[test]
fn err_index_type() {
    run_err("err_index_type", "E0200");
}

#[test]
fn err_index_nonarray() {
    run_err("err_index_nonarray", "E0205");
}

#[test]
fn err_array_mixed() {
    run_err("err_array_mixed", "E0200");
}

/// Out-of-bounds indexing is a runtime failure: non-zero exit + a message.
#[test]
fn oob_runtime_check() {
    run_err("oob", "out of bounds");
}

/// Integer division by zero aborts at runtime.
#[test]
fn div_zero_runtime_check() {
    run_err("div_zero", "division by zero");
}

#[test]
fn err_unknown_field() {
    run_err("err_unknown_field", "E0306");
}

#[test]
fn err_missing_field() {
    run_err("err_missing_field", "E0307");
}

#[test]
fn err_unknown_struct() {
    run_err("err_unknown_struct", "E0303");
}

#[test]
fn err_field_nonstruct() {
    run_err("err_field_nonstruct", "E0305");
}

/// Dereferencing null is a runtime failure.
#[test]
fn null_deref_runtime_check() {
    run_err("null_deref", "null reference");
}

#[test]
fn err_let_null() {
    run_err("err_let_null", "E0208");
}

#[test]
fn err_str_struct() {
    run_err("err_str_struct", "E0200");
}

#[test]
fn err_string_assign() {
    run_err("err_string_assign", "E0207");
}
