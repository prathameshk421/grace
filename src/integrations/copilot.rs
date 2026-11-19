//! copilot: not implemented yet.
use super::{HookResult, Integration};
use anyhow::{bail, Result};

pub static INTEGRATION: Integration = Integration {
    name: "copilot",
    tool: "copilot",
    setup,
    reads_stdin: false,
    hook,
    stdout: "",
};

fn setup(_exe: &str, _remove: bool) -> Result<String> {
    bail!("copilot setup is not implemented yet")
}

fn hook(_args: &[String], _stdin: &str) -> HookResult {
    HookResult::default()
}
