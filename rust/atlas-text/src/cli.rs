//! `cli` — the process boundary, and nothing under it: argv, stdin, stdout, exit
//! code.
//!
//! `main_entry` is the single call `src/main.rs` makes. It reads `argv[1]` and all
//! of stdin, hands both to [`crate::run`] — the one envelope, the one an
//! in-process caller calls too — prints the line it gets back, and turns a
//! refusal into `{"error": …}` with exit status 1.
//!
//! It decides no string rule and no request rule: which operations exist, how
//! large a request may be and what each answer looks like are
//! [`crate::envelope`]'s. The only refusal that lives here is the one `run`
//! cannot produce, because it is about the pipe rather than the request: stdin
//! that cannot be read.

use crate::envelope::{refusal, run};

pub fn main_entry() {
    let args: Vec<String> = std::env::args().collect();
    let op = args.get(1).cloned().unwrap_or_default();
    let mut input = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
        println!("{}", refusal("cannot read stdin"));
        std::process::exit(1);
    }
    match run(&op, &input) {
        Ok(answer) => println!("{}", answer),
        Err(message) => {
            println!("{}", refusal(&message));
            std::process::exit(1);
        }
    }
}
