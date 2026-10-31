from collections.abc import Callable
from pathlib import Path

import pytest

from ncli.utils import IS_WINDOWS


@pytest.fixture
def path_with(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Callable[..., None]:
    def install(*names: str) -> None:
        for name in names:
            if IS_WINDOWS:
                (tmp_path / f"{name}.cmd").write_text("@echo off\n")
                continue
            executable = tmp_path / name
            executable.write_text("#!/bin/sh\n")
            executable.chmod(0o755)
        monkeypatch.setenv("PATH", str(tmp_path))

    return install
