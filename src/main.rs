use std::env;
use std::io::{self, BufRead, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};
use std::sync::LazyLock;

use clap::Parser;
use regex::Regex;
use serde::Serialize;

// ============================================================================
// Pre-compiled Static Regular Expressions (Zero Per-Call Overhead)
// ============================================================================

static RE_OSC8: LazyLock<Regex> = LazyLock::new(|| {
    // Matches OSC 8 hyperlinks: \x1b]8;params;uri\x07 or \x1b\\ ... visible text ... \x1b]8;;\x07 or \x1b\\
    Regex::new(r"\x1B\]8;[^;]*;([^\x07\x1B]+)(?:\x07|\x1B\\)(.*?)\x1B\]8;;(?:\x07|\x1B\\)").unwrap()
});

static RE_ANSI: LazyLock<Regex> = LazyLock::new(|| {
    // Strips standard CSI SGR sequences and remaining OSC sequences
    Regex::new(r"\x1B\[[0-9;?]*[ -/]*[@-~]|\x1B\].*?(?:\x07|\x1B\\)").unwrap()
});

static RE_MARKDOWN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[.*?\]\((.+)\)$").unwrap());

static RE_EDITOR_URI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:vscode|vscode-insiders|cursor|zed|windsurf)://file/?(.*)$").unwrap()
});

static RE_GIT_STATUS: LazyLock<Regex> = LazyLock::new(|| {
    // Matches standard porcelain (M, MM, ??, UU) and rename/copy score prefixes (R100, C095)
    Regex::new(r"^(?:[ MADRCU?!]{1,2}|[RC]\d{1,3})\s+(.*)$").unwrap()
});

static RE_DIFF_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:---|\+\+\+)\s+(?:[ab]/)?(.*)$").unwrap());

static RE_PY_TRACEBACK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^File "([^"]+)", line (\d+)(?:, in .*)?$"#).unwrap());

static RE_RUST_DIAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\s*\d*\s*\|\s*)?-->\s+(.+?):(\d+):(\d+)\s*$").unwrap());

static RE_V8_PAREN: LazyLock<Regex> = LazyLock::new(|| {
    // Greedy match inside outer parens safely supports Next.js/SvelteKit paths like (app)
    Regex::new(r"^at\s+.+?\s+\((.+):(\d+):(\d+)\)$").unwrap()
});

static RE_V8_BARE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^at\s+(?:async\s+)?(.+):(\d+):(\d+)$").unwrap());

static RE_FRAG_RANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?)#L?(\d+)(?:C(\d+))?-L?(\d+)(?:C(\d+))?$").unwrap());

static RE_FRAG_SINGLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?)#L?(\d+)(?:C(\d+))?$").unwrap());

static RE_PAREN_COORDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?)\((\d+)(?:[,:]\s*(\d+))?\)(?:\s*:.*)?$").unwrap());

static RE_COLON_FULL_RANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?):(\d+):(\d+)-(\d+)(?::(\d+))?(?::.*)?$").unwrap());

static RE_COLON_LINE_RANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?):(\d+)-(\d+)(?::.*)?$").unwrap());

static RE_COLON_STANDARD: LazyLock<Regex> = LazyLock::new(|| {
    // Supports trailing colon, compiler diagnostics, and unspaced ripgrep/grep previews
    Regex::new(r"^(.+?):(\d+)(?::(\d+))?(?::.*)?$").unwrap()
});

// ============================================================================
// Data Structures
// ============================================================================

pub type Coordinates = (Option<usize>, Option<usize>, Option<usize>, Option<usize>);

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct FileReference {
    pub file: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub end_line: Option<usize>,
    pub end_column: Option<usize>,
}

impl FileReference {
    /// Serializes the reference into a JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

// ============================================================================
// Normalization Engine
// ============================================================================

pub struct Normalizer {
    base_dir: PathBuf,
}

impl Normalizer {
    pub fn new(base_dir: Option<PathBuf>) -> Self {
        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let base = match base_dir {
            Some(b) if b.is_absolute() => b,
            Some(b) => cwd.join(b),
            None => cwd,
        };
        let base = Self::lexical_normalize(&base);
        Self { base_dir: base }
    }

