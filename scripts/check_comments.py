import ast
import io
import json
import os
import re
import subprocess
import sys
import tokenize
from functools import cache
from pathlib import Path

import tree_sitter_bash
import tree_sitter_rust
import tree_sitter_toml
import tree_sitter_yaml
import yaml
from tree_sitter import Language, Parser, Query, QueryCursor

from comment_workflows import scripts, shell_source, uses_lines


LANGUAGES = {
    "bash": tree_sitter_bash,
    "rust": tree_sitter_rust,
    "toml": tree_sitter_toml,
    "yaml": tree_sitter_yaml,
}
EXTENSIONS = {
    ".rs": "rust", ".py": "python", ".sh": "bash", ".bash": "bash",
    ".ps1": "pwsh", ".psm1": "pwsh", ".psd1": "pwsh",
    ".toml": "toml", ".yml": "yaml", ".yaml": "yaml",
}
SHELLS = {"bash": "bash", "sh": "bash", "pwsh": "pwsh", "powershell": "pwsh",
          "python": "python", "python3": "python"}
SHEBANG = re.compile(r"#!\s*(?:/(?:usr/)?bin/(?:env[ \t]+)?)(bash|sh|pwsh|python3?)")
PIN = re.compile(r"\s*(?:-\s+)?uses:\s*['\"]?[\w.-]+/[\w./-]+@[a-fA-F0-9]{40}['\"]?\s+")
TAG = re.compile(r"#\s+(?:stable|v\d+(?:\.\d+){0,2})")
REMEDY = "Remove narration or clarify the code. Use 'Why: <reason>' only for an essential constraint."


class ParseFailure(Exception):
    def __init__(self, line, message):
        super().__init__(message)
        self.line = line


@cache
def grammar(name):
    language = Language(LANGUAGES[name].language())
    names = ["line_comment", "block_comment"] if name == "rust" else ["comment"]
    pattern = "\n".join(f"({kind}) @comment" for kind in names)
    if name == "rust":
        pattern += '\n((attribute (identifier) @name value: (_)) @doc (#eq? @name "doc"))'
    query = Query(language, pattern)
    return Parser(language), query


def public_rustdoc(node, path):
    attribute = node.type in {"inner_attribute_item", "attribute_item"}
    if path.parts[:1] != ("src",) or (not attribute and node.child_by_field_name("doc") is None):
        return False
    if node.type == "inner_attribute_item" or node.child_by_field_name("inner") is not None:
        if node.parent.type == "source_file":
            return path == Path("src/lib.rs")
        item = node.parent.parent
        if not item or item.type != "mod_item":
            return False
    else:
        item = node.next_named_sibling
        while item and item.type in {"line_comment", "block_comment", "attribute_item"}:
            item = item.next_named_sibling
    if item is None:
        return False

    def public(n):
        return any(c.type == "visibility_modifier" and c.text == b"pub" for c in n.named_children)

    parents = []
    ancestor = item.parent
    while ancestor is not None:
        if ancestor.type in {"block", "function_item"}:
            return False
        if ancestor.type == "impl_item":
            target = ancestor.child_by_field_name("type")
            if target and target.type == "generic_type":
                target = target.child_by_field_name("type")
            if not target or not any(
                public(peer) and (name := peer.child_by_field_name("name"))
                and name.text == target.text for peer in ancestor.parent.named_children
            ):
                return False
        if ancestor.type in {"mod_item", "trait_item", "struct_item", "enum_item"}:
            parents.append(ancestor)
        ancestor = ancestor.parent
    implicit = any(n.type in {"trait_item", "enum_item"} for n in parents)
    return (public(item) or implicit) and all(public(n) for n in parents)


def tree_comments(source, language, path):
    parser, query = grammar(language)
    root = parser.parse(source.encode()).root_node
    if root.has_error:
        node = root
        while node.type != "ERROR":
            child = next((c for c in node.children if c.has_error or c.is_missing), None)
            if child is None:
                break
            node = child
        raise ParseFailure(node.start_point.row + 1, f"cannot parse {language}; check syntax here")
    captures = QueryCursor(query).captures(root)
    nodes = [*captures.get("comment", []), *(a.parent for a in captures.get("doc", []))]
    for node in sorted(nodes, key=lambda n: n.start_byte):
        if language != "rust" or not public_rustdoc(node, path):
            yield node.start_point.row + 1, node.text.decode().rstrip("\r\n")


