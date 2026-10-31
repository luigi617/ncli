use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::Duration;
use std::{env, fs, thread};

const OUTLIVE: Duration = Duration::from_secs(2);

fn sleep(seconds: &str) {
    thread::sleep(Duration::from_secs(seconds.parse().unwrap()));
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
        ["escape", seconds] => {
            let mut grandchild = Command::new(env::current_exe().unwrap());
            grandchild.args(["sleep", seconds]);
            #[cfg(unix)]
            std::os::unix::process::CommandExt::process_group(&mut grandchild, 0);
            #[expect(
                clippy::zombie_processes,
                reason = "the sleeper must outlive this process"
            )]
            grandchild.spawn().unwrap();
            print!("done");
        }
        ["sleep", seconds] => sleep(seconds),
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
