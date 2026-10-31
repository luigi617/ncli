from ncli.command import CommandNode
from ncli.commands.review.command import ReviewCommand

COMMANDS: tuple[CommandNode, ...] = (ReviewCommand(),)
