use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::Duration;
use std::{env, fs, thread};

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
        ["sleep", seconds] => sleep(seconds),
        ["spawn-and-wait", seconds] => {
            Command::new(env::current_exe().unwrap())
                .args(["sleep", seconds])
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