    pub fn parse(&self, raw: &str) -> Option<FileReference> {
        // Stage 1: De-noising and Boundary Peeling
        let stripped_ansi = self.strip_ansi(raw);
        if stripped_ansi.trim().is_empty() {
            return None;
        }

        // Strip git status porcelain before trimming leading space (e.g., " M src/lib.rs")
        let pre_vcs = self.strip_git_status(&stripped_ansi);
        let unpeeled = self.peel_wrappers(pre_vcs.trim());
        if unpeeled.is_empty() {
            return None;
        }

        // Stage 2: Protocol and Link Unpacking
        let md_unpacked = self.unpack_markdown(&unpeeled);
        let (uri_unpacked, uri_coords) = self.unpack_uri(&md_unpacked);

        // Stage 3 (Part 1): Diff header stripping ("--- a/...", "+++ b/...")
        let diff_header_cleaned = self.strip_diff_headers(&uri_unpacked);

        // Stage 4: Coordinate and Diagnostic Extraction
        let (raw_path, extracted_coords) = self.extract_coordinates(&diff_header_cleaned);
        let coords = Self::merge_coordinates(uri_coords, extracted_coords);

        // Stage 3 (Part 2): Disambiguate bare "a/" or "b/" prefixes now that coordinates are stripped
        let vcs_resolved_path = self.disambiguate_diff_prefix(&raw_path);

        // Stage 5: Path Canonicalization
        let sanitized_path = self.canonicalize_path(&vcs_resolved_path)?;

        let (line, column, end_line, end_column) = coords.unwrap_or((None, None, None, None));

        Some(FileReference {
            file: sanitized_path,
            line,
            column,
            end_line,
            end_column,
        })
    }

    fn strip_ansi(&self, input: &str) -> String {
        // If the input is an OSC 8 hyperlink where the visible text has no coordinates
        // or the URI is a file:// link, promote or preserve cleanly.
        let osc_unpacked = RE_OSC8.replace_all(input, |caps: &regex::Captures| {
            let uri = &caps[1];
            let label = &caps[2];
            if label.contains(':') || label.contains('#') || label.contains('(') {
                label.to_string()
            } else if uri.starts_with("file://")
                || uri.starts_with("vscode://")
                || uri.starts_with("cursor://")
            {
                uri.to_string()
            } else {
                label.to_string()
            }
        });
        RE_ANSI.replace_all(&osc_unpacked, "").into_owned()
    }

    fn peel_wrappers(&self, input: &str) -> String {
        let mut s = input.trim().to_string();

        // Strip trailing sentence punctuation (, and ;)
        while s.ends_with(',') || s.ends_with(';') {
            s.pop();
            s = s.trim_end().to_string();
        }

        loop {
            let prev = s.clone();
            let bytes = s.as_bytes();

            if bytes.len() >= 2 {
                let first = bytes[0];
                let last = bytes[bytes.len() - 1];

                // Avoid peeling [...] if this is a Markdown link [label](target)
                let is_markdown_link = first == b'[' && last == b')' && s.contains("](");

                // Avoid peeling (...) if an inner '(' opens a coordinate suffix like "(foo.py(42))" vs "foo.py(42)"
                let matched_pair = !is_markdown_link
                    && ((first == b'"' && last == b'"')
                        || (first == b'\'' && last == b'\'')
                        || (first == b'`' && last == b'`')
                        || (first == b'(' && last == b')' && Self::is_outer_pair(&s, b'(', b')'))
                        || (first == b'[' && last == b']' && Self::is_outer_pair(&s, b'[', b']'))
                        || (first == b'{' && last == b'}' && Self::is_outer_pair(&s, b'{', b'}'))
                        || (first == b'<' && last == b'>'));

                if matched_pair {
                    s = s[1..s.len() - 1].trim().to_string();
                }
            }

            // Handle unmatched leading or trailing quotes from imprecise mouse selections
            for quote in ['"', '\'', '`'] {
                if s.starts_with(quote)
                    && !s.ends_with(quote)
                    && s.chars().filter(|&c| c == quote).count() == 1
                {
                    s = s[quote.len_utf8()..].trim_start().to_string();
                } else if s.ends_with(quote)
                    && !s.starts_with(quote)
                    && s.chars().filter(|&c| c == quote).count() == 1
                {
                    s.pop();
                    s = s.trim_end().to_string();
                }
            }

            // Re-strip trailing commas/semicolons that were inside quotes
            while s.ends_with(',') || s.ends_with(';') {
                s.pop();
                s = s.trim_end().to_string();
            }

            if s == prev {
                break;
            }
        }

        s
    }

