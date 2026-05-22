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
