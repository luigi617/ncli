# ncli

Define workflows that chain other CLIs: one named command runs an ordered sequence of external CLI invocations.

## Development

Requires [uv](https://docs.astral.sh/uv/).

```sh
uv sync                 # create .venv and install ncli with dev tools
uv run ncli --version   # run the CLI
uv run pytest           # run tests
uv run ruff check .     # lint
uv run ruff format .    # format
```
