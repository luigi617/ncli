use super::Change;

pub fn review(change: &Change) -> String {
    format!(
        "Review {change}. {} Do not modify anything and do not post comments.\n\
         Report concrete findings (bugs, correctness, security, design), each with file, line, \
         severity, and reasoning. If you cannot access the change, say so and stop.",
        change.access()
    )
}

pub fn rebuttal(change: &Change, own: &str, others: &str) -> String {
    format!(
        "You reviewed {change}. Other reviewers reviewed it independently. {}\n\
         \n\
         Your findings:\n\
         {own}\n\
         \n\
         The other reviewers' findings:\n\
         {others}\n\
         \n\
         Verify each of the other reviewers' findings against the change and state whether you agree \
         or dispute it, with reasoning. Revise your own findings where you are convinced, and add \
         anything everyone missed. Do not modify anything and do not post comments.",
        change.access()
    )
}

pub fn judge(change: &Change, debate: &str) -> String {
    format!(
        "You are judging a code review debate about {change}. Each reviewer reviewed it \
         independently, then responded to the others. {}\n\
         \n\
         {debate}\n\
         \n\
         Verify the disputed points against the change yourself and report only the findings that \
         survive, as JSON matching the given schema. For each finding, give the file and the most \
         relevant line, a short title, a one or two sentence explanation of what is wrong and why it \
         matters, and a one sentence fix. Report no findings if none survive. Set change_accessed to \
         false, with no findings, if you or any reviewer could not access the change. Do not modify \
         anything and do not post comments.",
        change.access()
    )
}
