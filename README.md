# `open-in-editor`

A high-performance, deterministic command-line utility that ingests noisy terminal strings—compiler diagnostics, stack traces, `ripgrep` lines, `git status` matrices, unified diff headers, and Markdown/file URIs—and either emits structured coordinates (`JSON` or `NUL`-delimited streams) or opens the resolved file directly in your editor or terminal viewer.

Designed for sub-$2\text{ ms}$ cold-start latency using pre-compiled `LazyLock` regular expressions and zero-shell-evaluation (`execve`) process vectorization.

## Key Features

* **5-Stage Deterministic Normalization Pipeline:**

  1. **De-noising & Boundary Peeling:** Strips 7-bit/8-bit ANSI SGR color sequences, unpacks OSC 8 terminal hyperlinks, trims balanced wrapper delimiters (`"..."`, `'...'`, `` `...` ``, `(...)`, `[...]`, `{...}`, `<...>`), recovers from unbalanced triple-click mouse selections, and strips trailing sentence punctuation (`,` and `;`).

  2. **Protocol & Link Unpacking:** Extracts targets from inline Markdown links (`[label](target)`), `file://` URIs (with RFC 3986 percent-decoding), and editor URI schemes (`vscode://`, `cursor://`, `zed://`, `windsurf://`).

  3. **Version Control Disambiguation:** Strips `git status` porcelain prefixes (`M`, `MM`, `??`, `R100 old -> new`) and unified diff headers (`--- a/`, `+++ b/`). Disambiguates bare `a/` and `b/` prefixes via filesystem introspection so real top-level `a/` or `b/` directories are never accidentally stripped.

  4. **Coordinate & Stack Trace Extraction:** Extracts `line`, `column`, `end_line`, and `end_column` across 10 distinct grammars including `ripgrep`, Rust `-->` diagnostics, Python tracebacks, and V8/Node.js stack frames (preserving Next.js/SvelteKit route groups like `src/routes/(app)/[id]/page.tsx`).

  5. **Lexical Path Canonicalization:** Expands `~/` via `$HOME`, resolves `.` and `..` components without requiring the target file to exist on disk, and guards leading-hyphen filenames (`-unusual.py` $\rightarrow$ `./-unusual.py`) against CLI flag misinterpretation.

* **Shell-Injection-Safe Dispatch:** Spawns editors directly via OS argument vectors (`argv`) with `--` option terminators—never passing unsanitized strings through `/bin/sh -c` or `eval`.

## Supported Input Formats

| Grammar / Source | Raw Input Example | Resolved Path | Line | Col | End Line | End Col | 
| ----- | ----- | ----- | ----- | ----- | ----- | ----- | 
| **Standard / Ripgrep** | `src/main.rs:42:7:fn main() {` | `src/main.rs` | `42` | `7` | `null` | `null` | 
| **Compiler Diagnostic** | `src/main.py:42:7: error: syntax` | `src/main.py` | `42` | `7` | `null` | `null` | 
| **Rust `rustc` Arrow** | `  --> src/main.rs:42:7` | `src/main.rs` | `42` | `7` | `null` | `null` | 
| **Python Traceback** | `File "/app/main.py", line 42, in run` | `/app/main.py` | `42` | `null` | `null` | `null` | 
| **V8 / Node.js Trace** | `at load (/app/(grp)/[id]/page.tsx:42:7)` | `/app/(grp)/[id]/page.tsx` | `42` | `7` | `null` | `null` | 
| **Colon Range** | `src/main.py:42-58` | `src/main.py` | `42` | `null` | `58` | `null` | 
| **Parenthetical** | `src/main.py(42, 7)` or `src/main.py(42:7)` | `src/main.py` | `42` | `7` | `null` | `null` | 
| **URI Fragment Range** | `src/main.py#L42C7-L58C12` | `src/main.py` | `42` | `7` | `58` | `12` | 
| **Markdown + File URI** | `[Doc](file://localhost/tmp/a%20b.py#L10)` | `/tmp/a b.py` | `10` | `null` | `null` | `null` | 
| **Git Status / Rename** | `R100 "src/old.py" -> "src/new.py"` | `src/new.py` | `null` | `null` | `null` | `null` | 
| **Git Diff Header** | `+++ b/src/components/Button.tsx` | `src/components/Button.tsx` | `null` | `null` | `null` | `null` | 

## Installation

Build and install the static native binary using Cargo:

```
cargo build --release
cp target/release/open-in-editor ~/.local/bin/
```

Run the unit test suite to verify all grammar extractors:

```
cargo test
```

## CLI Usage

