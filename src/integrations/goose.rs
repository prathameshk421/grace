//! goose: not implemented yet.
use super::{HookResult, Integration};
use anyhow::{bail, Result};

pub static INTEGRATION: Integration = Integration {
    name: "goose",
    tool: "goose",
    setup,
    reads_stdin: false,
    hook,
    stdout: "",
};

fn setup(_exe: &str, _remove: bool) -> Result<String> {
    bail!("goose setup is not implemented yet")
}

fn hook(_args: &[String], _stdin: &str) -> HookResult {
    HookResult::default()
}