    /// Verifies that the opening delimiter at index 0 actually closes at the very last byte,
    /// preventing `[label] (something) [id]` from stripping the first `[` and last `]`.
    fn is_outer_pair(s: &str, open: u8, close: u8) -> bool {
        let bytes = s.as_bytes();
        let mut depth = 0usize;
        for (i, &b) in bytes.iter().enumerate() {
            if b == open {
                depth += 1;
            } else if b == close {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i == bytes.len() - 1;
                }
            }
        }
        false
    }

    fn unpack_markdown(&self, input: &str) -> String {
        if let Some(caps) = RE_MARKDOWN.captures(input) {
            caps[1].trim().to_string()
        } else {
            input.to_string()
        }
    }

    fn unpack_uri(&self, input: &str) -> (String, Option<Coordinates>) {
        // 1. Handle editor schemes: vscode://file/path:line:col or cursor://file/path?line=10&column=5
        if let Some(caps) = RE_EDITOR_URI.captures(input) {
            let rest = &caps[1];
            let (path_part, query_coords) = Self::split_uri_query(rest);
            let decoded = Self::percent_decode(path_part);
            // Ensure leading slash for absolute Unix paths when `vscode://file/Users/...` is unpacked
            let normalized =
                if !decoded.starts_with('/') && !decoded.chars().nth(1).is_some_and(|c| c == ':') {
                    let candidate = format!("/{decoded}");
                    if Path::new(&candidate).exists()
                        || decoded.starts_with("Users/")
                        || decoded.starts_with("home/")
                        || decoded.starts_with("tmp/")
                    {
                        candidate
                    } else {
                        decoded
                    }
                } else {
                    decoded
                };
            return (normalized, query_coords);
        }

        // 2. Handle standard file:// URIs
        if let Some(stripped) = input.strip_prefix("file://") {
            let without_host = stripped.strip_prefix("localhost").unwrap_or(stripped);
            // Preserve #L42 fragment across percent-decoding
            if let Some((path_part, frag)) = without_host.split_once('#') {
                let decoded_path = Self::percent_decode(path_part);
                return (format!("{decoded_path}#{frag}"), None);
            }
            return (Self::percent_decode(without_host), None);
        }

        (input.to_string(), None)
    }

