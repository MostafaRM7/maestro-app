//! Deterministic fake agent. It performs no networking and never invokes a
//! provider. Scenario names are stable test-fixture identifiers.

use std::{
    io::Write as _,
    process::{Command, ExitCode, Stdio},
};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const SESSION_ID: &str = "fake-session-0001";
const BINDING_ID: &str = "fake-binding-0001";

#[tokio::main]
async fn main() -> ExitCode {
    let arguments = match Arguments::parse(std::env::args().skip(1)) {
        Ok(arguments) => arguments,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(64);
        }
    };

    match run(arguments).await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("fake-agent error: {error}");
            ExitCode::from(70)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Arguments {
    scenario: String,
    binding: Option<String>,
    volume: usize,
}

impl Arguments {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut arguments = arguments.into_iter();
        let mut scenario = None;
        let mut binding = None;
        let mut volume = 10_000;
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--scenario" => {
                    scenario = arguments.next();
                    if scenario.is_none() {
                        return Err("--scenario requires a value".to_owned());
                    }
                }
                "--binding" => {
                    binding = arguments.next();
                    if binding.is_none() {
                        return Err("--binding requires a value".to_owned());
                    }
                }
                "--volume" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--volume requires a value".to_owned())?;
                    volume = value
                        .parse()
                        .map_err(|_| "--volume must be a non-negative integer".to_owned())?;
                }
                "--list-scenarios" => {
                    println!("{}", SCENARIOS.join("\n"));
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(Self {
            scenario: scenario.ok_or_else(|| "--scenario is required".to_owned())?,
            binding,
            volume,
        })
    }
}

const SCENARIOS: &[&str] = &[
    "structured/happy",
    "structured/fragmented",
    "structured/multi-frame-read",
    "structured/permission",
    "structured/user-input",
    "structured/gui-actions",
    "structured/nonzero",
    "structured/crash",
    "structured/malformed",
    "structured/incompatible",
    "structured/delay",
    "structured/stall",
    "structured/flood",
    "structured/resume",
    "structured/process-tree",
    "structured/ignore-term",
    "tui/vt-baseline",
    "tui/alternate-screen",
    "tui/resize-mouse",
    "tui/osc-security",
    "shell/interactive",
];

