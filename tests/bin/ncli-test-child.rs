use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::Duration;
use std::{env, fs, thread};

const OUTLIVE: Duration = Duration::from_secs(2);

fn sleep(seconds: &str) {
    thread::sleep(Duration::from_secs(seconds.parse().unwrap()));
}

fn answer_verdict(dir: &str, verdict: &str, schema: &str) {
    let mut prompt = String::new();
    io::stdin().read_to_string(&mut prompt).unwrap();
    fs::write(Path::new(dir).join("judge.prompt"), prompt).unwrap();
    fs::write(Path::new(dir).join("judge.schema"), schema).unwrap();
    print!("{verdict}");
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args[..] {
        ["describe", arg] => {
            let mut input = String::new();
            io::stdin().read_to_string(&mut input).unwrap();
            let cwd = env::current_dir().unwrap();
            print!("{arg} {input} {}", cwd.display());
        }
        ["cat"] => {
            io::copy(&mut io::stdin(), &mut io::stdout()).unwrap();
        }
        ["write", stdout] => {
            io::stdout().write_all(stdout.as_bytes()).unwrap();
        }
        ["fail", code, ref lines @ ..] => {
            for line in lines {
                eprintln!("{line}");
            }
            return ExitCode::from(code.parse::<u8>().unwrap());
        }
        ["review", dir, name] => {
            let mut prompt = String::new();
            io::stdin().read_to_string(&mut prompt).unwrap();
            let prefix = format!("{name}.prompt.");
            let asked = fs::read_dir(dir)
                .unwrap()
                .filter(|entry| {
                    let entry = entry.as_ref().unwrap();
                    entry.file_name().to_string_lossy().starts_with(&prefix)
                })
                .count();
            fs::write(Path::new(dir).join(format!("{prefix}{asked}")), prompt).unwrap();
            println!("{name} answer {asked}");
        }
        ["cwd"] => print!("{}", env::current_dir().unwrap().display()),
        ["bytes"] => {
            let mut input = Vec::new();
            io::stdin().read_to_end(&mut input).unwrap();
            print!("{input:?}");
        }
        ["outlive", dir] => {
            fs::write(Path::new(dir).join("started"), "").unwrap();
            thread::sleep(OUTLIVE);
            fs::write(Path::new(dir).join("survived"), "").unwrap();
        }
        ["spawn-and-exit", dir] => {
            #[expect(
                clippy::zombie_processes,
                reason = "the grandchild must outlive this process"
            )]
            Command::new(env::current_exe().unwrap())
                .args(["outlive", dir])
                .spawn()
                .unwrap();
        }
        ["write-until-closed", dir] => {
            for _ in 0..100 {
                if writeln!(io::stdout(), "x")
                    .and_then(|()| io::stdout().flush())
                    .is_err()
                {
                    fs::write(Path::new(dir).join("closed"), "").unwrap();
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
        ["escape", dir] => {
            let mut grandchild = Command::new(env::current_exe().unwrap());
            grandchild.args(["write-until-closed", dir]);
            #[cfg(unix)]
            std::os::unix::process::CommandExt::process_group(&mut grandchild, 0);
            #[expect(
                clippy::zombie_processes,
                reason = "the grandchild must outlive this process"
            )]
            grandchild.spawn().unwrap();
            print!("done");
        }
        ["verdict", dir, verdict, "--schema", schema] => answer_verdict(dir, verdict, schema),
        ["verdict", dir, verdict, "--schema-file", path] => {
            answer_verdict(dir, verdict, &fs::read_to_string(path).unwrap());
        }
        ["perform", answer, "--session", _, "--schema", _] => print!("{answer}"),
        ["perform", answer, "--json", "--schema", _] => {
            let message = |text: &str| serde_json::json!({"type": "item.completed", "item": {"type": "agent_message", "text": text}});
            println!(
                "{}",
                serde_json::json!({"type": "thread.started", "thread_id": "thread-1"})
            );
            println!("{}", message("Working on it."));
            println!(
                "{}",
                serde_json::json!({"type": "item.completed", "item": {"type": "reasoning"}})
            );
            println!("{}", message(answer));
            println!("{}", serde_json::json!({"type": "turn.completed"}));
        }
        ["perform-anonymously", answer, "--json", "--schema", _] => println!(
            "{}",
            serde_json::json!({"type": "item.completed", "item": {"type": "agent_message", "text": answer}})
        ),
        ["sleep", seconds] => sleep(seconds),
        #[cfg(unix)]
        ["session"] => {
            let leader = unsafe { libc::getsid(0) == libc::getpid() };
            print!(
                "{}",
                if leader {
                    "session leader"
                } else {
                    "session member"
                }
            );
        }
        ["spawn-and-wait", dir] => {
            Command::new(env::current_exe().unwrap())
                .args(["outlive", dir])
                .status()
                .unwrap();
        }
        ["flaky", counter, failures] => {
            let counter = Path::new(counter);
            let attempt: u32 =
                fs::read_to_string(counter).map_or(0, |count| count.parse().unwrap());
            fs::write(counter, (attempt + 1).to_string()).unwrap();
            if attempt < failures.parse().unwrap() {
                eprintln!("transient");
                return ExitCode::FAILURE;
            }
            println!("ok");
        }
        _ => panic!("unknown child invocation: {args:?}"),
    }
    ExitCode::SUCCESS
}