    fn split_uri_query(uri_tail: &str) -> (&str, Option<Coordinates>) {
        let Some((path, query)) = uri_tail.split_once('?') else {
            return (uri_tail, None);
        };

        let mut line = None;
        let mut col = None;
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                match k {
                    "line" | "l" => line = v.parse::<usize>().ok().filter(|&n| n >= 1),
                    "column" | "col" | "c" => col = v.parse::<usize>().ok().filter(|&n| n >= 1),
                    _ => {}
                }
            }
        }

        let coords = if line.is_some() || col.is_some() {
            Some((line, col, None, None))
        } else {
            None
        };

        (path, coords)
    }

    fn percent_decode(s: &str) -> String {
        percent_encoding::percent_decode_str(s)
            .decode_utf8_lossy()
            .into_owned()
    }

    fn strip_git_status(&self, input: &str) -> String {
        let trimmed_end = input.trim_end();
        // Do not misinterpret Python `File "..."` or V8 `at ...` as git status
        if let Some(caps) = RE_GIT_STATUS.captures(trimmed_end) {
            let rest = caps[1].trim();
            if let Some((_, dest)) = rest.split_once(" -> ") {
                return dest.trim().trim_matches('"').to_string();
            }
            return rest.to_string();
        }
        trimmed_end.to_string()
    }

    fn strip_diff_headers(&self, input: &str) -> String {
        if let Some((_, dest)) = input.split_once(" -> ") {
            return dest.trim().trim_matches('"').to_string();
        }
        if let Some(caps) = RE_DIFF_HEADER.captures(input) {
            return caps[1].to_string();
        }
        input.to_string()
    }

    fn disambiguate_diff_prefix(&self, path: &str) -> String {
        if let Some(downstream) = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")) {
            let prefix = &path[0..1];
            let physical_prefix_dir = self.base_dir.join(prefix);
            let full_candidate = self.base_dir.join(path);

            // Preserve `a/` or `b/` only if `a/<downstream>` physically exists, or if `a/`
            // exists AND its immediate parent structure for `<downstream>` exists inside `a/`
            // while `<downstream>` does not exist directly at the root.
            if full_candidate.exists() {
                return path.to_string();
            }
            if physical_prefix_dir.is_dir()
                && !self.base_dir.join(downstream).exists()
                && Path::new(downstream).parent().is_some_and(|parent| {
                    !parent.as_os_str().is_empty() && physical_prefix_dir.join(parent).is_dir()
                })
            {
                return path.to_string();
            }
            return downstream.to_string();
        }
        path.to_string()
    }

    fn extract_coordinates(&self, input: &str) -> (String, Option<Coordinates>) {
        // 1. Python traceback: File "/repo/src/main.py", line 42, in main
        if let Some(caps) = RE_PY_TRACEBACK.captures(input) {
            let file = caps[1].to_string();
            let l = Self::parse_nonzero(&caps[2]);
            return (file, Some((l, None, None, None)));
        }

        // 2. Rust compiler diagnostic: --> src/main.rs:42:7
        if let Some(caps) = RE_RUST_DIAG.captures(input) {
            let file = caps[1].to_string();
            let l = Self::parse_nonzero(&caps[2]);
            let c = Self::parse_nonzero(&caps[3]);
            return (file, Some((l, c, None, None)));
        }

        // 3. V8 / Node.js stack trace with parens: at run (/repo/src/routes/(app)/page.tsx:42:7)
        if let Some(caps) = RE_V8_PAREN.captures(input) {
            let file = caps[1].to_string();
            let l = Self::parse_nonzero(&caps[2]);
            let c = Self::parse_nonzero(&caps[3]);
            return (file, Some((l, c, None, None)));
        }

        // 4. V8 / Node.js bare frame: at /repo/src/main.js:42:7
        if let Some(caps) = RE_V8_BARE.captures(input) {
            let file = caps[1].to_string();
            let l = Self::parse_nonzero(&caps[2]);
            let c = Self::parse_nonzero(&caps[3]);
            return (file, Some((l, c, None, None)));
        }

        // 5. URI Fragment Range: src/main.py#L42C7-L58C12 or #L42-L58 or #42-58
        if let Some(caps) = RE_FRAG_RANGE.captures(input) {
            let file = caps[1].to_string();
            let l = Self::parse_nonzero(&caps[2]);
            let c = caps.get(3).and_then(|m| Self::parse_nonzero(m.as_str()));
            let el = Self::parse_nonzero(&caps[4]);
            let ec = caps.get(5).and_then(|m| Self::parse_nonzero(m.as_str()));
            return (file, Some((l, c, el, ec)));
        }

        // 6. URI Fragment Single: src/main.py#L42C7 or #L42 or #42
        if let Some(caps) = RE_FRAG_SINGLE.captures(input) {
            let file = caps[1].to_string();
            let l = Self::parse_nonzero(&caps[2]);
            let c = caps.get(3).and_then(|m| Self::parse_nonzero(m.as_str()));
            return (file, Some((l, c, None, None)));
        }

        // 7. Parenthetical coordinates: src/main.py(42, 7) or src/main.py(42:7) or src/main.py(42)
        if let Some(caps) = RE_PAREN_COORDS.captures(input) {
            let file = caps[1].to_string();
            let l = Self::parse_nonzero(&caps[2]);
            let c = caps.get(3).and_then(|m| Self::parse_nonzero(m.as_str()));
            return (file, Some((l, c, None, None)));
        }

        // 8. Colon Full Range: src/main.py:42:7-58:12
        if let Some(caps) = RE_COLON_FULL_RANGE.captures(input) {
            let file = caps[1].to_string();
            let l = Self::parse_nonzero(&caps[2]);
            let c = Self::parse_nonzero(&caps[3]);
            let el = Self::parse_nonzero(&caps[4]);
            let ec = caps.get(5).and_then(|m| Self::parse_nonzero(m.as_str()));
            return (file, Some((l, c, el, ec)));
        }

        // 9. Colon Line Range: src/main.py:42-58
        if let Some(caps) = RE_COLON_LINE_RANGE.captures(input) {
            let file = caps[1].to_string();
            let l = Self::parse_nonzero(&caps[2]);
            let el = Self::parse_nonzero(&caps[3]);
            return (file, Some((l, None, el, None)));
        }

        // 10. Standard Colon / Compiler Diagnostic / Ripgrep output: src/main.py:42:7: error...
        if let Some(caps) = RE_COLON_STANDARD.captures(input) {
            let file = caps[1].to_string();
            let l = Self::parse_nonzero(&caps[2]);
            let c = caps.get(3).and_then(|m| Self::parse_nonzero(m.as_str()));
            return (file, Some((l, c, None, None)));
        }

        (input.to_string(), None)
    }

    fn parse_nonzero(s: &str) -> Option<usize> {
        s.parse::<usize>().ok().filter(|&n| n >= 1)
    }

    fn merge_coordinates(
        primary: Option<Coordinates>,
        secondary: Option<Coordinates>,
    ) -> Option<Coordinates> {
        match (primary, secondary) {
            (Some(p), Some(s)) => Some((p.0.or(s.0), p.1.or(s.1), p.2.or(s.2), p.3.or(s.3))),
            (Some(p), None) => Some(p),
            (None, Some(s)) => Some(s),
            (None, None) => None,
        }
    }

    fn canonicalize_path(&self, raw: &str) -> Option<String> {
        let trimmed = raw.trim().trim_matches('"').trim_matches('\'');
        if trimmed.is_empty() {
            return None;
        }

        // Expand `~` or `~/` via $HOME
        let expanded = if trimmed == "~" {
            env::var("HOME").unwrap_or_else(|_| trimmed.to_string())
        } else if let Some(rest) = trimmed.strip_prefix("~/") {
            if let Ok(home) = env::var("HOME") {
                format!("{}/{rest}", home.trim_end_matches('/'))
            } else {
                trimmed.to_string()
            }
        } else {
            trimmed.to_string()
        };

        let candidate_path = if !Path::new(&expanded).is_absolute() {
            self.base_dir.join(&expanded)
        } else {
            PathBuf::from(&expanded)
        };

        let normalized = Self::lexical_normalize(&candidate_path);
        let final_str = normalized.to_string_lossy().into_owned();

        if final_str.is_empty() {
            return None;
        }

        Some(final_str)
    }

    /// Lexically resolves `.` and `..` without touching the filesystem (preserving
    /// non-existent diff targets and avoiding unwanted symlink resolution).
    fn lexical_normalize(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for comp in path.components() {
            match comp {
                Component::Prefix(p) => out.push(p.as_os_str()),
                Component::RootDir => out.push(comp.as_os_str()),
                Component::CurDir => {}
                Component::ParentDir => {
                    let can_pop = out
                        .components()
                        .next_back()
                        .is_some_and(|c| matches!(c, Component::Normal(_)));
                    if can_pop {
                        out.pop();
                    } else if !out.is_absolute() {
                        out.push("..");
                    }
                }
                Component::Normal(c) => out.push(c),
            }
        }
        if out.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            out
        }
    }
}