```
Usage: open-in-editor [OPTIONS] [INPUTS]...

Arguments:
  [INPUTS]...  Raw input strings (reads from stdin if omitted)

Options:
  -C, --base-dir <BASE_DIR>    Base directory for resolving relative paths and git diff disambiguation
      --anchor-relative        Anchor relative paths to --base-dir (or $PWD)
      --json                   Emit structured JSON output instead of launching an editor [alias: --parse-only]
      --nul                    Emit NUL-delimited fields (<file>\0<line>\0<col>\0<end_line>\0<end_col>\0) [alias: --print0]
  -e, --editor <EDITOR>        Target editor or viewer to launch (nvim, cursor, code, bat, hx, zed, subl, nano, emacsclient) [env: EDITOR=] [default: nvim]
      --open-with <OPEN_WITH>  Custom argument vector template (e.g. "nvim '+call cursor({line},{column})' -- {file}") [env: FILE_OPENER_TEMPLATE=]
  -h, --help                   Print help
```

### 1. Direct Editor Dispatch

Pass messy terminal text as positional arguments or via standard input:

```
# Open a Rust compiler error directly in Neovim at line 42, column 7
open-in-editor --editor nvim "  --> src/main.rs:42:7"

# Pipe clipboard contents into Cursor
pbpaste | open-in-editor --editor cursor

# Preview a line range in bat with +/- 5 lines of surrounding context
open-in-editor --editor bat "file:///Users/dev/project/src/lib.rs#L42-L58"
```

Supported built-in `--editor` targets:

* `nvim` / `neovim`: `nvim "+call cursor(42, 7)" -- <file>`

* `vim` / `vi`: `vim "+call cursor(42, 7)" -- <file>`

* `cursor`: `cursor -g -- <file>:42:7`

* `code` / `vscode`: `code -g -- <file>:42:7`

* `bat`: `bat -H 42:58 -r 37:63 -- <file>`

* `hx` / `helix`: `hx -- <file>:42:7`

* `zed`: `zed -- <file>:42:7`

* `subl` / `sublime`: `subl -- <file>:42:7`

* `nano`: `nano +42,7 -- <file>`

* `emacsclient`: `emacsclient -n +42:7 -- <file>`

### 2. Custom Opener Templates (`--open-with`)

For custom editors or GUI wrappers, specify a template via `--open-with` or `FILE_OPENER_TEMPLATE`. Templates are tokenized into an argument vector (`argv`) without invoking a shell:

```
open-in-editor \
  --open-with "wezterm cli spawn -- nvim '+call cursor({line},{column})' -- {file}" \
  "src/main.rs:42:7: error: borrow check failed"
```

Supported placeholders: `{file}`, `{line}`, `{column}`, `{end_line}`, `{end_column}`.

### 3. Structured JSON Output (`--json`)

```
$ open-in-editor --json "[Main](file://localhost/tmp/my%20file.py#L42C7-L58C12)"
{"file":"/tmp/my file.py","line":42,"column":7,"end_line":58,"end_column":12}
```

### 4. Dependency-Free Shell Ingestion (`--nul` / `--print0`)

To consume parsed fields in POSIX/Bash/Zsh scripts without spawning `jq` or risking whitespace word-splitting:

```
open-in-editor --nul "src/my file.py:42:7: error" | {
  IFS= read -r -d '' file
  IFS= read -r -d '' line
  IFS= read -r -d '' col
  IFS= read -r -d '' end_line
  IFS= read -r -d '' end_col
  printf "File: [%s] Line: [%s] Col: [%s]\n" "$file" "$line" "$col"
}
```

## Workflow Integrations

### Tmux (`~/.tmux.conf`)

Bind `Enter` in `copy-mode-vi` to open any highlighted path/diagnostic directly in your editor:

```
# Pipe copy-mode selection directly into open-in-editor
bind-key -T copy-mode-vi Enter send-keys -X copy-pipe-and-cancel \
  "open-in-editor --editor cursor"

# Open system clipboard reference in Neovim inside a popup pane
bind-key C-o display-popup -E -w 85% -h 85% \
  "pbpaste | open-in-editor --editor nvim"
```

### Interactive `ripgrep` + `fzf` Navigation

Decouple fuzzy searching from editor dispatch while handling filenames with spaces and special characters safely:

```
rg --color=always --line-number --column --no-heading --smart-case "" | \
  fzf --ansi \
      --preview 'open-in-editor --editor bat {}' \
      --bind 'enter:become(open-in-editor --editor nvim {})' \
      --bind 'ctrl-o:become(open-in-editor --editor cursor {})' \
      --bind 'ctrl-v:become(open-in-editor --editor code {})'
```
