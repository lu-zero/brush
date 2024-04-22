use clap::Parser;

use crate::builtin::{BuiltinCommand, BuiltinExitCode};

#[derive(Parser)]
pub(crate) struct JobsCommand {
    #[arg(short = 'l')]
    also_show_pids: bool,

    #[arg(short = 'n')]
    list_changed_only: bool,

    #[arg(short = 'p')]
    show_pids_only: bool,

    #[arg(short = 'r')]
    running_jobs_only: bool,

    #[arg(short = 's')]
    stopped_jobs_only: bool,

    // TODO: Add -x option
    job_specs: Vec<String>,
}

#[async_trait::async_trait]
impl BuiltinCommand for JobsCommand {
    async fn execute(
        &self,
        _context: crate::context::CommandExecutionContext<'_>,
    ) -> Result<crate::builtin::BuiltinExitCode, crate::error::Error> {
        // TODO: jobs!
        Ok(BuiltinExitCode::Success)
    }
}