// ============================================================================
// Vector-Safe Process Dispatcher & Template Engine
// ============================================================================

pub struct Dispatcher;

impl Dispatcher {
    pub fn open(
        reference: &FileReference,
        editor: &str,
        template: Option<&str>,
    ) -> io::Result<ExitStatus> {
        if let Some(tpl) = template {
            return Self::open_with_template(reference, tpl);
        }

        let l = reference.line.unwrap_or(1);
        let c = reference.column.unwrap_or(1);

        let mut cmd = match editor {
            "nvim" | "neovim" | "vim" | "vi" => {
                let bin = if editor.starts_with('n') {
                    "nvim"
                } else {
                    "vim"
                };
                let mut cmd = Command::new(bin);
                if reference.line.is_some() || reference.column.is_some() {
                    cmd.arg(format!("+call cursor({l}, {c})"));
                }
                cmd.arg("--").arg(&reference.file);
                cmd
            }
            "cursor" | "code" | "vscode" => {
                let bin = if editor == "cursor" { "cursor" } else { "code" };
                let mut cmd = Command::new(bin);
                cmd.arg("-g")
                    .arg("--")
                    .arg(format!("{}:{l}:{c}", reference.file));
                cmd
            }
            "bat" => {
                let mut cmd = Command::new("bat");
                if let Some(el) = reference.end_line {
                    let start_ctx = l.saturating_sub(5).max(1);
                    let end_ctx = el.saturating_add(5);
                    cmd.arg("-H")
                        .arg(format!("{l}:{el}"))
                        .arg("-r")
                        .arg(format!("{start_ctx}:{end_ctx}"));
                } else if reference.line.is_some() {
                    let start_ctx = l.saturating_sub(5).max(1);
                    let end_ctx = l.saturating_add(5);
                    cmd.arg("-H")
                        .arg(l.to_string())
                        .arg("-r")
                        .arg(format!("{start_ctx}:{end_ctx}"));
                }
                cmd.arg("--").arg(&reference.file);
                cmd
            }
            "hx" | "helix" | "zed" | "subl" | "sublime" => {
                let bin = match editor {
                    "hx" | "helix" => "hx",
                    "zed" => "zed",
                    _ => "subl",
                };
                let mut cmd = Command::new(bin);
                cmd.arg("--").arg(format!("{}:{l}:{c}", reference.file));
                cmd
            }
            "nano" => {
                let mut cmd = Command::new("nano");
                if reference.line.is_some() || reference.column.is_some() {
                    cmd.arg(format!("+{l},{c}"));
                }
                cmd.arg("--").arg(&reference.file);
                cmd
            }
            "emacsclient" => {
                let mut cmd = Command::new("emacsclient");
                cmd.arg("-n");
                if reference.line.is_some() || reference.column.is_some() {
                    cmd.arg(format!("+{l}:{c}"));
                }
                cmd.arg("--").arg(&reference.file);
                cmd
            }
            custom => {
                let mut cmd = Command::new(custom);
                cmd.arg("--").arg(&reference.file);
                cmd
            }
        };

        cmd.status()
    }