def powershell_comments(source):
    try:
        result = subprocess.run(
            ["pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", "-File",
             str(Path(__file__).with_name("comment_tokens.ps1").resolve())],
            input=source, encoding="utf-8", capture_output=True, timeout=30,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        if result.returncode:
            raise ParseFailure(1, "PowerShell tokenization failed; check that pwsh 7 is available")
        report = json.loads(result.stdout)
    except (OSError, subprocess.TimeoutExpired, ValueError) as exc:
        raise ParseFailure(1, f"cannot inspect PowerShell; check pwsh 7 ({exc})") from exc
    if report["errors"]:
        first = report["errors"][0]
        raise ParseFailure(first["line"], f"cannot parse PowerShell: {first['message']}")
    return [(item["line"], item["text"]) for item in report["comments"]]


def comments(source, language, path):
    if language == "pwsh":
        return powershell_comments(source)
    if language == "python":
        try:
            ast.parse(source)
            tokens = tokenize.generate_tokens(io.StringIO(source).readline)
            return [(t.start[0], t.string) for t in tokens if t.type == tokenize.COMMENT]
        except (tokenize.TokenError, SyntaxError) as exc:
            line = exc.args[1][0] if isinstance(exc, tokenize.TokenError) else exc.lineno
            raise ParseFailure(line or 1, f"cannot tokenize Python: {exc.args[0]}") from exc
    return list(tree_comments(source, language, path))


def allowed(comment, line, source_line, language, pinned_action):
    if line == 1 and language in {"bash", "pwsh", "python"} and source_line == comment and SHEBANG.fullmatch(comment):
        return True
    if language == "pwsh" and source_line.strip() == comment and re.fullmatch(r"#requires\s+-Version\s+\d+(?:\.\d+)*", comment, re.I):
        return True
    if language == "python" and line <= 2 and source_line.strip() == comment and re.fullmatch(
        r"#(?:\s*-\*-\s*)?\s*coding[:=]\s*[-\w.]+(?:\s*-\*-)?", comment
    ):
        return True
    if pinned_action and TAG.fullmatch(comment):
        return bool(PIN.fullmatch(source_line.removesuffix(comment)))
    for opening, closing in [("/*", "*/"), ("<#", "#>"), ("//", ""), ("#", "")]:
        if comment.startswith(opening):
            body = comment[len(opening):].removesuffix(closing).strip()
            reason = re.fullmatch(r"Why: ([^\r\n]+)", body)
            return bool(reason and any(c.isalpha() for c in reason[1]) and (not closing or opening not in body)
                        and not re.match(r"(?:TODO|FIXME|TBD|placeholder)\b", reason[1], re.I))
    return False


def inspect_source(source, language, path, workflow=False):
    lines = source.splitlines()
    pins = uses_lines(source) if workflow else set()
    return [(line, text) for line, text in comments(source, language, path)
            if not allowed(text, line, lines[line - 1], language, line in pins)]


def inspect_workflow(source, path):
    for step in scripts(source):
        code = shell_source(step.scalar.value)
        possible = []
        errors = []
        for shell in step.shells:
            if shell not in SHELLS:
                yield step.source_line(1, ""), f"unsupported shell {shell!r}; add a parser for this run step"
                break
            try:
                possible.append(inspect_source(code, SHELLS[shell], path))
            except ParseFailure as exc:
                errors.append(exc)
        else:
            if not possible:
                error = errors[0]
                yield step.source_line(error.line, ""), str(error)
            elif errors and "#" in code:
                yield step.source_line(1, ""), "set shell explicitly to inspect this run step on a variable runner"
            elif len(possible) == 2 and set(possible[0]) != set(possible[1]):
                yield step.source_line(1, ""), "set shell explicitly; Bash and PowerShell interpret this step's comments differently"
            else:
                for line, text in possible[0]:
                    yield step.source_line(line, text), REMEDY


def inspect_file(path):
    if (path.parts[:2] == ("tests", "fixtures") or path.parts[:1] == ("docs",)
            or path.as_posix() == "pipetrain.example.toml"):
        return []
    language = EXTENSIONS.get(path.suffix.lower())
    if not language and path.suffix:
        return []
    with path.open("rb") as stream:
        if not language:
            first = stream.readline(256)
            if not first.startswith(b"#!"):
                return []
            interpreter = SHEBANG.fullmatch(first.decode().strip())
            if not interpreter:
                return [(1, "unknown interpreter; add support for this script")]
            language = SHELLS[interpreter[1]]
        stream.seek(0)
        data = stream.read()
    if language == "python":
        encoding, _ = tokenize.detect_encoding(io.BytesIO(data).readline)
    else:
        encoding = "utf-8-sig"
    source = data.decode(encoding).replace("\r\n", "\n")
    workflow = language == "yaml" and (
        path.name in {"action.yml", "action.yaml"} or path.parent.as_posix() == ".github/workflows"
    )
    findings = [(line, REMEDY) for line, _ in inspect_source(source, language, path, workflow)]
    if workflow:
        findings.extend(inspect_workflow(source, path))
    return findings


def main():
    try:
        top = subprocess.check_output(["git", "rev-parse", "--show-toplevel"])
        os.chdir(os.fsdecode(top.rstrip(b"\r\n")))
        tracked = set(subprocess.check_output(["git", "ls-files", "-z"]).split(b"\0"))
        deleted = set(subprocess.check_output(["git", "ls-files", "--deleted", "-z"]).split(b"\0"))
    except (OSError, subprocess.CalledProcessError) as exc:
        print(f"cannot list tracked files; run inside the Git checkout: {exc}", file=sys.stderr)
        return 1
    failed = False
    for name in sorted(tracked - deleted - {b""}):
        path = Path(os.fsdecode(name))
        try:
            findings = inspect_file(path)
        except ParseFailure as exc:
            findings = [(exc.line, str(exc))]
        except yaml.MarkedYAMLError as exc:
            findings = [(exc.problem_mark.line + 1, f"cannot read workflow: {exc.problem}")]
        except (OSError, UnicodeError, SyntaxError, LookupError) as exc:
            findings = [(1, f"cannot inspect source: {exc}")]
        for line, message in sorted(findings):
            failed = True
            print(f"{path.as_posix()}:{line}: {message}", file=sys.stderr)
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
