import argparse
from abc import ABC, abstractmethod
from collections.abc import Iterable
from typing import ClassVar


class Command(ABC):
    name: ClassVar[str]
    help: ClassVar[str]

    def configure(self, parser: argparse.ArgumentParser) -> None:  # noqa: B027
        pass

    def required_executables(self, args: argparse.Namespace) -> Iterable[str]:
        return ()

    @abstractmethod
    def run(self, args: argparse.Namespace) -> int: ...
