//! `tap-index`: prints a recording's transcript or index.
//!
//! ```text
//! tap-index transcript <recording> [--json]
//! tap-index index <recording>
//! ```
//!
//! `<recording>` is a `Session.tap/` package or a single-file `.tap` JSON
//! project.

use std::path::PathBuf;
use std::process::ExitCode;

use tap_index::SessionIndex;

const USAGE: &str = "usage:
  tap-index transcript <recording> [--json]   print the transcript (text, or JSON with --json)
  tap-index index <recording>                 print the index (marks, segments) as JSON

<recording> is a Session.tap/ package or a single-file .tap JSON project.";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tap-index: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let mut json = false;
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}\n{USAGE}")),
            _ => positional.push(arg),
        }
    }
    let [command, path] = positional.as_slice() else {
        return Err(USAGE.to_string());
    };
    if !matches!(command.as_str(), "transcript" | "index") {
        return Err(format!("unknown command {command}\n{USAGE}"));
    }
    // Read errors already name the file.
    let session = tap_index::open(&PathBuf::from(path)).map_err(|e| e.to_string())?;
    let index = SessionIndex::build(&session).map_err(|e| format!("{path}: {e}"))?;
    let output = match command.as_str() {
        "transcript" if json => to_json(&index.transcript())?,
        "transcript" => index.transcript().to_text(),
        "index" => to_json(&index)?,
        _ => unreachable!("checked above"),
    };
    print!("{output}");
    Ok(())
}

fn to_json(value: &impl serde::Serialize) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map(|mut s| {
            s.push('\n');
            s
        })
        .map_err(|e| e.to_string())
}
