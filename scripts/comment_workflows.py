import re
from dataclasses import dataclass
from functools import lru_cache

import yaml
from yaml.nodes import MappingNode, ScalarNode, SequenceNode


def fields(node):
    if not isinstance(node, MappingNode):
        return {}
    return {k.value: v for k, v in node.value if isinstance(k, ScalarNode)}


def default_shell(owner):
    return fields(fields(owner.get("defaults")).get("run")).get("shell")


@dataclass
class Script:
    scalar: ScalarNode
    shells: tuple
    lines: list

    def source_line(self, line, text):
        start = self.scalar.start_mark.line
        if self.scalar.style == "|":
            return start + line + 1
        if self.scalar.end_mark.line > start and text:
            first = text.splitlines()[0]
            for index in range(start, min(len(self.lines), self.scalar.end_mark.line + 1)):
                if first in self.lines[index]:
                    return index + 1
        return start + 1


@lru_cache(maxsize=1)
def owners(source):
    doc = fields(yaml.compose(source, Loader=yaml.BaseLoader))
    owners = list(fields(doc.get("jobs")).values())
    if "runs" in doc:
        owners.append(doc["runs"])
    return default_shell(doc), [fields(owner) for owner in owners]


def workflow_steps(job):
    steps = job.get("steps")
    return [fields(e) for e in steps.value] if isinstance(steps, SequenceNode) else []


def uses_lines(source):
    return {
        entry["uses"].start_mark.line + 1
        for job in owners(source)[1]
        for entry in [job, *workflow_steps(job)]
        if isinstance(entry.get("uses"), ScalarNode)
    }


def scripts(source):
    general_shell, jobs = owners(source)
    lines = source.splitlines()
    for job in jobs:
        inherited = default_shell(job) or general_shell
        runner = job.get("runs-on")
        for step in workflow_steps(job):
            run = step.get("run")
            if not isinstance(run, ScalarNode):
                continue
            shell = step.get("shell", inherited)
            if shell is not None:
                shells = (shell.value.split()[0].lower(),) if shell.value.strip() else ("",)
            elif isinstance(runner, ScalarNode) and "windows" in runner.value.lower():
                shells = ("pwsh",)
            elif runner is None or (
                isinstance(runner, ScalarNode) and runner.value.startswith(("ubuntu-", "macos-"))
            ):
                shells = ("bash",)
            else:
                shells = ("bash", "pwsh")
            yield Script(run, shells, lines)


def shell_source(script):
    expression = r"\$\{\{(?:[^'}]|'(?:[^']|'')*'|}(?!}))*}}"
    return re.sub(expression, lambda m: re.sub(r"[^\r\n]", "x", m[0]), script)
