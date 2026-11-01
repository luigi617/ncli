use super::Change;

const SCRATCH: &str = "Your current directory is an empty scratch directory: write any test, script, or \
                       checkout there, and do not modify the repository or the change, and do not post \
                       comments.";

pub fn review(change: &Change) -> String {
    format!(
        "Review {change}. {} {SCRATCH}\n\
         \n\
         Look for real problems: bugs, security issues, and design flaws. Report a change that is \
         not needed for what the change sets out to do, or code that would be clearly simpler \
         written another way, only when the improvement is worth making; do not report style \
         preferences or nitpicks. If the change is good, say so and report nothing; never invent a \
         finding to have something to say.\n\
         \n\
         Reproduce every bug before reporting it, by running the code or a test or script that shows \
         the wrong behavior, and report it only if it reproduces, with the steps and what happened. \
         Report each finding with file, line, severity, and reasoning. If you cannot access the \
         change, say so and stop.",
        change.access()
    )
}

pub fn rebuttal(change: &Change, own: &str, others: &str) -> String {
    format!(
        "You reviewed {change}. Other reviewers reviewed it independently. {} {SCRATCH}\n\
         \n\
         Your findings:\n\
         {own}\n\
         \n\
         The other reviewers' findings:\n\
         {others}\n\
         \n\
         Verify each of the other reviewers' findings against the change, reproducing each bug \
         yourself before agreeing with it, and state whether you agree or dispute it, with \
         reasoning. Dispute a bug that does not reproduce and a finding that is only a preference. \
         Revise your own findings where you are convinced, and add anything everyone missed, \
         reproduced the same way.",
        change.access()
    )
}

pub fn judge(change: &Change, debate: &str) -> String {
    format!(
        "You are judging a code review debate about {change}. Each reviewer reviewed it \
         independently, then responded to the others. {} {SCRATCH}\n\
         \n\
         {debate}\n\
         \n\
         Verify the disputed points against the change yourself, reproducing a bug when the \
         reviewers disagree on whether it reproduces, and report only the findings that survive, \
         as JSON matching the given schema. Keep a bug only if it was reproduced. Keep an \
         unnecessary change or a simplification only if it is clearly worth making, and drop \
         preferences and nitpicks. No findings is the right verdict for a good change. For each \
         finding, give the file and the most relevant line, a short title, a one or two sentence \
         explanation of what is wrong, how a bug was reproduced, and why it matters, and a one \
         sentence fix. Set change_accessed to false, with no findings, if you or any reviewer could \
         not access the change.",
        change.access()
    )
}