    /// Vectorizes a user template without invoking `/bin/sh -c`, preventing shell injection.
    pub fn open_with_template(reference: &FileReference, template: &str) -> io::Result<ExitStatus> {
        let tokens = Self::tokenize_template(template);
        let Some((bin, raw_args)) = tokens.split_first() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Empty opener template",
            ));
        };

        let l = reference.line.unwrap_or(1).to_string();
        let c = reference.column.unwrap_or(1).to_string();
        let el = reference
            .end_line
            .map_or_else(String::new, |v| v.to_string());
        let ec = reference
            .end_column
            .map_or_else(String::new, |v| v.to_string());

        let mut cmd = Command::new(bin);
        for token in raw_args {
            let expanded = token
                .replace("{file}", &reference.file)
                .replace("{line}", &l)
                .replace("{column}", &c)
                .replace("{end_line}", &el)
                .replace("{end_column}", &ec);
            cmd.arg(expanded);
        }

        cmd.status()
    }

    fn tokenize_template(template: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut current = String::new();
        let mut in_single = false;
        let mut in_double = false;

        for ch in template.chars() {
            match ch {
                '\'' if !in_double => in_single = !in_single,
                '"' if !in_single => in_double = !in_double,
                c if c.is_whitespace() && !in_single && !in_double => {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                }
                c => current.push(c),
            }
        }
        if !current.is_empty() {
            tokens.push(current);
        }
        tokens
    }
}

// ============================================================================
// CLI Entrypoint
// ============================================================================

