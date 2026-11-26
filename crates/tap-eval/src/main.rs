//! `tap-eval`: applies an edit list to a recording.
//!
//! ```text
//! tap-eval preview <recording> [--edits <file>] [--json]
//! tap-eval apply <recording> --edits <file> --out <path>
//! tap-eval plan-delete <recording> <pane> <start-byte> <end-byte>
//! tap-eval redact <recording> <text>
//! ```
//!
//! `<recording>` is a `Session.tap/` package or a single-file `.tap` JSON
//! project. `--edits` names an `edits.json` document or a JSON array of
//! edit ops; without it the recording's own edit list is used.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Serialize;
use tap_eval::{Evaluation, Outcome, evaluate_entries};
use tap_index::{IndexOptions, SessionIndex};
use tap_model::edl::{EditEntry, SegId};
use tap_model::{Edits, PaneId, Session};

const USAGE: &str = "usage:
  tap-eval preview <recording> [--edits <file>] [--json]
      evaluate the edits; print what each did and the edited transcript
  tap-eval apply <recording> --edits <file> --out <path>
      evaluate the edits and write the edited session (a .tap JSON file,
      or a Session.tap/ package when <path> ends in /)
  tap-eval plan-delete <recording> <pane> <start-byte> <end-byte>
      what deleting a block does: excise it, or skip it and why
  tap-eval redact <recording> <text>
      plan the redaction of <text>; print the edits and what remains

<recording> is a Session.tap/ package or a single-file .tap JSON project.
--edits names an edits.json document or a JSON array of edit ops; without
it the recording's own edit list is used.";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tap-eval: {message}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    positional: Vec<String>,
    edits: Option<PathBuf>,
    out: Option<PathBuf>,
    json: bool,
}

fn parse(args: Vec<String>) -> Result<Option<Args>, String> {
    let mut parsed = Args {
        positional: Vec::new(),
        edits: None,
        out: None,
        json: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => parsed.json = true,
            "--edits" => parsed.edits = Some(args.next().ok_or("--edits needs a file")?.into()),
            "--out" => parsed.out = Some(args.next().ok_or("--out needs a path")?.into()),
            "-h" | "--help" => return Ok(None),
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{USAGE}"));
            }
            _ => parsed.positional.push(arg),
        }
    }
    Ok(Some(parsed))
}

fn read_edits(path: &Path) -> Result<Vec<EditEntry>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if value.is_array() {
        serde_json::from_value(value).map_err(|e| format!("{}: {e}", path.display()))
    } else {
        let edits: Edits =
            serde_json::from_value(value).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(edits.edits)
    }
}

fn to_json(value: &impl Serialize) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map(|mut s| {
            s.push('\n');
            s
        })
        .map_err(|e| e.to_string())
}

fn seconds(us: u64) -> String {
    format!("{}.{:01}s", us / 1_000_000, (us % 1_000_000) / 100_000)
}

