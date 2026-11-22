//! The `import_model` arm, proved with a stand-in `atlas-setup`.
//!
//! What is real here: the child is a real process, started the way the product starts it
//! (`atlas_platform::child::command_with` — `env_clear` plus the allowlist), with the root
//! as its working directory and an argv the test reads back. What is scripted: the tool
//! itself, because the real `atlas-setup` would copy or download a multi-gigabyte model.
//!
//! The three promises, one per group below:
//!
//! * a successful import finishes the row and `data/config.json` names the model — read
//!   back from the file, not from the exit code;
//! * a refused import finishes the row with the *tool's* sentence and leaves the settings
//!   file byte for byte as it was;
//! * a tool that exits 0 and names nothing is not called a success.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use serde_json::Value;

use crate::import_model::{apply, missing_line, BINARY_NAME};
use crate::stop::Stop;

/// The stand-in `atlas-setup`, compiled once per test process by the machine's `rustc`.
///
/// A closure would prove nothing: the things this arm can get wrong are the argv, the
/// working directory, the environment and the exit status, and all four are the child's.
/// The program records the value it was given and the environment it inherited, obeys
/// `data/standin-mode` (`fail`: refuse with the tool's own `data/setup.json` sentence;
/// `silent`: exit 0 and name nothing), and otherwise names a model the way the real tool
/// does.
const STAND_IN: &str = r#"
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| -> String {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|at| args.get(at + 1))
            .cloned()
            .unwrap_or_default()
    };
    let root = std::path::PathBuf::from(value("--root"));
    let data = root.join("data");
    let _ = std::fs::create_dir_all(&data);
    let _ = std::fs::write(data.join("ran.txt"), value("--import-model"));
    let env: Vec<String> = std::env::vars()
        .map(|(name, value)| format!("{}={}", name, value))
        .collect();
    let _ = std::fs::write(data.join("env.txt"), env.join("\n"));
    let mode = std::fs::read_to_string(data.join("standin-mode")).unwrap_or_default();
    match mode.trim() {
        "fail" => {
            let _ = std::fs::write(
                data.join("setup.json"),
                "{\"ok\":false,\"message\":\"that file is not a .gguf: bring in a whole model file\"}",
            );
            std::process::exit(1);
        }
        "silent" => return,
        _ => {}
    }
    let models = root.join("models");
    let _ = std::fs::create_dir_all(&models);
    let model = models.join("imported.gguf");
    let _ = std::fs::write(&model, b"GGUF");
    let named = model.to_string_lossy().replace('\\', "/");
    let _ = std::fs::write(
        data.join("config.json"),
        format!("{{\"model_local\":\"{}\"}}\n", named),
    );
}
"#;

/// The compiled stand-in, built once per test process.
fn stand_in() -> Option<PathBuf> {
    static BUILT: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
            let dir = std::env::temp_dir().join(format!(
                "atlas-import-stand-in-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).ok()?;
            let source = dir.join("standin.rs");
            std::fs::write(&source, STAND_IN).ok()?;
            let program = dir.join("atlas-setup");
            // The environment is *not* cleared here, unlike a child of the product:
            // `rustc` is this test's own tool and needs `PATH` for the linker.
            let built = Command::new(rustc)
                .arg(&source)
                .arg("-o")
                .arg(&program)
                .arg("--edition=2021")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .ok()?;
            if !built.status.success() || !program.is_file() {
                eprintln!(
                    "the stand-in did not compile:\n{}",
                    String::from_utf8_lossy(&built.stderr)
                );
                return None;
            }
            Some(program)
        })
        .clone()
}

/// A project root with `data/`, `models/`, and a stand-in at the path the resolver reads
/// first (`rust/target/release/atlas-setup`); removed when the test ends.
pub(crate) struct Root {
    pub(crate) path: PathBuf,
}

