# `open-in-editor`

A command-line utility that parses messy terminal output (compiler diagnostics, stack traces, `ripgrep` lines, `git status` entries, unified diff headers, and Markdown or file URIs) and either prints JSON coordinates or opens the resolved file in an editor or terminal viewer.

It uses pre-compiled `LazyLock` regular expressions for sub-2 ms cold start and spawns editors directly via `execve` without shell evaluation.

## Features

The parser runs a five-stage pipeline:

1. De-noising and boundary peeling: strips 7-bit and 8-bit ANSI SGR color sequences, unpacks OSC 8 terminal hyperlinks, trims balanced wrapper delimiters (`"..."`, `'...'`, `` `...` ``, `(...)`, `[...]`, `{...}`, `<...>`), recovers from unbalanced triple-click mouse selections, and strips trailing `,` and `;` punctuation.
2. Protocol and link unpacking: extracts targets from inline Markdown links (`[label](target)`), `file://` URIs (with RFC 3986 percent-decoding), and editor URI schemes (`vscode://`, `cursor://`, `zed://`, `windsurf://`).
3. Version control disambiguation: strips `git status` porcelain prefixes (`M`, `MM`, `??`, `R100 old -> new`) and unified diff headers (`--- a/`, `+++ b/`). Checks the filesystem before stripping bare `a/` and `b/` prefixes so real top-level `a/` or `b/` directories stay intact.
4. Coordinate and stack trace extraction: extracts `line`, `column`, `end_line`, and `end_column` across 10 formats including `ripgrep`, Rust `-->` diagnostics, Python tracebacks, and V8/Node.js stack frames (preserving Next.js and SvelteKit route groups like `src/routes/(app)/[id]/page.tsx`).
5. Lexical path canonicalization: expands `~/` via `$HOME`, resolves relative paths against `--base-dir` (or `$PWD`) into absolute paths, and resolves `.` and `..` components without requiring the target file to exist on disk.

Editors launch via OS argument vectors (`argv`) with `--` option terminators, never passing unsanitized strings through `/bin/sh -c` or `eval`.

## Supported input formats

Relative paths resolve against `--base-dir` (or `$PWD`, shown here as `/repo`) and output as absolute paths:

| Grammar / Source | Raw Input Example | Resolved Path | Line | Col | End Line | End Col |
| ----- | ----- | ----- | ----- | ----- | ----- | ----- |
| **Standard / Ripgrep** | `src/main.rs:42:7:fn main() {` | `/repo/src/main.rs` | `42` | `7` | `null` | `null` |
| **Compiler Diagnostic** | `src/main.py:42:7: error: syntax` | `/repo/src/main.py` | `42` | `7` | `null` | `null` |
| **Rust `rustc` Arrow** | `  --> src/main.rs:42:7` | `/repo/src/main.rs` | `42` | `7` | `null` | `null` |
| **Python Traceback** | `File "/app/main.py", line 42, in run` | `/app/main.py` | `42` | `null` | `null` | `null` |
| **V8 / Node.js Trace** | `at load (/app/(grp)/[id]/page.tsx:42:7)` | `/app/(grp)/[id]/page.tsx` | `42` | `7` | `null` | `null` |
| **Colon Range** | `src/main.py:42-58` | `/repo/src/main.py` | `42` | `null` | `58` | `null` |
| **Parenthetical** | `src/main.py(42, 7)` or `src/main.py(42:7)` | `/repo/src/main.py` | `42` | `7` | `null` | `null` |
| **URI Fragment Range** | `src/main.py#L42C7-L58C12` | `/repo/src/main.py` | `42` | `7` | `58` | `12` |
| **Markdown + File URI** | `[Doc](file://localhost/tmp/a%20b.py#L10)` | `/tmp/a b.py` | `10` | `null` | `null` | `null` |
| **Git Status / Rename** | `R100 "src/old.py" -> "src/new.py"` | `/repo/src/new.py` | `null` | `null` | `null` | `null` |
| **Git Diff Header** | `+++ b/src/components/Button.tsx` | `/repo/src/components/Button.tsx` | `null` | `null` | `null` | `null` |

## Installation

### Using mise (recommended)

Install globally with `mise`:

```bash
mise use -g github:iloveitaly/open-in-editor
```

### Pre-built binaries

