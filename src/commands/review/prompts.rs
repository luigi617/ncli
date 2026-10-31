pub fn review(link: &str) -> String {
    format!(
        "Review the code change at {link}. Fetch it yourself; for a GitHub PR use `gh pr view` and \
         `gh pr diff`. Do not modify anything and do not post comments.\n\
         Report concrete findings (bugs, correctness, security, design), each with file, line, \
         severity, and reasoning. If you cannot access the change, say so and stop."
    )
}

pub fn rebuttal(link: &str, own: &str, others: &str) -> String {
    format!(
        "You reviewed the code change at {link}. Other reviewers reviewed it independently.\n\
         \n\
         Your findings:\n\
         {own}\n\
         \n\
         The other reviewers' findings:\n\
         {others}\n\
         \n\
         Verify each of the other reviewers' findings against the change and state whether you agree \
         or dispute it, with reasoning. Revise your own findings where you are convinced, and add \
         anything everyone missed. Do not modify anything and do not post comments."
    )
}

pub fn judge(link: &str, debate: &str) -> String {
    format!(
        "You are judging a code review debate about the change at {link}. Each reviewer reviewed it \
         independently, then responded to the others.\n\
         \n\
         {debate}\n\
         \n\
         Verify the disputed points against the change yourself. Produce the final findings that \
         survive the debate, ranked by severity, each with file, line, and justification. Then list \
         the findings you rejected and why. Do not modify anything and do not post comments."
    )
}
