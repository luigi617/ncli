use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use clap::ValueEnum;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::error::Error;
use crate::process::{self, Cancellation, Cli, NoExecutable, ProcessError, StderrTracer};

pub const STYLE: &str = include_str!("agent/style.md");

pub const WORKDIR: &str = "ncli-";

pub static TASK_SCHEMA: LazyLock<String> = LazyLock::new(|| task_schema("{}"));

// Kept on one line: Windows cannot pass an argument containing a newline to a batch-file shim.
pub fn schema(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .expect("the schema is valid JSON")
        .to_string()
}

// The task schema with more required fields, given as a JSON object of their schemas.
pub fn task_schema(fields: &str) -> String {
    let mut task: serde_json::Value = serde_json::from_str(include_str!("agent/task.schema.json"))
        .expect("the task schema is valid JSON");
    let fields: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(fields).expect("the fields are a JSON object");
    for (name, field) in fields {
        task["required"]
            .as_array_mut()
            .expect("the task schema lists its required fields")
            .push(name.clone().into());
        task["properties"][name] = field;
    }
    task.to_string()
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

    // Claude nests a session's subagents below it, and they repeat its work.
    pub fn sessions(self) -> Option<Sessions> {
        let (var, home, dir, depth) = match self {
            Self::Claude => ("CLAUDE_CONFIG_DIR", ".claude", "projects", Some(2)),
            Self::Codex => ("CODEX_HOME", ".codex", "sessions", None),
        };
        let root = env::var_os(var)
            .filter(|root| !root.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home_dir| home_dir.join(home)))?;
        Some(Sessions {
            dir: root.join(dir),
            depth,
        })
    }

    pub fn first_available() -> Result<Agent, NoExecutable> {
        process::first_available(
            Self::value_variants().iter().map(|kind| kind.agent()),
            |agent| agent.cli.executable(),
        )
    }
}

pub struct Sessions {
    pub dir: PathBuf,
    pub depth: Option<usize>,
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

    fn read(&self, assigned: Option<String>, output: &str) -> (Option<String>, String) {
        match self {
            Self::Assigned(_) => (assigned, output.to_owned()),
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
struct Outcome {
    completed: bool,
    report: String,
    #[serde(flatten)]
    fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoFields {}

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
    // Codex writes only inside its working directory unless given more; Claude takes the same flag.
    pub fn writable(self, dir: &Path) -> Self {
        Self {
            cli: self.cli.arg("--add-dir").arg(dir),
            ..self
        }
    }

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
        self.perform_as(task, &TASK_SCHEMA, cancellation)
            .map(|(performed, NoFields {})| performed)
    }

    // The schema extends the task schema with the fields of T, as task_schema builds it.
    pub fn perform_as<T: DeserializeOwned>(
        &self,
        task: &str,
        schema: &str,
        cancellation: &Cancellation,
    ) -> Result<(Performed, T), Error> {
        let (answer, id) = self.attempt(task, schema, None, cancellation);
        let Some(id) = id else {
            answer?;
            return Err(Error::NoSession(self.name.clone()));
        };
        let resume = self
            .resume
            .iter()
            .copied()
            .chain([id.as_str()])
            .collect::<Vec<_>>()
            .join(" ");
        let (report, fields) =
            answer
                .and_then(|answer| self.outcome(answer))
                .map_err(|source| Error::Resumable {
                    source: Box::new(source),
                    resume: resume.clone(),
                })?;
        Ok((Performed { report, resume }, fields))
    }

    // Starts in an empty directory instead of the user's, which limits only where the agent works by
    // default, not what it can reach. Its session cannot be resumed: agents keep sessions per
    // directory, and this one is removed afterwards.
    pub fn perform_isolated(
        &self,
        task: &str,
        cancellation: &Cancellation,
    ) -> Result<String, Error> {
        self.perform_isolated_as(task, &TASK_SCHEMA, cancellation)
            .map(|(report, NoFields {})| report)
    }

    // The schema extends the task schema with the fields of T, as task_schema builds it.
    pub fn perform_isolated_as<T: DeserializeOwned>(
        &self,
        task: &str,
        schema: &str,
        cancellation: &Cancellation,
    ) -> Result<(String, T), Error> {
        let workdir = tempfile::Builder::new()
            .prefix(WORKDIR)
            .tempdir()
            .map_err(Error::WorkingDirectory)?;
        self.attempt(task, schema, Some(workdir.path()), cancellation)
            .0
            .and_then(|answer| self.outcome(answer))
    }

    fn attempt(
        &self,
        task: &str,
        schema: &str,
        cwd: Option<&Path>,
        cancellation: &Cancellation,
    ) -> (Result<String, Error>, Option<String>) {
        let prompt = format!(
            "{task}\n\n\
             When you are done, answer as JSON matching the given schema. Set completed to false if \
             you could not finish the task, and put in report the answer, or what you did and why. \
             {STYLE}"
        );
        let (args, assigned) = self.session.start();
        let result = self.invoke(args, &prompt, Some(schema), cwd, cancellation);
        let ran = match &result {
            Ok(output) => Some(output.as_str()),
            Err(Error::Process(error)) if !matches!(error, ProcessError::Spawn { .. }) => {
                Some(error.stdout())
            }
            Err(_) => None,
        };
        let (id, answer) = ran.map_or((None, String::new()), |output| {
            self.session.read(assigned, output)
        });
        (result.map(|_| answer), id)
    }

    fn outcome<T: DeserializeOwned>(&self, answer: String) -> Result<(String, T), Error> {
        let invalid = |source, output| Error::InvalidAnswer {
            agent: self.name.clone(),
            source,
            output,
        };
        let Outcome {
            completed,
            report,
            fields,
        } = serde_json::from_str(&answer).map_err(|source| invalid(source, answer.clone()))?;
        if !completed {
            return Err(Error::Incomplete {
                agent: self.name.clone(),
                report,
            });
        }
        let fields = T::deserialize(serde_json::Value::Object(fields))
            .map_err(|source| invalid(source, answer))?;
        Ok((report, fields))
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
            .prefix(WORKDIR)
            .tempdir()
            .map_err(Error::WorkingDirectory)?;
        if let Some(schema) = schema {
            args.extend(
                self.schema_arg
                    .args(schema, scratch.path())
                    .map_err(Error::WorkingDirectory)?,
            );
        }
        Ok(self
            .cli
            .run(&args, Some(prompt.as_bytes()), cwd, cancellation)?)
    }
}
