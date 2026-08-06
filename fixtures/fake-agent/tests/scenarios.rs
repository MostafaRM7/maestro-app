use std::process::Command;

#[test]
fn happy_scenario_is_deterministic() {
    let first = run("structured/happy");
    let second = run("structured/happy");

    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(first.stderr, second.stderr);
    assert!(String::from_utf8_lossy(&first.stdout).contains("Maestro ✓"));
}

#[test]
fn nonzero_scenario_preserves_output_and_stderr() {
    let output = run("structured/nonzero");

    assert_eq!(output.status.code(), Some(23));
    assert!(String::from_utf8_lossy(&output.stdout).contains("accepted-before-failure"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("deterministic fixture failure"));
}

fn run(scenario: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_maestro-fake-agent"))
        .args(["--scenario", scenario])
        .output()
        .expect("fake agent starts")
}
