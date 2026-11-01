use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::LazyLock;

use clap::ValueEnum;
use serde::Deserialize;
use uuid::Uuid;

use crate::error::Error;
use crate::process::{self, Cancellation, Cli, NoExecutable, StderrTracer};

pub static TASK_SCHEMA: LazyLock<String> =
    LazyLock::new(|| schema(include_str!("agent/task.schema.json")));

// Kept on one line: Windows cannot pass an argument containing a newline to a batch-file shim.
pub fn schema(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .expect("the schema is valid JSON")
        .to_string()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum AgentKind {
    #[default]
    Claude,
    Codex,
}

impl AgentKind {
    pub fn agent(self) -> Agent {
        let (argv, schema_arg, session, resume): (&[&str], _, _, _) = match self {
            Self::Claude => (
                &["claude", "--dangerously-skip-permissions", "--print"],
                SchemaArg::Inline("--json-schema"),
                Session::Assigned("--session-id"),
                &["claude", "--resume"][..],
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
                Session::FromEvents("--json"),
                &["codex", "resume"][..],
            ),
        };
        let name = self.to_possible_value().expect("no agent kind is skipped");
        Agent {
            name: name.get_name().to_owned(),
            cli: Cli::new(argv.iter().copied()).tracer(StderrTracer),
            schema_arg,
            session,
            resume,
        }
    }

    pub fn first_available() -> Result<Agent, NoExecutable> {
        process::first_available(
            Self::value_variants().iter().map(|kind| kind.agent()),
            |agent| agent.cli.executable(),
        )
    }
}

pub enum SchemaArg {
    Inline(&'static str),
    File(&'static str),
}

impl SchemaArg {
    fn args(&self, schema: &str, scratch: &Path) -> io::Result<Vec<OsString>> {
        Ok(match self {
            Self::Inline(flag) => vec![flag.into(), schema.into()],
            Self::File(flag) => {
                let path = scratch.join("schema.json");
                fs::write(&path, schema)?;
                vec![flag.into(), path.into()]
            }
        })
    }
}

pub enum Session {
    Assigned(&'static str),
    FromEvents(&'static str),
}

impl Session {
    fn start(&self) -> (Vec<OsString>, Option<String>) {
        match self {
            Self::Assigned(flag) => {
                let id = Uuid::new_v4().to_string();
                (vec![flag.into(), id.clone().into()], Some(id))
            }
            Self::FromEvents(flag) => (vec![flag.into()], None),
        }
    }

    fn read(&self, assigned: Option<String>, output: String) -> (Option<String>, String) {
        match self {
            Self::Assigned(_) => (assigned, output),
            Self::FromEvents(_) => {
                let (mut id, mut answer) = (None, String::new());
                for event in output
                    .lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                {
                    match event {
                        Event::ThreadStarted { thread_id } => id = Some(thread_id),
                        Event::ItemCompleted {
                            item: Item::AgentMessage { text },
                        } => answer = text,
                        _ => {}
                    }
                }
                (id, answer)
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    #[serde(rename = "thread.started")]
    ThreadStarted { thread_id: String },
    #[serde(rename = "item.completed")]
    ItemCompleted { item: Item },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Item {
    #[serde(rename = "agent_message")]
    AgentMessage { text: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Outcome {
    completed: bool,
    report: String,
}

pub struct Performed {
    pub report: String,
    pub resume: String,
}

impl fmt::Display for Performed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n\nResume: {}", self.report.trim(), self.resume)
    }
}

pub struct Agent {
    pub name: String,
    pub cli: Cli,
    pub schema_arg: SchemaArg,
    pub session: Session,
    pub resume: &'static [&'static str],
}

impl Agent {
    pub fn ask(
        &self,
        prompt: &str,
        schema: Option<&str>,
        cwd: Option<&Path>,
        cancellation: &Cancellation,
    ) -> Result<String, Error> {
        self.invoke(Vec::new(), prompt, schema, cwd, cancellation)
    }

    pub fn perform(&self, task: &str, cancellation: &Cancellation) -> Result<Performed, Error> {
        let prompt = format!(
            "{task}\n\n\
             When you are done, answer as JSON matching the given schema. Set completed to false if \
             you could not finish the task, and put in report the answer, or what you did and why."
        );
        let (args, assigned) = self.session.start();
        let output = self.invoke(args, &prompt, Some(&TASK_SCHEMA), None, cancellation)?;
        let (id, answer) = self.session.read(assigned, output);
        let id = id.ok_or_else(|| Error::NoSession(self.name.clone()))?;
        let resume = self
            .resume
            .iter()
            .copied()
            .chain([id.as_str()])
            .collect::<Vec<_>>()
            .join(" ");
        let outcome: Outcome =
            serde_json::from_str(&answer).map_err(|source| Error::InvalidOutcome {
                agent: self.name.clone(),
                source,
                output: answer,
                resume: resume.clone(),
            })?;
        if !outcome.completed {
            return Err(Error::Incomplete {
                agent: self.name.clone(),
                report: outcome.report,
                resume,
            });
        }
        Ok(Performed {
            report: outcome.report,
            resume,
        })
    }

    fn invoke(
        &self,
        mut args: Vec<OsString>,
        prompt: &str,
        schema: Option<&str>,
        cwd: Option<&Path>,
        cancellation: &Cancellation,
    ) -> Result<String, Error> {
        let scratch = tempfile::Builder::new()
            .prefix("ncli-")
            .tempdir()
            .map_err(Error::WorkingDirectory)?;
        if let Some(schema) = schema {
            args.extend(
                self.schema_arg
                    .args(schema, scratch.path())
                    .map_err(Error::WorkingDirectory)?,
            );
        }
        Ok(self.cli.run(&args, Some(prompt), cwd, cancellation)?)
    }
}