/// One line per edit: what it did.
fn describe(entries: &[EditEntry], evaluation: &Evaluation) -> String {
    let mut out = String::new();
    for (index, (entry, outcome)) in entries.iter().zip(&evaluation.outcomes).enumerate() {
        let what = match outcome {
            Outcome::Disabled => "disabled".to_string(),
            Outcome::Time => "applied to the time map".to_string(),
            Outcome::Excised {
                removed,
                fixups,
                inputs_removed,
            } => {
                let parts: Vec<&str> = fixups.parts.iter().map(|p| p.state.as_str()).collect();
                format!(
                    "excised bytes {}..{} and {inputs_removed} input events; fixups: {}",
                    removed.start,
                    removed.end,
                    if parts.is_empty() {
                        "none".to_string()
                    } else {
                        format!("{} ({})", parts.join(", "), fixups.text)
                    }
                )
            }
            Outcome::Refused { reasons } => format!(
                "refused, played as a skip: {}",
                reasons
                    .iter()
                    .map(|r| serde_json::to_string(r).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Outcome::Replaced { program, splices } => format!(
                "rewritten in place (program bytes {}..{}, {splices} splices)",
                program.start, program.end
            ),
            Outcome::NeedsMask { reason } => format!(
                "needs a mask: {}",
                serde_json::to_string(reason).unwrap_or_default()
            ),
            Outcome::Superseded { by } => format!("superseded by #{by}"),
            Outcome::Redacted { events } => format!("withheld {} input events", events.len()),
            Outcome::Masked { region } => {
                let region = &evaluation.masks[*region];
                format!(
                    "mask over {} cells from program byte {}, {} spans",
                    region.cells,
                    region.anchor,
                    region.spans.len()
                )
            }
            Outcome::Inert { reason } => format!("no effect: {reason}"),
            Outcome::PreBake { reason } => format!("refused: {reason}"),
        };
        out.push_str(&format!("#{index} {}: {what}\n", entry.op.name()));
    }
    out
}

fn load(path: &str) -> Result<(Session, SessionIndex), String> {
    let session = tap_index::open(Path::new(path)).map_err(|e| e.to_string())?;
    let raw = SessionIndex::build(&session).map_err(|e| format!("{path}: {e}"))?;
    Ok((session, raw))
}

#[derive(Serialize)]
struct Preview<'a> {
    outcomes: &'a [Outcome],
    masks: &'a [tap_eval::MaskRegion],
    #[serde(rename = "sourceDurationUs")]
    source_duration_us: u64,
    #[serde(rename = "editedDurationUs")]
    edited_duration_us: u64,
    transcript: tap_index::Transcript,
}

fn run(args: Vec<String>) -> Result<(), String> {
    let Some(args) = parse(args)? else {
        println!("{USAGE}");
        return Ok(());
    };
    let Some((command, rest)) = args.positional.split_first() else {
        return Err(USAGE.to_string());
    };
    match (command.as_str(), rest) {
        ("preview" | "apply", [path]) => {
            let (session, raw) = load(path)?;
            let entries = match &args.edits {
                Some(file) => read_edits(file)?,
                None if command == "apply" => return Err(format!("apply needs --edits\n{USAGE}")),
                None => session.edits.edits.clone(),
            };
            let evaluation =
                evaluate_entries(&session, &raw, &entries).map_err(|e| format!("{path}: {e}"))?;
            if command == "apply" {
                let out = args.out.ok_or(format!("apply needs --out\n{USAGE}"))?;
                let baked = tap_eval::bake::bake(&session, &raw, &entries, &evaluation)
                    .map_err(|e| format!("{path}: {e}"))?;
                if out.to_string_lossy().ends_with('/') {
                    tap_model::package::create(&out, &baked).map_err(|e| e.to_string())?;
                } else if !baked.edits.edits.is_empty() {
                    return Err(format!(
                        "the edited session keeps {} edits (time edits and masks), which a .tap JSON file cannot hold; write a package by ending --out with /",
                        baked.edits.edits.len()
                    ));
                } else {
                    let json = tap_model::legacy::export_json(&baked).map_err(|e| e.to_string())?;
                    std::fs::write(&out, json).map_err(|e| format!("{}: {e}", out.display()))?;
                }
                print!("{}", describe(&entries, &evaluation));
                println!("wrote {}", out.display());
                return Ok(());
            }
            let edited = evaluation
                .index(&session, IndexOptions::default())
                .map_err(|e| format!("{path}: {e}"))?;
            let transcript = edited.transcript();
            if args.json {
                print!(
                    "{}",
                    to_json(&Preview {
                        outcomes: &evaluation.outcomes,
                        masks: &evaluation.masks,
                        source_duration_us: session.manifest.duration_us,
                        edited_duration_us: evaluation.time_map.edited_duration(),
                        transcript,
                    })?
                );
            } else {
                print!("{}", describe(&entries, &evaluation));
                println!(
                    "# duration {} -> {}\n",
                    seconds(session.manifest.duration_us),
                    seconds(evaluation.time_map.edited_duration())
                );
                print!("{}", transcript.to_text());
            }
            Ok(())
        }
        ("plan-delete", [path, pane, start, end]) => {
            let (session, raw) = load(path)?;
            let byte = |s: &str| {
                s.parse::<u64>()
                    .map_err(|_| format!("not a byte offset: {s}"))
            };
            let segment = SegId {
                pane: PaneId::new(pane.as_str())?,
                start: byte(start)?,
                end: byte(end)?,
            };
            let plan =
                tap_eval::plan_delete(&session, &raw, &segment).map_err(|e| e.to_string())?;
            print!("{}", to_json(&plan)?);
            Ok(())
        }
        ("redact", [path, text]) => {
            let (session, raw) = load(path)?;
            let redaction =
                tap_eval::redact::plan(&session, &raw, text).map_err(|e| e.to_string())?;
            #[derive(Serialize)]
            struct Out<'a> {
                edits: &'a [EditEntry],
                report: &'a tap_eval::redact::Report,
            }
            print!(
                "{}",
                to_json(&Out {
                    edits: &redaction.edits,
                    report: &redaction.report,
                })?
            );
            Ok(())
        }
        _ => Err(USAGE.to_string()),
    }
}
