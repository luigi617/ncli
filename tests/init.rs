mod common;

use std::fs;

use common::ncli;
use tempfile::TempDir;

#[test]
fn install_adds_the_hook_to_the_zsh_startup_file_once() {
    let dir = TempDir::new().unwrap();
    let zshrc = dir.path().join(".zshrc");
    fs::write(&zshrc, "export EDITOR=vim").unwrap();
    let install = || {
        let output = ncli(&dir.path().join("state"))
            .args(["init", "zsh", "--install"])
            .env("ZDOTDIR", dir.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    assert!(install().starts_with(r#"Added eval "$(ncli init zsh)" to "#));
    assert!(install().ends_with(" already runs ncli\n"));
    assert_eq!(
        fs::read_to_string(&zshrc).unwrap(),
        "export EDITOR=vim\neval \"$(ncli init zsh)\"\n"
    );
}
