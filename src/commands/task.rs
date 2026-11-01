use crate::agent::AgentKind;
use crate::commands::Context;
use crate::error::Error;

pub fn run(task: &str, context: &Context) -> Result<String, Error> {
    if task.trim().is_empty() {
        return Err(Error::EmptyTask);
    }
    let agent = AgentKind::first_available()?;
    Ok(agent.perform(task, context.cancellation)?.to_string())
}
