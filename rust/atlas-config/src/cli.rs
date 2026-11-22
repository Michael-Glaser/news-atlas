//! The JSON CLI: one object on stdin, one on stdout — the shape every ATLAS
//! binary speaks, so a shell run and a Rust caller ask the same questions and a
//! script gets an object it can parse instead of prose it has to scrape.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::audit::audit_no_downloads;
use crate::config::Config;
use crate::paths::Paths;
use crate::ports::load_managed_at;
use crate::spec::BIND_HOST;
use crate::spawn::spawn_env;
use crate::value::value_as_text;

/// Hard cap on stdin, so a caller cannot make the binary buffer without bound.
pub const MAX_STDIN: usize = 4 * 1024 * 1024;

/// What the binary answers for `--help`.
///
/// This text is the operator documentation for the binary (there is no man page),
/// so it names every key a caller may read. The `cli` tests check the keys
/// against it: a shape that grows without a line here is a bug.
pub const USAGE: &str = "atlas-config: config | validate | paths | spawn_env | audit | set

One JSON object on stdin (may be empty), one object on stdout. Exit 0 on
success, 1 when the verdict is bad (validate, audit) or the request is
malformed. A malformed request prints {\"error\": \"<sentence>\"} and exits 1.

config     effective settings: config_file, root, values, sources, files,
           problems, notes, security, ok. A value ATLAS had to clamp is in
           notes, with the range and the value in force. {\"persist_ports\":
           false} reads data/config.json without writing a picked port into it.
validate   ok, count, problems, notes, bind_host; ok:false when a setting is
           unusable, and notes names every number that was clamped into range
paths      every derived path under the project root, plus posix, data_mode,
           file_mode
spawn_env  env and names: the exact environment a child process gets, with
           {\"extra\": {NAME: \"value\"}} appended when it is allowlisted
audit      ok, vacuous, findings, finding_count, scanned, skipped, root, read,
           rules. Walks the tree ({\"root\": \"...\"} defaults to this project);
           vacuous:true means no code file was read, so ok proves nothing.
set        writes validated settings to data/config.json (atomically, 0600) and
           answers changed, values, notes, config_file. Arguments are
           key=value pairs after --root DIR:
             atlas-config set --root /srv/atlas model_context=65536 model_answer=4096
           The settable keys are model_local and the five model_* numbers; a
           value out of its range is clamped and said in notes, a value that is
           not a number, or a key that is not settable, is refused by name and
           nothing is written. {\"settings\": {...}} on stdin works too.
";

fn render(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

/// One subcommand, plus the words after it (`atlas-config set k=v …`).
///
/// [`run_cli`] is the two-argument form every caller already knows; this one exists because
/// `set` takes its values as arguments rather than as JSON, which is what a person types.
pub fn run_with(cmd: &str, input: &str, words: &[String]) -> Result<(String, i32), String> {
    if matches!(cmd, "help" | "--help" | "-h") {
        return Ok((USAGE.to_string(), 0));
    }
    let request = if input.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str::<Value>(input)
            .map_err(|error| format!("stdin is not JSON ({})", error))?
    };
    if !request.is_object() {
        return Err("input must be a JSON object".to_string());
    }
    match cmd {
        "set" => set_settings(&request, words),
        "config" => {
            // Python's `load()` picked a never-stored port once and wrote it
            // down; `describe()` reported it as `auto`. Anything else is a pure
            // read, so a caller can ask without touching the disk — and saying
            // `false` is the only way to say that. A `"persist_ports": "no"`
            // used to count as yes and write the file, which is the wrong way
            // round for a typo.
            let persist = match request.get("persist_ports") {
                None => true,
                Some(Value::Bool(flag)) => *flag,
                Some(other) => {
                    return Err(format!(
                        "persist_ports must be true or false, not {} (pass false to read \
                         data/config.json without writing a picked port into it)",
                        value_as_text(Some(other))
                    ))
                }
            };
            let paths = Paths::discover();
            let config = if persist {
                load_managed_at(&paths).map_err(|error| error.to_string())?
            } else {
                Config::load_at(&paths).map_err(|error| error.to_string())?
            };
            let problems = config.validate();
            let mut described = config.describe();
            if let Value::Object(map) = &mut described {
                map.insert("ok".to_string(), Value::Bool(problems.is_empty()));
            }
            Ok((render(&described), 0))
        }
        "validate" => {
            let paths = Paths::discover();
            let config = Config::load_at(&paths).map_err(|error| error.to_string())?;
            let problems = config.validate();
            let mut map = Map::new();
            map.insert("ok".to_string(), Value::Bool(problems.is_empty()));
            map.insert("count".to_string(), Value::from(problems.len()));
            map.insert(
                "problems".to_string(),
                Value::Array(problems.iter().map(|problem| problem.to_json()).collect()),
            );
            // A clamp is not a problem — the app still starts — but `validate`
            // is the pre-flight a person runs, so it says what it moved.
            map.insert(
                "notes".to_string(),
                Value::Array(
                    config
                        .notes
                        .iter()
                        .map(|note| Value::String(note.clone()))
                        .collect(),
                ),
            );
            map.insert("bind_host".to_string(), Value::String(BIND_HOST.to_string()));
            let code = if problems.is_empty() { 0 } else { 1 };
            Ok((render(&Value::Object(map)), code))
        }
        "paths" => Ok((render(&Paths::discover().to_json()), 0)),
        "spawn_env" => {
            let extra: Vec<(String, String)> = match request.get("extra") {
                Some(Value::Object(map)) => map
                    .iter()
                    .map(|(key, value)| (key.clone(), value_as_text(Some(value))))
                    .collect(),
                _ => Vec::new(),
            };
            let env = spawn_env(&extra);
            let mut object = Map::new();
            let mut names = Vec::new();
            for (name, value) in &env {
                names.push(Value::String(name.clone()));
                object.insert(name.clone(), Value::String(value.clone()));
            }
            let mut map = Map::new();
            map.insert("env".to_string(), Value::Object(object));
            map.insert("names".to_string(), Value::Array(names));
            Ok((render(&Value::Object(map)), 0))
        }
        "audit" => {
            let root = match request.get("root").and_then(Value::as_str) {
                Some(text) if !text.trim().is_empty() => PathBuf::from(text.trim()),
                _ => Paths::discover().root,
            };
            let audit = audit_no_downloads(&root);
            let code = if audit.ok { 0 } else { 1 };
            Ok((render(&audit.to_json()), code))
        }
        other => Err(format!(
            "unknown subcommand: {} (config | validate | paths | spawn_env | audit | set)",
            other
        )),
    }
}

/// [`run_with`] for a caller with no arguments of its own — the two-argument shape.
pub fn run_cli(cmd: &str, input: &str) -> Result<(String, i32), String> {
    run_with(cmd, input, &[])
}

/// `set`: validate, write once, answer what is in force.
///
/// The values arrive as `key=value` words (what a person types) and/or as
/// `{"settings": {...}}` on stdin (what a caller with a map sends). Both go through the
/// same two functions the window's request goes through — [`crate::model_settings`] for the
/// rules and [`Config::save`] for the atomic 0600 write — so the CLI and the queue cannot
/// disagree about what a setting may be. Nothing is written when anything is refused: a
/// half-applied settings request is the failure mode this ordering exists to prevent.
fn set_settings(request: &Value, words: &[String]) -> Result<(String, i32), String> {
    let mut root: Option<PathBuf> = None;
    let mut settings: Map<String, Value> = match request.get("settings") {
        Some(Value::Object(map)) => map.clone(),
        Some(other) => {
            return Err(format!(
                "settings has to be an object of key/value pairs, not {}",
                value_as_text(Some(other))
            ))
        }
        None => Map::new(),
    };
    let mut index = 0;
    while index < words.len() {
        let word = words[index].trim();
        if word == "--root" {
            index += 1;
            let value = words
                .get(index)
                .ok_or_else(|| "--root needs a directory after it".to_string())?;
            root = Some(PathBuf::from(value));
        } else if let Some((key, value)) = word.split_once('=') {
            // The value stays text: `crate::model_settings` decides what a number is (a
            // numeric string is one, "lots" is not) and what a path is. Typing the value
            // here would be a second set of rules for the same question.
            settings.insert(key.trim().to_string(), Value::String(value.trim().to_string()));
        } else {
            return Err(format!(
                "{word:?} is not a setting: write key=value, for example model_context=65536"
            ));
        }
        index += 1;
    }
    if settings.is_empty() {
        return Err(
            "no settings were named: write key=value, for example model_context=65536"
                .to_string(),
        );
    }
    let paths = match root {
        Some(root) => Paths::at(root),
        None => Paths::discover(),
    };
    let overrides = crate::model_settings(&settings, &paths.root)?;
    let config = Config::save(&paths, &overrides.map).map_err(|error| error.to_string())?;
    let mut changed: Vec<Value> = overrides
        .map
        .keys()
        .map(|key| Value::String(key.clone()))
        .collect();
    changed.sort_by_key(|value| value.as_str().unwrap_or_default().to_string());
    // Every clamp is said out loud, in the same mechanism the rest of the crate uses: the
    // request's own notes first (what this call moved), then the file's (what was already
    // out of range when it was read).
    let mut notes: Vec<Value> = overrides.notes.iter().cloned().map(Value::String).collect();
    notes.extend(config.notes.iter().cloned().map(Value::String));
    let mut map = Map::new();
    map.insert(
        "config_file".to_string(),
        Value::String(crate::paths::show(&paths.config_file)),
    );
    map.insert("changed".to_string(), Value::Array(changed));
    map.insert("values".to_string(), config.values_json());
    map.insert("notes".to_string(), Value::Array(notes));
    Ok((render(&Value::Object(map)), 0))
}

/// Read stdin, refusing anything past [`MAX_STDIN`] instead of growing a buffer
/// without a bound.
pub fn read_stdin() -> Result<String, String> {
    // A terminal is not input: it is a person waiting. `bash rust/build.sh` hung for
    // 15 minutes here (measured 2026-11-21T18:05Z) because this call blocked on a
    // terminal that never reaches EOF, so an interactive `atlas-config audit` had
    // nothing to read and no way to say so. Empty input is what a caller gets with
    // nothing piped in, and every handler already treats it as "use the defaults".
    if std::io::stdin().is_terminal() {
        return Ok(String::new());
    }
    let stdin = std::io::stdin();
    let mut handle = stdin.lock().take((MAX_STDIN + 1) as u64);
    let mut buffer = String::new();
    if handle.read_to_string(&mut buffer).is_err() {
        return Err("cannot read stdin as utf-8".to_string());
    }
    if buffer.len() > MAX_STDIN {
        return Err(format!("stdin larger than {} bytes", MAX_STDIN));
    }
    Ok(buffer)
}

fn fail(message: &str) -> ! {
    let mut map = Map::new();
    map.insert("error".to_string(), Value::String(message.to_string()));
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{}", render(&Value::Object(map)));
    let _ = stdout.flush();
    std::process::exit(1);
}

/// The binary's `main`.
pub fn main_entry() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = match args.get(1) {
        Some(cmd) => cmd.as_str(),
        None => fail("missing subcommand (config | validate | paths | spawn_env | audit | set)"),
    };
    // The words after the subcommand: `set` takes its values as `key=value`, which is what
    // a person types. Every other subcommand ignores them, and `run_with` decides.
    let words: Vec<String> = args.iter().skip(2).cloned().collect();
    let input = match read_stdin() {
        Ok(input) => input,
        Err(error) => fail(&error),
    };
    match run_with(cmd, &input, &words) {
        Ok((output, code)) => {
            let mut stdout = std::io::stdout();
            let _ = writeln!(stdout, "{}", output);
            let _ = stdout.flush();
            if code != 0 {
                std::process::exit(code);
            }
        }
        Err(error) => fail(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;


#[test]
fn the_cli_answers_one_json_object_per_subcommand() {
    for cmd in ["validate", "paths", "spawn_env", "audit"] {
        let (out, code) = run_cli(cmd, "").expect(cmd);
        let parsed: Value = serde_json::from_str(&out).expect(cmd);
        assert!(parsed.is_object(), "{}", cmd);
        assert!(code == 0 || code == 1, "{}", cmd);
    }
    let (help, code) = run_cli("--help", "").expect("help");
    assert_eq!(code, 0);
    assert!(help.contains("validate"));
    assert!(run_cli("nonsense", "").is_err());
    assert!(run_cli("paths", "[1]").is_err());
    assert!(run_cli("paths", "{not json").is_err());
    let (out, code) = run_cli("spawn_env", r#"{"extra": {"ATLAS_X": "1"}}"#).expect("spawn_env");
    let parsed: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        parsed.get("env").and_then(|env| env.get("ATLAS_X")).and_then(Value::as_str),
        Some("1")
    );
    assert_eq!(code, 0);
}

#[test]
fn every_subcommand_answers_exactly_the_keys_the_usage_text_documents() {
    // The binary is the operator tool and its USAGE string is the only
    // documentation of its JSON, so the two are checked against each other: a
    // key that grows here without a line there fails, and so does the reverse.
    // `persist_ports: false` keeps this a read: the other mode writes a picked
    // port into the project's own `data/config.json`.
    let (out, code) = run_cli("config", r#"{"persist_ports": false}"#).expect("config");
    let described: Value = serde_json::from_str(&out).expect("config json");
    assert_eq!(code, 0);
    for key in [
        "config_file",
        "root",
        "values",
        "sources",
        "files",
        "problems",
        "notes",
        "security",
        "ok",
    ] {
        assert!(described.get(key).is_some(), "config is missing {}", key);
        assert!(USAGE.contains(key), "USAGE does not document config's {}", key);
    }
    assert!(
        described.get("values").and_then(Value::as_object).is_some(),
        "values must be an object"
    );
    assert!(
        described.get("problems").and_then(Value::as_array).is_some(),
        "problems must be an array"
    );
    assert!(
        described.get("ok").and_then(Value::as_bool).is_some(),
        "ok must be a boolean"
    );
    // Every documented setting is in `values`, inside its range — and every
    // whole-number row (the three ports included) is an integer there, because a
    // caller parses this JSON and `8080.0` is not a port.
    let values = described.get("values").and_then(Value::as_object).expect("values");
    for spec in crate::spec::SPEC.iter() {
        let value = values.get(spec.key).unwrap_or_else(|| panic!("{} missing", spec.key));
        let number = value.as_f64().unwrap_or_else(|| panic!("{} is not a number", spec.key));
        if spec.kind == crate::spec::Kind::Int {
            assert!(value.is_u64() || value.is_i64(), "{} is not an integer: {}", spec.key, value);
        }
        assert!(
            (spec.low..=spec.high).contains(&number),
            "{} is {} outside {}..={}",
            spec.key,
            number,
            spec.low,
            spec.high
        );
    }

    let (out, code) = run_cli("validate", "").expect("validate");
    let validated: Value = serde_json::from_str(&out).expect("validate json");
    assert!(code == 0 || code == 1);
    for key in ["ok", "count", "problems", "notes", "bind_host"] {
        assert!(validated.get(key).is_some(), "validate is missing {}", key);
        assert!(USAGE.contains(key), "USAGE does not document validate's {}", key);
    }
    assert!(
        validated.get("notes").and_then(Value::as_array).is_some(),
        "notes must be an array"
    );
    assert_eq!(
        validated.get("bind_host").and_then(Value::as_str),
        Some("127.0.0.1")
    );

    let (out, _) = run_cli("paths", "").expect("paths");
    let paths: Value = serde_json::from_str(&out).expect("paths json");
    // A path-keyed object: every documented path plus the mode answers.
    for key in ["root", "data_dir", "config_file", "db", "model_file"] {
        assert!(paths.get(key).and_then(Value::as_str).is_some(), "paths is missing {}", key);
    }
    for key in ["posix", "data_mode", "file_mode"] {
        assert!(paths.get(key).is_some(), "paths is missing {}", key);
        assert!(USAGE.contains(key), "USAGE does not document paths' {}", key);
    }

    let (out, _) = run_cli("spawn_env", "").expect("spawn_env");
    let spawn: Value = serde_json::from_str(&out).expect("spawn_env json");
    for key in ["env", "names"] {
        assert!(spawn.get(key).is_some(), "spawn_env is missing {}", key);
        assert!(USAGE.contains(key), "USAGE does not document spawn_env's {}", key);
    }
    assert_eq!(
        spawn.get("names").and_then(Value::as_array).map(Vec::len),
        spawn
            .get("env")
            .and_then(Value::as_object)
            .map(|env| env.len()),
        "names and env must describe the same variables"
    );

    let (out, code) = run_cli("audit", "").expect("audit");
    let audit: Value = serde_json::from_str(&out).expect("audit json");
    assert!(code == 0 || code == 1);
    for key in [
        "ok",
        "vacuous",
        "findings",
        "finding_count",
        "scanned",
        "skipped",
        "root",
        "read",
        "rules",
    ] {
        assert!(audit.get(key).is_some(), "audit is missing {}", key);
        assert!(USAGE.contains(key), "USAGE does not document audit's {}", key);
    }
}

#[test]
fn a_persist_ports_flag_that_is_not_a_boolean_is_refused_instead_of_writing() {
    // `{"persist_ports": "no"}` used to be read as "yes" and wrote the file.
    let error = run_cli("config", r#"{"persist_ports": "no"}"#).expect_err("must refuse");
    assert!(error.contains("persist_ports must be true or false"), "{}", error);
    assert!(error.contains("data/config.json"), "the fix is named: {}", error);
    assert!(run_cli("config", r#"{"persist_ports": false}"#).is_ok());
}

/// A project root of this test's own: a directory, its `data/`, and nothing else.
///
/// `set` writes what it is given, so a test that ran it against the real project would edit
/// the project's own `data/config.json`. `--root` is the door that lets the same code run
/// over a throwaway tree; nothing here touches `Paths::discover()`.
fn project(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "atlas-config-cli-{}-{}",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("data")).expect("data");
    root
}

#[test]
fn set_writes_the_numbers_it_was_given_and_they_are_read_back_unchanged() {
    let root = project("set");
    let at = root.to_string_lossy().to_string();
    let (out, code) = run_with(
        "set",
        "",
        &[
            "--root".to_string(),
            at.clone(),
            "model_context=65536".to_string(),
            "model_answer=4096".to_string(),
        ],
    )
    .expect("set");
    assert_eq!(code, 0);
    let answered: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(answered["changed"], serde_json::json!(["model_answer", "model_context"]));
    assert_eq!(answered["values"]["model_context"], serde_json::json!(65536));
    assert!(answered["notes"].as_array().map(Vec::is_empty).unwrap_or(false), "{answered}");

    // The promise is about the file, not about the answer: the process that reads
    // `data/config.json` next has to see the numbers this call was given.
    let paths = Paths::at(&root);
    let stored = crate::read_stored(&paths).expect("stored");
    let config = Config::build(&paths, &[], &stored);
    assert_eq!(config.model_context, 65536);
    assert_eq!(config.model_answer, 4096);

    // Both ends of a range are values, not clamps: 1024 is the low end of the context and
    // 256 the low end of an answer, and neither may be moved.
    run_with(
        "set",
        "",
        &[
            "--root".to_string(),
            at.clone(),
            "model_context=1024".to_string(),
            "model_answer=256".to_string(),
        ],
    )
    .expect("set the low ends");
    let config = Config::build(&paths, &[], &crate::read_stored(&paths).expect("stored"));
    assert_eq!((config.model_context, config.model_answer), (1024, 256));

    // A number outside its range is clamped, said in `notes`, and stored as the clamp: a
    // refusal here would leave the reader with no way to make the change they asked for.
    let (out, code) = run_with(
        "set",
        "",
        &[
            "--root".to_string(),
            at,
            "model_context=999999".to_string(),
        ],
    )
    .expect("set");
    assert_eq!(code, 0);
    let answered: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(answered["values"]["model_context"], serde_json::json!(262_144));
    assert!(
        answered["notes"]
            .as_array()
            .map(|notes| notes.iter().any(|note| note
                .as_str()
                .unwrap_or_default()
                .contains("1024 to 262144")))
            .unwrap_or(false),
        "the clamp is not said: {answered}"
    );
    assert_eq!(Config::build(&paths, &[], &crate::read_stored(&paths).expect("stored")).model_context, 262_144);

    // A word where a number belongs is refused, and the file keeps the byte it had.
    let before = std::fs::read(paths.config_file.clone()).expect("config file");
    let error = run_with(
        "set",
        "",
        &[
            "--root".to_string(),
            paths.root.to_string_lossy().to_string(),
            "model_answer=lots".to_string(),
        ],
    )
    .expect_err("refused");
    assert!(error.contains("model_answer = \"lots\" is not usable"), "{error}");
    assert!(error.contains("256 to 32768"), "the range is named: {error}");
    assert_eq!(
        std::fs::read(paths.config_file.clone()).expect("config file"),
        before,
        "a refusal wrote the settings file"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn set_refuses_a_word_that_is_not_a_setting_instead_of_guessing_what_it_meant() {
    let root = project("words");
    let error = run_with(
        "set",
        "",
        &[
            "--root".to_string(),
            root.to_string_lossy().to_string(),
            "model_context 65536".to_string(),
        ],
    )
    .expect_err("refused");
    assert!(error.contains("is not a setting"), "{error}");
    assert!(
        !Paths::at(&root).config_file.exists(),
        "a malformed word wrote the settings file"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failure_is_a_json_object_on_stdout_and_a_bad_command_says_which() {
    // A caller parses stdout, so an error is an object too — not a bare line.
    let err = run_cli("nonsense", "").expect_err("unknown subcommand");
    assert!(err.contains("nonsense"));
    assert!(err.contains("config | validate | paths | spawn_env | audit"), "{}", err);
    assert!(USAGE.contains("{\"error\""));
}
}