// Keeping the scenario table in one match makes the executable's complete
// externally visible fixture contract auditable in one place.
#[allow(clippy::too_many_lines)]
async fn run(arguments: Arguments) -> Result<u8, Box<dyn std::error::Error>> {
    match arguments.scenario.as_str() {
        "structured/happy" => {
            write_happy(false).await?;
            Ok(0)
        }
        "structured/fragmented" => {
            write_happy(true).await?;
            Ok(0)
        }
        "structured/multi-frame-read" => {
            let bytes = happy_frames()
                .into_iter()
                .flat_map(|value| frame_bytes(&value))
                .collect::<Vec<_>>();
            let mut stdout = tokio::io::stdout();
            stdout.write_all(&bytes).await?;
            stdout.flush().await?;
            Ok(0)
        }
        "structured/permission" => {
            emit(json!({
                "type": "permission_request",
                "protocol_version": 1,
                "session_id": SESSION_ID,
                "request_id": "permission-0001",
                "tool": "shell",
                "command": ["git", "status"],
                "paths": ["."],
            }))
            .await?;
            let response = read_json_line().await?;
            emit(json!({
                "type": "permission_result",
                "request_id": "permission-0001",
                "decision": response.get("decision").and_then(Value::as_str).unwrap_or("cancel"),
            }))
            .await?;
            Ok(0)
        }
        "structured/user-input" => {
            emit(json!({
                "type": "user_input_request",
                "protocol_version": 1,
                "session_id": SESSION_ID,
                "request_id": "input-0001",
                "prompt": "Choose a deterministic answer",
                "choices": ["alpha", "beta"],
            }))
            .await?;
            let response = read_json_line().await?;
            emit(json!({
                "type": "user_input_result",
                "request_id": "input-0001",
                "value": response.get("value").cloned().unwrap_or(Value::Null),
            }))
            .await?;
            Ok(0)
        }
        "structured/gui-actions" => {
            emit(json!({ "type": "ready", "session_id": SESSION_ID })).await?;
            let action = read_json_line().await?;
            emit(json!({ "type": "action_ack", "action": action })).await?;
            Ok(0)
        }
        "structured/nonzero" => {
            emit(json!({ "type": "message", "content": "accepted-before-failure" })).await?;
            eprintln!("deterministic fixture failure");
            Ok(23)
        }
        "structured/crash" => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(b"{\"type\":\"partial")?;
            stdout.flush()?;
            std::process::abort();
        }
        "structured/malformed" => {
            let mut stdout = tokio::io::stdout();
            stdout.write_all(b"not-json\n").await?;
            stdout
                .write_all(b"{\"type\":42,\"payload\":\"invalid-type\"}\n")
                .await?;
            stdout.flush().await?;
            Ok(0)
        }
        "structured/incompatible" => {
            emit(json!({ "type": "init", "protocol_version": 999 })).await?;
            Ok(0)
        }
        "structured/delay" => {
            emit(json!({ "type": "init", "protocol_version": 1 })).await?;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            emit(json!({ "type": "result", "status": "completed-after-delay" })).await?;
            Ok(0)
        }
        "structured/stall" => {
            emit(json!({ "type": "init", "protocol_version": 1 })).await?;
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok(0)
        }
        "structured/flood" => {
            for sequence in 0..arguments.volume {
                emit(json!({
                    "type": "delta",
                    "sequence": sequence,
                    "content": "deterministic-stream-chunk",
                }))
                .await?;
            }
            Ok(0)
        }
        "structured/resume" => {
            if let Some(binding) = arguments.binding {
                if binding != BINDING_ID {
                    return Ok(65);
                }
                emit(json!({ "type": "resumed", "binding_id": binding })).await?;
            } else {
                emit(json!({ "type": "binding", "binding_id": BINDING_ID })).await?;
            }
            Ok(0)
        }
        "structured/process-tree" => run_process_tree().await,
        "internal/process-tree-child" => run_process_tree_child(),
        "structured/ignore-term" => ignore_termination().await,
        "tui/vt-baseline" => {
            tui_baseline().await?;
            Ok(0)
        }
        "tui/alternate-screen" => {
            let mut stdout = tokio::io::stdout();
            stdout
                .write_all(
                    b"main-before\r\n\x1b[?1049h\x1b[2J\x1b[Halternate\x1b[?1049lmain-after\r\n",
                )
                .await?;
            stdout.flush().await?;
            Ok(0)
        }
        "tui/resize-mouse" => {
            let _terminal_mode = TerminalModeGuard::enter()?;
            let mut stdout = tokio::io::stdout();
            stdout
                .write_all(b"\x1b[?1003h\x1b[?1006hmouse-ready\r\n")
                .await?;
            stdout.flush().await?;
            let input = read_sgr_mouse_reports(5).await?;
            let dimensions = tokio::task::spawn_blocking(terminal_dimensions).await??;
            stdout.write_all(b"size:").await?;
            stdout.write_all(dimensions.as_bytes()).await?;
            stdout.write_all(b"\r\n").await?;
            stdout.write_all(b"reports:5\r\n").await?;
            stdout.write_all(b"mouse:").await?;
            stdout.write_all(&input).await?;
            stdout.write_all(b"\x1b[?1006l\x1b[?1003l\r\n").await?;
            stdout.flush().await?;
            Ok(0)
        }
        "tui/osc-security" => {
            let mut stdout = tokio::io::stdout();
            stdout
                .write_all(
                    b"\x1b]0;hostile-title\x07\x1b]52;c;c2VjcmV0\x07\x1b]8;;https://example.invalid/?token=fixture\x07link\x1b]8;;\x07\r\n",
                )
                .await?;
            stdout.flush().await?;
            Ok(0)
        }
        "shell/interactive" => {
            let mut stdout = tokio::io::stdout();
            stdout.write_all(b"fake$ ").await?;
            stdout.flush().await?;
            let mut line = String::new();
            BufReader::new(tokio::io::stdin())
                .read_line(&mut line)
                .await?;
            stdout.write_all(b"received:").await?;
            stdout.write_all(line.as_bytes()).await?;
            stdout.flush().await?;
            Ok(0)
        }
        _ => Ok(66),
    }
}

fn happy_frames() -> Vec<Value> {
    vec![
        json!({ "type": "init", "protocol_version": 1, "session_id": SESSION_ID, "binding_id": BINDING_ID }),
        json!({ "type": "message_delta", "sequence": 1, "content": "Hello, " }),
        json!({ "type": "message_delta", "sequence": 2, "content": "Maestro ✓" }),
        json!({ "type": "tool_start", "sequence": 3, "tool": "read_file", "path": "README.md" }),
        json!({ "type": "tool_end", "sequence": 4, "tool": "read_file", "status": "ok" }),
        json!({ "type": "artifact", "sequence": 5, "path": "README.md", "kind": "file" }),
        json!({ "type": "usage", "sequence": 6, "input_tokens": 10, "output_tokens": 5 }),
        json!({ "type": "result", "sequence": 7, "status": "completed" }),
    ]
}

async fn write_happy(fragmented: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut stdout = tokio::io::stdout();
    for value in happy_frames() {
        let bytes = frame_bytes(&value);
        if fragmented {
            for chunk in bytes.chunks(3) {
                stdout.write_all(chunk).await?;
                stdout.flush().await?;
                tokio::task::yield_now().await;
            }
        } else {
            stdout.write_all(&bytes).await?;
        }
    }
    stdout.flush().await?;
    Ok(())
}

fn frame_bytes(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).expect("fixture JSON is serializable");
    bytes.push(b'\n');
    bytes
}

async fn emit(value: Value) -> Result<(), std::io::Error> {
    let mut stdout = tokio::io::stdout();
    stdout.write_all(&frame_bytes(&value)).await?;
    stdout.flush().await
}

