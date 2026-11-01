use crate::agent::AgentKind;
use crate::commands::Context;
use crate::error::Error;

pub fn run(words: &[String], context: &Context) -> Result<String, Error> {
    let [task] = words else {
        return Err(Error::NotATask(words.join(" ")));
    };
    let task = trimmed(task)?;
    if !task.contains(char::is_whitespace) {
        return Err(Error::NotATask(task.to_owned()));
    }
    let agent = AgentKind::first_available()?;
    Ok(agent.perform(task, context.cancellation)?.to_string())
}

pub(crate) fn trimmed(task: &str) -> Result<&str, Error> {
    let task = task.trim();
    if task.is_empty() {
        return Err(Error::EmptyTask);
    }
    Ok(task)
}
