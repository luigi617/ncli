from collections.abc import Callable
from pathlib import Path

import pytest

from ncli.executables import (
    MissingExecutablesError,
    executable_exists,
    missing_executables,
    require_executables,
)


def test_executable_exists_only_for_installed_names(path_with: Callable[..., None]) -> None:
    path_with("claude")
    assert executable_exists("claude")
    assert not executable_exists("codex")


def test_missing_executables_keeps_declared_order_without_duplicates(
    path_with: Callable[..., None],
) -> None:
    path_with("claude")
    assert missing_executables(["gemini", "claude", "codex", "gemini"]) == ["gemini", "codex"]


def test_non_executable_file_counts_as_missing(
    path_with: Callable[..., None], tmp_path: Path
) -> None:
    path_with()
    (tmp_path / "claude").write_text("")
    assert missing_executables(["claude"]) == ["claude"]


def test_require_executables_reports_every_missing_name(path_with: Callable[..., None]) -> None:
    path_with("claude")
    with pytest.raises(MissingExecutablesError) as exc:
        require_executables(["claude", "codex", "gemini"])
    assert str(exc.value) == "required executables not found on PATH: codex, gemini"


def test_require_executables_passes_when_all_installed(path_with: Callable[..., None]) -> None:
    path_with("claude", "codex")
    require_executables(["claude", "codex"])
