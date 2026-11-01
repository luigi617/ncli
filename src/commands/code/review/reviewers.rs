use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use clap::ValueEnum;

use crate::error::Error;
use crate::process::{Cancellation, Cli, StderrTracer};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ReviewerKind {
    #[default]
    Claude,
    Codex,
}

impl ReviewerKind {
    pub fn reviewer(self) -> Reviewer {
        let (argv, schema_arg): (&[&str], _) = match self {
            Self::Claude => (
                &["claude", "--dangerously-skip-permissions", "--print"],
                SchemaArg::Inline("--json-schema"),
            ),
            Self::Codex => (
                &[
                    "codex",
                    "exec",
                    "--sandbox",
                    "workspace-write",
                    "--config",
                    "sandbox_workspace_write.network_access=true",
                    "--skip-git-repo-check",
                ],
                SchemaArg::File("--output-schema"),
            ),
        };
        let name = self
            .to_possible_value()
            .expect("no reviewer kind is skipped");
        Reviewer {
            name: name.get_name().to_owned(),
            cli: Cli::new(argv.iter().copied())
                .timeout(Duration::from_secs(30 * 60))
                .attempts(3)
                .retry_delay(Duration::from_secs(10))
                .tracer(StderrTracer),
            schema_arg,
        }
    }
}

pub enum SchemaArg {
    Inline(&'static str),
    File(&'static str),
}

impl SchemaArg {
    fn args(&self, schema: &str, workdir: &Path) -> io::Result<Vec<OsString>> {
        Ok(match self {
            Self::Inline(flag) => vec![flag.into(), schema.into()],
            Self::File(flag) => {
                let path = workdir.join("schema.json");
                fs::write(&path, schema)?;
                vec![flag.into(), path.into()]
            }
        })
    }
}

pub struct Reviewer {
    pub name: String,
    pub cli: Cli,
    pub schema_arg: SchemaArg,
}

impl Reviewer {
    pub fn ask(
        &self,
        prompt: &str,
        schema: Option<&str>,
        cancellation: &Cancellation,
    ) -> Result<String, Error> {
        let workdir = tempfile::Builder::new()
            .prefix("ncli-review-")
            .tempdir()
            .map_err(Error::WorkingDirectory)?;
        let args = schema
            .map(|schema| self.schema_arg.args(schema, workdir.path()))
            .transpose()
            .map_err(Error::WorkingDirectory)?
            .unwrap_or_default();
        Ok(self
            .cli
            .run(&args, Some(prompt), Some(workdir.path()), cancellation)?)
    }
}
