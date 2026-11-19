//! agy: not implemented yet.
use super::{HookResult, Integration};
use anyhow::{bail, Result};

pub static INTEGRATION: Integration = Integration {
    name: "agy",
    tool: "agy",
    setup,
    reads_stdin: false,
    hook,
    stdout: "",
};

fn setup(_exe: &str, _remove: bool) -> Result<String> {
    bail!("agy setup is not implemented yet")
}

fn hook(_args: &[String], _stdin: &str) -> HookResult {
    HookResult::default()
}
