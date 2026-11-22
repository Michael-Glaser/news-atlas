//! The binary's whole interface: argv in, one report out, an exit code.
//!
//! Decisions: the flag names, that `normalize` and `carry` **do nothing without
//! `--apply`** (a dry run is the default, not an option), the exit codes below, and
//! that every line goes through a `Write` so a test can read what a user would see.
//!
//! It does not: parse JSON from stdin (this is a person's tool, not a wire helper),
//! run a check (the modules do), or decide anything about the archive.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::carry::{self, Options as CarryOptions};
use crate::engines;
use crate::normalize;
use crate::report::Report;
use crate::verify::{verify, Limits};
use crate::Command;

/// Nothing disagrees.
pub const EXIT_OK: i32 = 0;
/// Something disagrees, and `verify` or `normalize` was asked to prove it.
pub const EXIT_FINDINGS: i32 = 1;
/// The command line was wrong.
pub const EXIT_USAGE: i32 = 2;
/// The run could not do what it was asked (no database, no engines, no model): not a
/// verdict about the archive.
pub const EXIT_REFUSED: i32 = 3;

const USAGE: &str = "\
atlas-migrate — carry an existing ATLAS archive over, and prove it

  atlas-migrate verify     [flags]   read-only report; non-zero when anything disagrees
  atlas-migrate normalize  [flags]   list the statements that repair it (--apply runs them)
  atlas-migrate carry      [flags]   re-derive the model's fields (--apply runs it)
  atlas-migrate all        [flags]   verify, normalize, verify, carry

flags
  --db PATH          the archive (default: data/atlas.db)
  --root PATH        the application root; the rule engines live in <root>/rust/target/release
  --apply            write. Without it normalize and carry only list what they would do
  --dry-run          the default; stated so a script can be explicit
  --json             one JSON object instead of the text report
  --no-samples       leave the sampled rows out of the report
  --sample N         sampled rows per table (default 20)
  --probe N          articles the full-text staleness probe reads (default 20)
  --place-limit N    rows the place check may examine (default 400; one engine call each)
  --publisher-limit N  empty publishers the fill may ask about (default 400; one engine call each)
  --limit N          articles this carry run may classify (default: no bound)
  --rate-ms N        milliseconds between two model calls (default 250)
  --max-minutes N    stop the carry run after N minutes
  --ids A,B,C        carry these articles only
  --force            re-carry articles that already have a receipt
  --topics-only      carry the deterministic topic pass only; no model call
  --no-retype        leave stored entity types alone (no detach/restore pass)
  --include-dupes    include hidden duplicates (dupe=1)
  --help             this text

exit codes: 0 clean · 1 findings · 2 usage · 3 could not run
";

/// One parsed command line.
#[derive(Clone, Debug)]
pub struct Args {
    pub command: Command,
    pub db: PathBuf,
    /// The application root, when the caller named one. Left out, it is derived from
    /// the archive's own location and then from the checkout the command ran in.
    pub root: Option<PathBuf>,
    pub apply: bool,
    pub json: bool,
    pub samples: bool,
    pub limits: Limits,
    pub carry: CarryOptions,
}

impl Default for Args {
    fn default() -> Args {
        Args {
            command: Command::Verify,
            db: PathBuf::from("data/atlas.db"),
            root: None,
            apply: false,
            json: false,
            samples: true,
            limits: Limits::default(),
            carry: CarryOptions::default(),
        }
    }
}