impl Root {
    pub(crate) fn new(tag: &str) -> Option<Root> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|spent| spent.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "atlas-import-{}-{}-{}",
            tag,
            std::process::id(),
            stamp
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("data")).ok()?;
        std::fs::create_dir_all(path.join("models")).ok()?;
        let from = stand_in()?;
        let release = path.join("rust").join("target").join("release");
        std::fs::create_dir_all(&release).ok()?;
        let to = release.join(BINARY_NAME);
        std::fs::copy(&from, &to).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut mode = std::fs::metadata(&to).ok()?.permissions();
            mode.set_mode(0o755);
            std::fs::set_permissions(&to, mode).ok()?;
        }
        Some(Root { path })
    }

    fn mode(&self, mode: &str) {
        std::fs::write(self.path.join("data/standin-mode"), mode).expect("mode");
    }

    /// The queue that belongs to this root, so a loop test can push the row the window
    /// would push.
    pub(crate) fn queue(&self) -> atlas_store::requests::Requests {
        atlas_store::requests::Requests::open(self.path.join("data/requests.db"))
            .expect("open the queue")
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn payload(source: &str) -> Value {
    serde_json::json!({ "import_model": source, "why": "the model I already have" })
}

#[test]
fn an_import_names_the_model_and_is_reported_as_one_setting_in_force() {
    let root = match Root::new("ok") {
        Some(root) => root,
        None => panic!("the stand-in did not compile"),
    };
    let source = "/Users/somebody/models/qwen.gguf";
    let acted = apply(&payload(source), &root.path, &Stop::idle()).expect("handled");
    assert_eq!(acted.error, None, "{acted:?}");
    assert_eq!(acted.stored, 1, "one setting is now in force");
    // The value arrived as one argument, unquoted and unparsed.
    let ran = std::fs::read_to_string(root.path.join("data/ran.txt")).expect("ran.txt");
    assert_eq!(ran, source);
    // And the file the launcher builds llama.cpp's argv from names the model.
    let config = std::fs::read_to_string(root.path.join("data/config.json")).expect("config");
    assert!(config.contains("imported.gguf"), "{config}");
}

#[test]
fn the_child_gets_the_allowlist_and_not_the_agents_own_environment() {
    let root = match Root::new("env") {
        Some(root) => root,
        None => panic!("the stand-in did not compile"),
    };
    // A variable only this process sets: the arm must not hand it to the tool.
    std::env::set_var("ATLAS_IMPORT_HOSTILE", "1");
    let acted = apply(&payload("/tmp/x.gguf"), &root.path, &Stop::idle()).expect("handled");
    assert_eq!(acted.error, None, "{acted:?}");
    let env = std::fs::read_to_string(root.path.join("data/env.txt")).expect("env.txt");
    assert!(
        !env.contains("ATLAS_IMPORT_HOSTILE"),
        "the tool inherited the agent's environment: {env}"
    );
    assert!(
        env.contains("PATH="),
        "the allowlist still carries PATH, or the child cannot run: {env}"
    );
}

#[test]
fn a_refused_import_finishes_with_the_tools_own_sentence_and_keeps_the_file() {
    let root = match Root::new("refused") {
        Some(root) => root,
        None => panic!("the stand-in did not compile"),
    };
    let written = root.path.join("data/config.json");
    std::fs::write(&written, "{\"model_context\":65536}\n").expect("config");
    let before = std::fs::read(&written).expect("config");
    root.mode("fail");
    let acted = apply(&payload("/tmp/notamodel.zip"), &root.path, &Stop::idle()).expect("handled");
    let sentence = acted.error.expect("refused");
    assert!(
        sentence.contains("not a .gguf"),
        "the tool's own answer travels, not a generic one: {sentence}"
    );
    assert_eq!(acted.stored, 0, "a refusal stored nothing");
    assert_eq!(
        std::fs::read(&written).expect("config"),
        before,
        "a refused import rewrote the settings file"
    );
}

#[test]
fn a_tool_that_exits_zero_and_names_nothing_is_not_a_success() {
    let root = match Root::new("silent") {
        Some(root) => root,
        None => panic!("the stand-in did not compile"),
    };
    root.mode("silent");
    let acted = apply(&payload("/tmp/x.gguf"), &root.path, &Stop::idle()).expect("handled");
    let sentence = acted.error.expect("refused");
    assert!(sentence.contains("names no model"), "{sentence}");
    assert_eq!(acted.stored, 0);
    assert!(
        !root.path.join("data/config.json").exists(),
        "an empty config was invented"
    );
}

#[test]
fn a_payload_with_no_source_is_refused_before_any_child_runs() {
    let root = match Root::new("empty") {
        Some(root) => root,
        None => panic!("the stand-in did not compile"),
    };
    for empty in ["", "   "] {
        let acted = apply(&payload(empty), &root.path, &Stop::idle()).expect("handled");
        assert!(acted.error.expect("refused").contains("is empty"));
        assert_eq!(acted.stored, 0);
    }
    assert!(!root.path.join("data/ran.txt").exists(), "nothing ran");
}

#[test]
fn a_request_that_is_not_about_importing_is_not_this_arms_business() {
    let root = match Root::new("other") {
        Some(root) => root,
        None => panic!("the stand-in did not compile"),
    };
    let settings = serde_json::json!({ "settings": { "model_context": 8192 } });
    assert!(apply(&settings, &root.path, &Stop::idle()).is_none());
    let engine = serde_json::json!({ "key": "search_engine", "value": "x" });
    assert!(apply(&engine, &root.path, &Stop::idle()).is_none());
    // A value of the wrong type is refused by shape, and no child runs.
    let number = serde_json::json!({ "import_model": 7 });
    let acted = apply(&number, &root.path, &Stop::idle()).expect("handled");
    let sentence = acted.error.expect("refused");
    assert!(sentence.contains("has to be the path"), "{sentence}");
    assert!(sentence.contains("a number"), "the shape is named: {sentence}");
    assert!(!root.path.join("data/ran.txt").exists(), "nothing ran");
}

#[test]
fn a_machine_without_the_tool_is_told_by_name_path_and_fix() {
    // Pure: the sentence is built from a root, never from this machine's `PATH`.
    let root = std::env::temp_dir().join(format!("atlas-import-missing-{}", std::process::id()));
    let sentence = missing_line(&atlas_config::Paths::at(&root));
    assert!(sentence.contains(BINARY_NAME), "{sentence}");
    assert!(sentence.contains("rust/target/release"), "{sentence}");
    assert!(sentence.contains("cargo build --release"), "{sentence}");
    assert!(sentence.contains("on PATH"), "{sentence}");
}
