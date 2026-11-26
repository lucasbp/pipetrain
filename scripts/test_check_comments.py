import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from check_comments import ParseFailure, inspect_source, inspect_workflow


class SourceTests(unittest.TestCase):
    def lines(self, source, language, filename="src/lib.rs"):
        return [n for n, _ in inspect_source(source, language, Path(filename))]

    def test_rust_literals_nested_comments_and_macro_tokens(self):
        source = '''fn main() {
    let html = r###"https://host/#x // /* payload */"###;
    println!("{}", "/// text");
    /* outer /* nested */ end */
    println!(/* annotate */ "{}", html);
    // Why: A reader can still hold this revision.
}'''
        self.assertEqual(self.lines(source, "rust"), [4, 5])

    def test_public_docs_and_private_narration(self):
        source = '''//! The library.
/// Field values are in bytes.
#[derive(Debug)]
pub struct Packet { pub size: u32 }
/// Internal history.
pub(crate) fn inspect() {}
struct Logs;
impl Logs {
    /// Internal method.
    pub fn trace() {}
}
mod internal {
    /// Hidden here.
    pub fn local() {}
}'''
        self.assertEqual(self.lines(source, "rust"), [5, 9, 13])
        self.assertEqual(self.lines("/// text\npub fn fixture() {}", "rust", "tests/common.rs"), [1])
        attributes = '''#[doc = "Public API contract."]
pub struct Interface;
#[doc = "Private narration."]
fn helper() {}
#[doc(hidden)]
pub fn hidden() {}'''
        self.assertEqual(self.lines(attributes, "rust"), [3])

    def test_bash_and_python_inline_data(self):
        bash = """#!/bin/sh
cat <<'END'
# literal fixture
END
echo "ref#42" "${x#ref}" # narration
echo done # Why: The caller retains the descriptor.
"""
        self.assertEqual(self.lines(bash, "bash"), [5])
        python = '''# -*- coding: utf-8 -*-
fixture = """# SAMPLE
// fixture, not code
"""
url = "https://demo.test/#label" # stale
'''
        self.assertEqual(self.lines(python, "python"), [5])

    def test_powershell_comments_and_here_strings(self):
        source = """#requires -Version 5.1
$body = @'
# fixture
<##>
'@
<# hidden #>
"$($x # nested comment
)"
"""
        self.assertEqual(self.lines(source, "pwsh"), [6, 7])

    def test_config_literals_and_the_why_exception(self):
        src = '''url = "https://host/#data" # stale
test = """
# sample data
"""
# Why:
# Why: TODO
# Why: Readers may still reference the old version.
'''
        self.assertEqual(self.lines(src, "toml"), [1, 5, 6])
        self.assertEqual(self.lines("/* Why: valid\nextra prose */ fn main() {}", "rust"), [1])

    def test_syntax_errors_have_locations(self):
        for source, language in [('fn main( {', 'rust'), ('def broken(:', 'python'), ("'unclosed", 'pwsh')]:
            with self.subTest(language=language), self.assertRaises(ParseFailure) as failure:
                self.lines(source, language)
            self.assertGreaterEqual(failure.exception.line, 1)


class WorkflowTests(unittest.TestCase):
    def test_version_pin_requires_a_uses_sha(self):
        sha = "f" * 40
        source = f"""runs:
 steps:
  - uses: acme/example@{sha} # v1.2.3
  - uses: acme/example@{sha} # stable
  - uses: acme/example@v4 # v4
    name: item # stable
  - uses: acme/example@{sha} # v4 explanation
    env:
      uses: acme/example@{sha} # v4
"""
        self.assertEqual([line for line, _ in inspect_source(
            source, "yaml", Path(".github/workflows/test.yml"), True
        )], [5, 6, 7, 9])

    def test_actual_run_steps_and_action_expressions(self):
        source = """jobs:
  test:
    runs-on: ubuntu-latest
    defaults: {run: {shell: bash}}
    env:
      run: |
        # data
    steps:
      - run: |
          echo "${{ format('}} # data', matrix.os) }}"
          echo yes # drift
      - shell: pwsh
        run: |
          $file = @'
          # fixture
          '@
          Get-Item . # drift
"""
        self.assertEqual([n for n, _ in inspect_workflow(source, Path("ci.yml"))], [11, 17])

    def test_composite_action_and_folded_run(self):
        source = """runs:
  using: composite
  steps:
    - shell: sh
      run: >
        echo 'this is a URL'
        # stale
"""
        self.assertEqual([n for n, _ in inspect_workflow(source, Path("action.yml"))], [7])

    def test_unsupported_shell_is_actionable(self):
        source = "runs:\n  steps:\n    - shell: custom\n      run: echo hi\n"
        findings = list(inspect_workflow(source, Path("action.yml")))
        self.assertEqual(findings[0][0], 4)
        self.assertIn("unsupported shell", findings[0][1])


class EntryPointTests(unittest.TestCase):
    def test_tracked_helpers_nested_cwd_and_unreadable_files(self):
        script = Path(__file__).with_name("check_comments.py").resolve()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(["git", "init", "--quiet"], cwd=root, check=True)
            helpers = root / "tests" / "helpers"
            helpers.mkdir(parents=True)
            fixture = root / "tests" / "fixtures" / "train-2026-10-28" / "config.toml"
            fixture.parent.mkdir(parents=True)
            fixture.write_text("# documented fixture", encoding="utf-8")
            (fixture.parent / "sample.rs").write_text("// sample input", encoding="utf-8")
            docs = root / "docs"
            docs.mkdir()
            (docs / "demo.sh").write_text("# explanation", encoding="utf-8")
            (helpers / "test.py").write_text('print("# literal")\n# stale\n', encoding="utf-8")
            subprocess.run(["git", "add", "tests", "docs"], cwd=root, check=True)
            (root / "local.py").write_text("# untracked", encoding="utf-8")

            def run():
                return subprocess.run([sys.executable, "-B", str(script)], cwd=helpers,
                                      capture_output=True, text=True, timeout=20)

            found = run()
            self.assertEqual(found.returncode, 1)
            self.assertIn("tests/helpers/test.py:2:", found.stderr)
            self.assertNotIn("config.toml", found.stderr)
            self.assertNotIn("sample.rs", found.stderr)
            self.assertNotIn("demo.sh", found.stderr)
            self.assertNotIn("local.py", found.stderr)
            (helpers / "test.py").write_text('print("# literal")', encoding="utf-8")
            self.assertEqual(run().returncode, 0)
            (helpers / "test.py").unlink()
            self.assertEqual(run().returncode, 0)
            (helpers / "test.py").write_bytes(b"\xff\xff")
            self.assertIn("cannot inspect source", run().stderr)


if __name__ == "__main__":
    unittest.main()