/// Parse argv (without the program name).
pub fn parse(argv: &[String]) -> std::result::Result<Option<Args>, String> {
    let mut args = Args::default();
    let mut index = 0usize;
    if argv.is_empty() {
        return Err("no command given".to_string());
    }
    args.command = match argv[0].as_str() {
        "verify" => Command::Verify,
        "normalize" | "repair" => Command::Normalize,
        "carry" => Command::Carry,
        "all" => Command::All,
        "--help" | "-h" | "help" => return Ok(None),
        other => return Err(format!("unknown command: {}", other)),
    };
    index += 1;
    while index < argv.len() {
        let flag = argv[index].as_str();
        index += 1;
        let mut value = |name: &str| -> std::result::Result<String, String> {
            match argv.get(index) {
                Some(text) => {
                    index += 1;
                    Ok(text.clone())
                }
                None => Err(format!("{} needs a value", name)),
            }
        };
        match flag {
            "--help" | "-h" => return Ok(None),
            "--apply" => args.apply = true,
            "--dry-run" => args.apply = false,
            "--json" => args.json = true,
            "--no-samples" => args.samples = false,
            "--force" => args.carry.force = true,
            "--topics-only" => args.carry.topics_only = true,
            "--no-retype" => args.carry.retype = false,
            "--include-dupes" => args.carry.include_dupes = true,
            "--db" => args.db = PathBuf::from(value("--db")?),
            "--root" => args.root = Some(PathBuf::from(value("--root")?)),
            "--sample" => {
                args.limits.sample = number(&value("--sample")?, "--sample")?;
            }
            "--probe" => {
                args.limits.probe = number(&value("--probe")?, "--probe")?;
            }
            "--place-limit" => {
                args.limits.place = number(&value("--place-limit")?, "--place-limit")?;
            }
            "--publisher-limit" => {
                args.limits.publisher = number(&value("--publisher-limit")?, "--publisher-limit")?;
            }
            "--limit" => args.carry.limit = number(&value("--limit")?, "--limit")?,
            "--rate-ms" => {
                args.carry.rate_ms = number(&value("--rate-ms")?, "--rate-ms")? as u64;
            }
            "--max-minutes" => {
                args.carry.max_minutes = Some(number(&value("--max-minutes")?, "--max-minutes")? as u64);
            }
            "--ids" => {
                let raw = value("--ids")?;
                let mut ids: Vec<i64> = Vec::new();
                for part in raw.split(',') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    match part.parse::<i64>() {
                        Ok(id) if id > 0 => ids.push(id),
                        _ => return Err(format!("--ids expects article numbers, got '{}'", part)),
                    }
                }
                args.carry.ids = ids;
            }
            other => return Err(format!("unknown flag: {}", other)),
        }
    }
    args.carry.dry_run = !args.apply;
    Ok(Some(args))
}

fn number(raw: &str, flag: &str) -> std::result::Result<usize, String> {
    match raw.trim().parse::<usize>() {
        Ok(value) => Ok(value),
        Err(_) => Err(format!("{} expects a number, got '{}'", flag, raw)),
    }
}