#[derive(Parser, Debug)]
#[command(
    name = "open-in-editor",
    about = "Deterministic terminal file reference opener and editor dispatcher"
)]
struct Cli {
    /// Base directory for resolving relative paths and git diff disambiguation
    #[arg(short = 'C', long = "base-dir")]
    base_dir: Option<PathBuf>,

    /// Emit structured JSON output instead of launching an editor
    #[arg(long)]
    json: bool,

    /// Target editor or viewer to launch (nvim, cursor, code, bat, hx, zed, subl, nano, emacsclient)
    #[arg(short, long, env = "EDITOR", default_value = "nvim")]
    editor: String,

    /// Custom argument vector template (e.g. "nvim '+call cursor({line},{column})' -- {file}")
    #[arg(long = "open-with", env = "FILE_OPENER_TEMPLATE")]
    open_with: Option<String>,

    /// Raw input strings (reads from stdin if omitted)
    inputs: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let normalizer = Normalizer::new(cli.base_dir.clone());

    let lines: Vec<String> = if cli.inputs.is_empty() {
        io::stdin().lock().lines().map_while(Result::ok).collect()
    } else {
        cli.inputs.clone()
    };

    let mut matched_any = false;
    let mut stdout = io::stdout().lock();

    for line in lines {
        let Some(reference) = normalizer.parse(&line) else {
            continue;
        };
        matched_any = true;

        if cli.json {
            if let Ok(json) = reference.to_json() {
                let _ = writeln!(stdout, "{json}");
            }
        } else {
            match Dispatcher::open(&reference, &cli.editor, cli.open_with.as_deref()) {
                Ok(status) if status.success() => {}
                Ok(status) => {
                    return ExitCode::from(status.code().unwrap_or(1) as u8);
                }
                Err(err) => {
                    eprintln!("open-in-editor: failed to execute editor: {err}");
                    return ExitCode::FAILURE;
                }
            }
        }
    }

    if matched_any {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// ============================================================================
// Unit Test Suite (Verifying All Specification Grammars)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn norm() -> Normalizer {
        Normalizer::new(Some(PathBuf::from("/work")))
    }

    #[test]
    fn test_ansi_and_wrappers() {
        let n = norm();
        let input = "\x1b[38;5;2m\"'src/my file.py:42:7'\",;\x1b[0m";
        assert_eq!(
            n.parse(input),
            Some(FileReference {
                file: "/work/src/my file.py".into(),
                line: Some(42),
                column: Some(7),
                end_line: None,
                end_column: None,
            })
        );
    }

    #[test]
    fn test_unbalanced_triple_click_quote() {
        let n = norm();
        assert_eq!(
            n.parse("\"src/main.rs:42:7"),
            Some(FileReference {
                file: "/work/src/main.rs".into(),
                line: Some(42),
                column: Some(7),
                end_line: None,
                end_column: None,
            })
        );
    }

    #[test]
    fn test_markdown_and_uris() {
        let n = norm();
        assert_eq!(
            n.parse("[Main File](file://localhost/tmp/a%20dir/caf%C3%A9.py#L42C7-L58C12)"),
            Some(FileReference {
                file: "/tmp/a dir/café.py".into(),
                line: Some(42),
                column: Some(7),
                end_line: Some(58),
                end_column: Some(12),
            })
        );

        assert_eq!(
            n.parse("cursor://file/Users/dev/repo/src/lib.rs:104:12"),
            Some(FileReference {
                file: "/Users/dev/repo/src/lib.rs".into(),
                line: Some(104),
                column: Some(12),
                end_line: None,
                end_column: None,
            })
        );
    }

    #[test]
    fn test_git_status_and_diff_prefixes() {
        let n = norm();
        // Leading space in git status porcelain (" M ")
        assert_eq!(
            n.parse(" M src/main.py:12").unwrap().file,
            "/work/src/main.py"
        );
        // Rename with spaces and score
        assert_eq!(
            n.parse("R100 \"src/old name.py\" -> \"src/new name.py\"")
                .unwrap()
                .file,
            "/work/src/new name.py"
        );
        // Diff header and bare a/ prefix with coordinates
        assert_eq!(
            n.parse("+++ b/src/components/Button.tsx").unwrap().file,
            "/work/src/components/Button.tsx"
        );
        assert_eq!(
            n.parse("a/src/main.rs:42:7").unwrap(),
            FileReference {
                file: "/work/src/main.rs".into(),
                line: Some(42),
                column: Some(7),
                end_line: None,
                end_column: None,
            }
        );
    }

