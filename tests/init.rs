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

#[cfg(unix)]
#[test]
fn a_new_state_directory_is_private_and_an_existing_one_is_left_alone() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let mode = |path: &std::path::Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let init = |state: &std::path::Path| {
        assert!(
            ncli(state)
                .args(["init", "zsh"])
                .output()
                .unwrap()
                .status
                .success()
        );
    };
    let new = dir.path().join("new");
    init(&new);
    assert_eq!(mode(&new), 0o700);
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    init(dir.path());
    assert_eq!(mode(dir.path()), 0o755);
}