/// The application root: `--root`, else the archive's own root, else the checkout the
/// command was run from.
pub fn resolve_root(db: &Path, explicit: Option<&Path>) -> PathBuf {
    if let Some(root) = explicit {
        return root.to_path_buf();
    }
    let from_db = db
        .parent()
        .and_then(|parent| parent.parent())
        .map(Path::to_path_buf);
    if let Some(root) = &from_db {
        if root.join("rust").join("target").join("release").is_dir() {
            return root.clone();
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join("rust").join("target").join("release").is_dir() {
            return cwd;
        }
    }
    from_db.unwrap_or_else(|| PathBuf::from("."))
}

/// Run one command line and print to `out`.
pub fn run(argv: &[String], out: &mut dyn Write) -> i32 {
    let args = match parse(argv) {
        Ok(Some(args)) => args,
        Ok(None) => {
            let _ = write!(out, "{}", USAGE);
            return EXIT_OK;
        }
        Err(reason) => {
            let _ = writeln!(out, "atlas-migrate: {}\n\n{}", reason, USAGE);
            return EXIT_USAGE;
        }
    };
    match execute(&args, out) {
        Ok(code) => code,
        Err(reason) => {
            // `--json` promises one object a script can parse; a run that could not start
            // keeps that promise and says why in the object, instead of printing a human
            // line a parser chokes on.
            if args.json {
                write_json(
                    out,
                    &serde_json::json!({
                        "error": reason.message(),
                        "refused": true,
                        "database": args.db.display().to_string(),
                    }),
                );
            } else {
                let _ = writeln!(out, "atlas-migrate: {}", reason);
            }
            EXIT_REFUSED
        }
    }
}

/// The same, with a real stdout.
pub fn main(argv: Vec<String>) -> i32 {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    run(&argv, &mut lock)
}

fn execute(args: &Args, out: &mut dyn Write) -> crate::Result<i32> {
    let root = resolve_root(&args.db, args.root.as_deref());
    // The engine gap reaches the reader through `report.skipped` ("place check:
    // atlas-places unavailable …"), which names the check and the fix, so the flag itself
    // is not printed twice.
    let (rules, _engine_gap) = engines::for_root(&root);
    match args.command {
        Command::Verify => {
            let report = verify(&args.db, rules.as_ref(), args.limits)?;
            print_report(&report, args, out);
            Ok(verdict_of(&report))
        }
        Command::Normalize => normalize_only(args, rules.as_ref(), out),
        Command::Carry => carry_only(args, out),
        Command::All => all(args, rules.as_ref(), out),
    }
}

/// Verify, normalize, verify, carry — and, with `--json`, **one** object for the three,
/// because a script that reads stdout cannot tell three pretty-printed documents from one
/// malformed one.
fn all(
    args: &Args,
    rules: &dyn atlas_ingest::Rules,
    out: &mut dyn Write,
) -> crate::Result<i32> {
    let before = verify(&args.db, rules, args.limits)?;
    if !args.json {
        // Text mode prints each step as it happens, in the order the run book documents.
        print_report(&before, args, out);
        let normalize_code = normalize_only(args, rules, out)?;
        let carry_code = carry_only(args, out)?;
        return Ok(if normalize_code != EXIT_OK {
            normalize_code
        } else {
            carry_code
        });
    }
    let mut normalize_buffer: Vec<u8> = Vec::new();
    let normalize_code = normalize_only(args, rules, &mut normalize_buffer)?;
    let mut carry_buffer: Vec<u8> = Vec::new();
    let carry_code = carry_only(args, &mut carry_buffer)?;
    let mut parts = serde_json::Map::new();
    parts.insert("verify".to_string(), before.to_json());
    parts.insert("normalize".to_string(), captured(&normalize_buffer));
    parts.insert("carry".to_string(), captured(&carry_buffer));
    write_json(out, &Value::Object(parts));
    // A repair that leaves something broken is the verdict; a carry that could not run is
    // "could not run". The worse of the two is what a script sees.
    Ok(if normalize_code != EXIT_OK {
        normalize_code
    } else {
        carry_code
    })
}

/// The JSON one step printed, or `null` when it printed none (a step that failed early
/// prints a human reason, and a script must see that as "no answer", not as an empty one).
fn captured(buffer: &[u8]) -> Value {
    serde_json::from_slice::<Value>(buffer).unwrap_or(Value::Null)
}

fn print_report(report: &Report, args: &Args, out: &mut dyn Write) {
    if args.json {
        let text = serde_json::to_string_pretty(&report.to_json())
            .unwrap_or_else(|_| "{}".to_string());
        let _ = writeln!(out, "{}", text);
    } else {
        let _ = write!(out, "{}", report.to_text(args.samples));
    }
}

/// The verdict: a broken finding fails the run. A check that could not run is *printed*
/// (and carried in `--json`) rather than turned into a failure, because "the engine is not
/// built" is not a statement about the archive — `verify` says so on its own line, every
/// time.
fn verdict_of(report: &Report) -> i32 {
    if report.is_clean() {
        EXIT_OK
    } else {
        EXIT_FINDINGS
    }
}

fn normalize_only(
    args: &Args,
    rules: &dyn atlas_ingest::Rules,
    out: &mut dyn Write,
) -> crate::Result<i32> {
    let report = verify(&args.db, rules, args.limits)?;
    let plan = normalize::plan(&report);
    if !args.apply {
        if args.json {
            let value = serde_json::json!({
                "dry_run": true,
                "database": report.database,
                "statements": plan_json(&plan),
                "statements_planned": plan.batches.len(),
                "rows": plan.rows(),
                "before": report.to_json(),
            });
            write_json(out, &value);
            return Ok(verdict_of(&report));
        }
        let _ = writeln!(
            out,
            "ATLAS archive normalize — dry run (nothing was written; pass --apply to run)\ndatabase: {}",
            args.db.display()
        );
        let _ = writeln!(out, "\n{} statement(s), {} row(s) would change\n", plan.batches.len(), plan.rows());
        let _ = write!(out, "{}", plan.to_text());
        if !report.skipped.is_empty() {
            let _ = writeln!(out, "\nnot checked");
            for reason in &report.skipped {
                let _ = writeln!(out, "  - {}", reason);
            }
        }
        return Ok(verdict_of(&report));
    }
    let applied = normalize::apply(&args.db, &plan)?;
    let after = verify(&args.db, rules, args.limits)?;
    // `--json` prints one object and nothing else: a script that reads stdout cannot parse
    // a human header glued to a JSON body, so the header only exists in the text mode.
    if args.json {
        let value = serde_json::json!({
            "dry_run": false,
            "database": report.database,
            "applied": {
                "statements": applied.statements,
                "rows": applied.rows,
                "full_text_rebuilt": applied.fts_rebuilt,
            },
            "statements": plan_json(&plan),
            "before": report.to_json(),
            "after": after.to_json(),
        });
        write_json(out, &value);
        return Ok(if after.is_clean() { EXIT_OK } else { EXIT_FINDINGS });
    }
    let _ = writeln!(
        out,
        "ATLAS archive normalize — applied\ndatabase: {}",
        args.db.display()
    );
    let _ = writeln!(
        out,
        "\n{} statement(s) run, {} row(s) changed{}",
        applied.statements,
        applied.rows,
        if applied.fts_rebuilt {
            ", full-text index rebuilt"
        } else {
            ""
        }
    );
    let _ = writeln!(
        out,
        "before: {} broken finding(s) · after: {}",
        report.broken().len(),
        after.broken().len()
    );
    if applied.rows == 0 && applied.statements > 0 {
        let _ = writeln!(
            out,
            "every statement was already satisfied: the armed plan had nothing left to change"
        );
    }
    if !after.findings.is_empty() {
        let _ = writeln!(out, "\nremaining");
        for finding in &after.findings {
            let _ = writeln!(out, "  {}", finding.line());
        }
    }
    Ok(if after.is_clean() { EXIT_OK } else { EXIT_FINDINGS })
}

/// The plan as JSON, one object per statement: the kind, the table, the SQL, the rows it
/// will change, the note, and the ids the finding named.
fn plan_json(plan: &normalize::Plan) -> serde_json::Value {
    serde_json::json!(
        plan.batches
            .iter()
            .map(|batch| serde_json::json!({
                "kind": batch.kind.id(),
                "table": batch.subject,
                "sql": batch.sql,
                "rows": batch.rows(),
                "note": batch.note,
                "bound_rows": batch.params.len(),
            }))
            .collect::<Vec<serde_json::Value>>()
    )
}

fn write_json(out: &mut dyn Write, value: &serde_json::Value) {
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string())
    );
}

