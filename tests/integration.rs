//! End-to-end tests: compile + run each program in `tests/cases/` and check
//! its output, or — for error cases — that it fails with the expected
//! diagnostic code.
//!
//! Success case `foo` needs `tests/cases/foo.lum` and `tests/cases/foo.out`
//! (exact stdout). Error cases assert a non-zero exit and that stderr mentions
//! the expected `Exxxx` diagnostic code.

use std::process::Command;

/// Path to the `lumo` binary built by Cargo for this test run.
fn lumo() -> &'static str {
    env!("CARGO_BIN_EXE_lumo")
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

/// `-O2` must promote stack slots to SSA registers (mem2reg): the unoptimized
/// IR has `alloca`s, the optimized IR should not.
#[test]
fn optimization_promotes_allocas() {
    let unopt = emit_ir("fib", None);
    let opt = emit_ir("fib", Some("-O2"));
    assert!(
        unopt.contains("alloca"),
        "unoptimized IR was expected to contain allocas"
    );
    assert!(
        !opt.contains("alloca"),
        "`-O2` should remove allocas via mem2reg, but some remain:\n{}",
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