    #[test]
    fn test_ripgrep_and_compiler_diagnostics() {
        let n = norm();
        // Unspaced ripgrep output
        assert_eq!(
            n.parse("src/main.rs:42:7:fn main() {"),
            Some(FileReference {
                file: "/work/src/main.rs".into(),
                line: Some(42),
                column: Some(7),
                end_line: None,
                end_column: None,
            })
        );
        // Rust compiler arrow
        assert_eq!(
            n.parse("  --> src/main.rs:42:7"),
            Some(FileReference {
                file: "/work/src/main.rs".into(),
                line: Some(42),
                column: Some(7),
                end_line: None,
                end_column: None,
            })
        );
    }

    #[test]
    fn test_stack_traces_with_framework_route_groups() {
        let n = norm();
        // V8 stack trace containing Next.js/SvelteKit (app) and [id] route segments
        assert_eq!(
            n.parse("    at load (/repo/src/routes/(app)/[id]/page.tsx:42:7)"),
            Some(FileReference {
                file: "/repo/src/routes/(app)/[id]/page.tsx".into(),
                line: Some(42),
                column: Some(7),
                end_line: None,
                end_column: None,
            })
        );
        // Python traceback
        assert_eq!(
            n.parse("  File \"/repo/src/main.py\", line 42, in main"),
            Some(FileReference {
                file: "/repo/src/main.py".into(),
                line: Some(42),
                column: None,
                end_line: None,
                end_column: None,
            })
        );
    }

    #[test]
    fn test_leading_dash_filename() {
        let n = norm();
        assert_eq!(
            n.parse("-unusual-filename.py:15:3"),
            Some(FileReference {
                file: "/work/-unusual-filename.py".into(),
                line: Some(15),
                column: Some(3),
                end_line: None,
                end_column: None,
            })
        );
    }

    #[test]
    fn test_cli_parsing_flag_after_input() {
        let cli = Cli::try_parse_from(["open-in-editor", "src/a.rs:1", "--json"]).unwrap();
        assert!(cli.json);
        assert_eq!(cli.inputs, vec!["src/a.rs:1"]);
    }

    #[test]
    fn test_cli_parsing_leading_dash_input_with_separator() {
        let cli = Cli::try_parse_from(["open-in-editor", "--json", "--", "--- a/foo.rs"]).unwrap();
        assert!(cli.json);
        assert_eq!(cli.inputs, vec!["--- a/foo.rs"]);

        let cli2 = Cli::try_parse_from(["open-in-editor", "--", "-unusual.py:10"]).unwrap();
        assert_eq!(cli2.inputs, vec!["-unusual.py:10"]);
    }

    #[test]
    fn test_cli_parsing_leading_whitespace_input() {
        let cli = Cli::try_parse_from(["open-in-editor", "  --> src/main.rs:42:7"]).unwrap();
        assert_eq!(cli.inputs, vec!["  --> src/main.rs:42:7"]);
    }

    #[test]
    fn test_base_dir_git_disambiguation() {
        let temp_dir = std::env::temp_dir().join(format!("oie_test_{}", std::process::id()));
        let real_a_dir = temp_dir.join("a").join("nested");
        let _ = std::fs::create_dir_all(&real_a_dir);

        let n = Normalizer::new(Some(temp_dir.clone()));

        // Case 1: `a/nested/file.rs` exists under real `a/`, so `a/` is preserved as a directory
        let parsed = n.parse("a/nested/file.rs:10").unwrap();
        assert_eq!(
            parsed.file,
            temp_dir.join("a/nested/file.rs").to_string_lossy()
        );

        // Case 2: `a/other/file.rs` does not exist under `a/`, so `a/` is treated as a diff prefix and stripped
        let parsed2 = n.parse("a/other/file.rs:10").unwrap();
        assert_eq!(
            parsed2.file,
            temp_dir.join("other/file.rs").to_string_lossy()
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
