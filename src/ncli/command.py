from __future__ import annotations

import argparse
from abc import ABC, abstractmethod
from collections.abc import Sequence
from dataclasses import dataclass
from typing import ClassVar, TypeAlias


@dataclass(frozen=True)
class Argument:
    name: str
    help: str

    def add_to(self, parser: argparse.ArgumentParser) -> None:
        parser.add_argument(self.name, help=self.help)


@dataclass(frozen=True)
class Flag:
    name: str
    help: str
    choices: Sequence[str] | None = None
    default: str | None = None

    def add_to(self, parser: argparse.ArgumentParser) -> None:
        help = self.help if self.default is None else f"{self.help} (default: %(default)s)"
        parser.add_argument(self.name, help=help, choices=self.choices, default=self.default)


class Command(ABC):
    name: ClassVar[str]
    help: ClassVar[str]
    arguments: Sequence[Argument] = ()
    flags: Sequence[Flag] = ()
    required_executables: Sequence[str] = ()

    @abstractmethod
    def run(self, args: argparse.Namespace) -> int: ...


@dataclass(frozen=True)
class CommandGroup:
    name: str
    help: str
    subcommands: Sequence[CommandNode]


CommandNode: TypeAlias = Command | CommandGroup