fn carry_only(args: &Args, out: &mut dyn Write) -> crate::Result<i32> {
    let root = resolve_root(&args.db, args.root.as_deref());
    let outcome = carry::run(&args.db, &root, &args.carry)?;
    // A run that could not reach the model proves nothing about the archive: that is
    // "could not run", not "clean" and not "broken". It must not say "applied" either.
    // `outcome.refused` is set by the pass itself; the words it prints are for a human
    // and may change without changing the exit code.
    let refused = outcome.refused;
    let missing = outcome
        .missing_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<String>>()
        .join(",");
    if args.json {
        let value = serde_json::json!({
            "dry_run": args.carry.dry_run,
            "refused": refused,
            "candidates": outcome.candidates,
            "considered": outcome.considered,
            "classified": outcome.classified,
            "unclassified": outcome.unclassified,
            "already_carried": outcome.already,
            "failed": outcome.failed,
            "topics_written": outcome.topics,
            "entity_types_replaced": outcome.retyped,
            "entity_types_confirmed": outcome.confirmed,
            "entity_types_unclassified": outcome.untyped,
            "heuristic_writes_reverted": outcome.reverted,
            "missing_ids": outcome.missing_ids,
            "stopped": outcome.stopped,
        });
        write_json(out, &value);
    } else {
        let _ = writeln!(
            out,
            "ATLAS archive carry — {}\ndatabase: {}",
            if refused {
                "nothing written (the model was not available; see below)"
            } else if args.carry.dry_run {
                "dry run (nothing was written; pass --apply to run)"
            } else {
                "applied"
            },
            args.db.display()
        );
        let _ = writeln!(
            out,
            "\ncandidates {} · considered {} · already carried {} · failed {}",
            outcome.candidates, outcome.considered, outcome.already, outcome.failed
        );
        if !missing.is_empty() {
            let _ = writeln!(out, "no article with id {}", missing);
        }
        if !args.carry.dry_run && !refused {
            let _ = writeln!(
                out,
                "classified by the model {} · left unclassified {} · heuristic writes reverted {}",
                outcome.classified, outcome.unclassified, outcome.reverted
            );
            let _ = writeln!(
                out,
                "entity types replaced {} · confirmed {} · left unclassified {} · topic writes {}",
                outcome.retyped, outcome.confirmed, outcome.untyped, outcome.topics
            );
        }
        if !outcome.stopped.is_empty() {
            let _ = writeln!(out, "stopped: {}", outcome.stopped);
        }
    }
    Ok(if refused { EXIT_REFUSED } else { EXIT_OK })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn a_dry_run_is_the_default_for_every_writing_command() {
        let parsed = parse(&args(&["normalize"])).expect("parse").expect("a command");
        assert!(!parsed.apply, "normalize must not write by default");
        assert!(parsed.carry.dry_run);
        let parsed = parse(&args(&["carry"])).expect("parse").expect("a command");
        assert!(!parsed.apply);
        assert!(parsed.carry.dry_run);
        let parsed = parse(&args(&["normalize", "--apply"])).expect("parse").expect("a command");
        assert!(parsed.apply);
    }

    #[test]
    fn flags_carry_their_values_and_a_bad_one_is_refused() {
        let parsed = parse(&args(&[
            "carry",
            "--db",
            "/tmp/x.db",
            "--limit",
            "5",
            "--rate-ms",
            "0",
            "--ids",
            "3,4",
            "--no-retype",
            "--topics-only",
            "--max-minutes",
            "2",
        ]))
        .expect("parse")
        .expect("a command");
        assert_eq!(parsed.db, PathBuf::from("/tmp/x.db"));
        assert_eq!(parsed.carry.limit, 5);
        assert_eq!(parsed.carry.rate_ms, 0);
        assert_eq!(parsed.carry.ids, vec![3, 4]);
        assert!(!parsed.carry.retype);
        assert!(parsed.carry.topics_only);
        assert_eq!(parsed.carry.max_minutes, Some(2));
        assert!(parse(&args(&["carry", "--limit"])).is_err());
        assert!(parse(&args(&["carry", "--ids", "x"])).is_err());
        assert!(parse(&args(&["nonsense"])).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn the_bounded_checks_take_their_bounds_from_the_command_line() {
        // One engine process per row: a bound a script cannot set is a bound that turns
        // `verify` into an afternoon on an archive whose publisher column a migration
        // emptied.
        let parsed = parse(&args(&[
            "verify",
            "--place-limit",
            "7",
            "--publisher-limit",
            "9",
            "--probe",
            "3",
            "--sample",
            "4",
        ]))
        .expect("parse")
        .expect("a command");
        assert_eq!(parsed.limits.place, 7);
        assert_eq!(parsed.limits.publisher, 9);
        assert_eq!(parsed.limits.probe, 3);
        assert_eq!(parsed.limits.sample, 4);
        assert!(parse(&args(&["verify", "--publisher-limit", "x"])).is_err());
    }

    #[test]
    fn help_prints_the_usage_and_exits_zero() {
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(run(&args(&["--help"]), &mut out), EXIT_OK);
        assert!(String::from_utf8_lossy(&out).contains("--apply"));
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(run(&args(&["oops"]), &mut out), EXIT_USAGE);
    }

    #[test]
    fn a_missing_database_is_could_not_run_not_a_finding() {
        let path = std::env::temp_dir().join("atlas-migrate-absent.db");
        let _ = std::fs::remove_file(&path);
        let mut out: Vec<u8> = Vec::new();
        let code = run(
            &args(&["verify", "--db", path.to_str().unwrap_or("")]),
            &mut out,
        );
        assert_eq!(code, EXIT_REFUSED);
        assert!(String::from_utf8_lossy(&out).contains("no database"));
    }
}