async fn read_json_line() -> Result<Value, Box<dyn std::error::Error>> {
    let mut line = String::new();
    BufReader::new(tokio::io::stdin())
        .read_line(&mut line)
        .await?;
    Ok(serde_json::from_str(&line)?)
}

async fn read_sgr_mouse_reports(expected: usize) -> Result<Vec<u8>, std::io::Error> {
    const MAXIMUM_REPORT_BYTES: usize = 512;
    let mut stdin = tokio::io::stdin();
    let mut input = Vec::with_capacity(128);
    let mut report_start = 0;
    let mut reports = 0;
    while reports < expected {
        let mut byte = [0_u8; 1];
        let count = stdin.read(&mut byte).await?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "fake TUI mouse input ended before every report arrived",
            ));
        }
        input.push(byte[0]);
        if input.len() > MAXIMUM_REPORT_BYTES {
            return Err(std::io::Error::other(
                "fake TUI mouse reports exceeded the bounded input size",
            ));
        }
        if matches!(byte[0], b'M' | b'm') {
            if !valid_sgr_mouse_report(&input[report_start..]) {
                return Err(std::io::Error::other(
                    "fake TUI received an invalid SGR mouse report",
                ));
            }
            reports += 1;
            report_start = input.len();
        }
    }
    Ok(input)
}

fn valid_sgr_mouse_report(report: &[u8]) -> bool {
    if report.len() < 9
        || !report.starts_with(b"\x1b[<")
        || !report
            .last()
            .is_some_and(|final_byte| matches!(final_byte, b'M' | b'm'))
    {
        return false;
    }
    let parameters = &report[3..report.len() - 1];
    let mut fields = parameters.split(|byte| *byte == b';');
    (0..3).all(|_| {
        fields
            .next()
            .is_some_and(|field| !field.is_empty() && field.iter().all(u8::is_ascii_digit))
    }) && fields.next().is_none()
}

struct TerminalModeGuard;

impl TerminalModeGuard {
    fn enter() -> Result<Self, std::io::Error> {
        set_terminal_mode(&["-icanon", "-echo", "min", "1", "time", "0"])?;
        Ok(Self)
    }
}

impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        let _ = set_terminal_mode(&["icanon", "echo"]);
    }
}

fn set_terminal_mode(arguments: &[&str]) -> Result<(), std::io::Error> {
    let status = Command::new("stty")
        .args(arguments)
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "stty could not configure the fixture PTY",
        ))
    }
}

fn terminal_dimensions() -> Result<String, std::io::Error> {
    let output = Command::new("stty")
        .arg("size")
        .stdin(Stdio::inherit())
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(
            "stty could not read the fixture PTY size",
        ));
    }
    let dimensions = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if dimensions.is_empty() {
        Err(std::io::Error::other(
            "stty returned an empty fixture PTY size",
        ))
    } else {
        Ok(dimensions)
    }
}

async fn run_process_tree() -> Result<u8, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let mut child = std::process::Command::new(executable)
        .args(["--scenario", "internal/process-tree-child"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    emit(json!({ "type": "child_started", "pid": child.id() })).await?;
    let _ = child.wait()?;
    Ok(0)
}

fn run_process_tree_child() -> Result<u8, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let mut grandchild = std::process::Command::new(executable)
        .args(["--scenario", "structured/stall"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let _ = grandchild.wait()?;
    Ok(0)
}

#[cfg(unix)]
async fn ignore_termination() -> Result<u8, Box<dyn std::error::Error>> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    emit(json!({ "type": "ready", "ignoring": "SIGTERM" })).await?;
    loop {
        terminate.recv().await;
        emit(json!({ "type": "signal_ignored", "signal": "SIGTERM" })).await?;
    }
}

#[cfg(not(unix))]
async fn ignore_termination() -> Result<u8, Box<dyn std::error::Error>> {
    std::future::pending::<()>().await;
    Ok(0)
}

async fn tui_baseline() -> Result<(), std::io::Error> {
    let mut stdout = tokio::io::stdout();
    stdout
        .write_all(b"\x1b[2J\x1b[H\x1b[32mMaestro fake TUI \xE2\x9C\x93\x1b[0m\r\n> ")
        .await?;
    stdout.flush().await?;
    let mut line = String::new();
    BufReader::new(tokio::io::stdin())
        .read_line(&mut line)
        .await?;
    stdout.write_all(b"echo: ").await?;
    stdout.write_all(line.trim_end().as_bytes()).await?;
    stdout.write_all(b"\r\n").await?;
    stdout.flush().await
}

#[cfg(test)]
mod tests {
    use super::Arguments;

    #[test]
    fn scenario_is_explicit_and_unknown_arguments_fail() {
        assert_eq!(
            Arguments::parse(["--scenario".to_owned(), "structured/happy".to_owned()])
                .expect("valid arguments")
                .scenario,
            "structured/happy"
        );
        assert!(Arguments::parse(["structured/happy".to_owned()]).is_err());
    }
}