Download binaries for Linux (`x86_64`, `aarch64` musl), macOS (Apple Silicon, Intel), and Windows (`x86_64`, `aarch64`) from [GitHub Releases](https://github.com/iloveitaly/open-in-editor/releases).

### From source

Build and install locally with Cargo:

```bash
cargo build --release
cp target/release/open-in-editor ~/.local/bin/
```

## CLI usage

```
Deterministic terminal file reference opener and editor dispatcher

Usage: open-in-editor [OPTIONS] [INPUTS]...

Arguments:
  [INPUTS]...  Raw input strings (reads from stdin if omitted)

Options:
  -C, --base-dir <BASE_DIR>    Base directory for resolving relative paths and git diff disambiguation
      --json                   Emit structured JSON output instead of launching an editor
  -e, --editor <EDITOR>        Target editor or viewer to launch (nvim, cursor, code, bat, cat, hx, zed, subl, nano, emacsclient) [env: EDITOR=nvim] [default: nvim]
      --open-with <OPEN_WITH>  Custom argument vector template (e.g. "nvim '+call cursor({line},{column})' -- {file}") [env: FILE_OPENER_TEMPLATE=]
  -h, --help                   Print help
```

### 1. Direct editor dispatch

Pass terminal output as positional arguments or on standard input:

```
# Open a Rust compiler error directly in Neovim at line 42, column 7
open-in-editor --editor nvim "  --> src/main.rs:42:7"

# Pipe clipboard contents into Cursor
pbpaste | open-in-editor --editor cursor

# Preview a line range in bat with +/- 5 lines of surrounding context
open-in-editor --editor bat "file:///Users/dev/project/src/lib.rs#L42-L58"
```

Flags can appear anywhere on the command line (for example, `open-in-editor src/main.rs:4 --json`). When an input starts with a dash, such as diff headers (`--- a/foo.rs`) or filenames (`-unusual.py`), put `--` before it so the parser treats it as an input rather than a flag:

```
# Pass leading-dash inputs safely using the -- separator
open-in-editor --json -- "--- a/foo.rs"
```

Inputs passed on stdin or starting with whitespace (`"  --> src/main.rs:42:7"`) do not need `--`.

Supported built-in `--editor` targets:

* `nvim` / `neovim`: `nvim "+call cursor(42, 7)" -- <file>`

* `vim` / `vi`: `vim "+call cursor(42, 7)" -- <file>`

* `cursor`: `cursor -g -- <file>:42:7`

* `code` / `vscode`: `code -g -- <file>:42:7`

* `bat`: `bat -H 42:58 -r 37:63 -- <file>`

* `cat`: `cat -- <file>` (prints the whole file)

* `hx` / `helix`: `hx -- <file>:42:7`

* `zed`: `zed -- <file>:42:7`

* `subl` / `sublime`: `subl -- <file>:42:7`

* `nano`: `nano +42,7 -- <file>`

* `emacsclient`: `emacsclient -n +42:7 -- <file>`

### 2. Custom opener templates (`--open-with`)

For other editors or GUI wrappers, set a template with `--open-with` or `FILE_OPENER_TEMPLATE`. The template splits into an `argv` array without invoking a shell:

```
open-in-editor \
  --open-with "wezterm cli spawn -- nvim '+call cursor({line},{column})' -- {file}" \
  "src/main.rs:42:7: error: borrow check failed"
```

Supported placeholders: `{file}`, `{line}`, `{column}`, `{end_line}`, `{end_column}`.

### 3. Structured JSON output (`--json`)

Relative paths resolve against `--base-dir` (or `$PWD`) and output as absolute paths:

```
$ open-in-editor --json "[Main](file://localhost/tmp/my%20file.py#L42C7-L58C12)"
{"file":"/tmp/my file.py","line":42,"column":7,"end_line":58,"end_column":12}

$ open-in-editor -C /repo --json "src/main.rs:42:7"
{"file":"/repo/src/main.rs","line":42,"column":7,"end_line":null,"end_column":null}
```

## Workflow integrations

### Tmux (`~/.tmux.conf`)

Bind `Enter` in `copy-mode-vi` to open a selected path or diagnostic in your editor:

```
# Pipe copy-mode selection directly into open-in-editor
bind-key -T copy-mode-vi Enter send-keys -X copy-pipe-and-cancel \
  "open-in-editor --editor cursor"

# Open system clipboard reference in Neovim inside a popup pane
bind-key C-o display-popup -E -w 85% -h 85% \
  "pbpaste | open-in-editor --editor nvim"
```

### Interactive `ripgrep` and `fzf` navigation

Search with `ripgrep` and `fzf` and open the selected match directly:

```
rg --color=always --line-number --column --no-heading --smart-case "" | \
  fzf --ansi \
      --preview 'open-in-editor --editor bat {}' \
      --bind 'enter:become(open-in-editor --editor nvim {})' \
      --bind 'ctrl-o:become(open-in-editor --editor cursor {})' \
      --bind 'ctrl-v:become(open-in-editor --editor code {})'
```

## Releasing

Releases use [Release Please](https://github.com/googleapis/release-please) and Conventional Commits:

1. Write commits using [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `feat!:`, `chore:`).
2. Pushes to `master` update a release PR with the changelog and next semver version.
3. Merging that PR creates the GitHub release and tag, triggering binary builds for Linux, macOS, and Windows.
4. Run `just github_setup` once on a new repository to grant Actions write and PR-approval permissions and set up force-push protection on `master`.
