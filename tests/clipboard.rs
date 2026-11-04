#![cfg(windows)]

mod common;

use std::fs;
use std::process::Command;

use common::{ncli, terminal_state};
use tempfile::TempDir;

#[test]
fn copy_puts_text_on_the_windows_clipboard_intact() {
    let state = TempDir::new().unwrap();
    for text in [
        "ok",
        "## ncli code review: 1 finding\n\n### 🔴 High: héllo wörld ✓",
    ] {
        fs::write(terminal_state(state.path()).join("output.txt"), text).unwrap();
        let output = ncli(state.path()).arg("copy").output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let pasted = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-Clipboard -Raw",
            ])
            .output()
            .unwrap();
        assert!(pasted.status.success(), "{pasted:?}");
        assert_eq!(
            String::from_utf8(pasted.stdout)
                .unwrap()
                .trim_end()
                .replace("\r\n", "\n"),
            text
        );
    }
}
