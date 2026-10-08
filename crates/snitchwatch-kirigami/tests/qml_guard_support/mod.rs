//! Source-level helpers shared by the honest-UI guard files: comment
//! stripping, block finding and `text:` binding checks for QML written in
//! this shell's flat one-property-per-line style.
//!
//! Lives in a subdirectory so cargo compiles it as a module of each guard
//! file that declares `mod qml_guard_support;`, not as a test of its own.
#![allow(dead_code)]

/// Drop whole-line `//` comments so a guard can't trip over prose that merely
/// names the thing it forbids (same helper shape as `qml_source_guards.rs`).
pub fn code_lines(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `<open> ... }` body in `source` (e.g. open = `Controls.Label {`),
/// found by brace counting (braces inside double-quoted strings are ignored).
/// Good enough for the flat, one-property-per-line style these pages are
/// written in.
pub fn blocks(source: &str, open: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find(open) {
        let body_start = start + open.len();
        let mut depth = 1usize;
        let mut in_string = false;
        let mut end = rest.len();
        for (i, ch) in rest[body_start..].char_indices() {
            match ch {
                '"' => in_string = !in_string,
                '{' if !in_string => depth += 1,
                '}' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        end = body_start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        found.push(rest[body_start..end].to_string());
        rest = &rest[end..];
    }
    found
}

/// The block's own `text:` binding: the `text:` line at the block's top
/// property indent (so a nested child's `text:` — an action inside `actions:`,
/// say — is never picked up) plus any continuation lines indented deeper than
/// it (how multi-line ternaries / `+` chains are written here), so a later
/// `color:` line can't be mistaken for part of the text.
pub fn text_binding(block: &str) -> Option<String> {
    let lines: Vec<&str> = block.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let base = indent(lines.iter().find(|l| !l.trim().is_empty())?);
    let start = lines
        .iter()
        .position(|l| indent(l) == base && l.trim_start().starts_with("text:"))?;
    let mut binding = lines[start].to_string();
    for line in &lines[start + 1..] {
        if line.trim().is_empty() || indent(line) <= base {
            break;
        }
        binding.push('\n');
        binding.push_str(line);
    }
    Some(binding)
}

/// Whether a `text:` binding is nothing but double-quoted string literals
/// joined by `+` — no identifiers, property reads, calls or ternaries, so it
/// cannot carry data.
pub fn is_fixed_text(binding: &str) -> bool {
    let Some(expr) = binding.trim_start().strip_prefix("text:") else {
        return false;
    };
    let mut chars = expr.chars();
    let mut literals = 0;
    while let Some(ch) = chars.next() {
        match ch {
            c if c.is_whitespace() || c == '+' => {}
            '"' => {
                literals += 1;
                loop {
                    match chars.next() {
                        Some('\\') => {
                            chars.next();
                        }
                        Some('"') => break,
                        Some(_) => {}
                        None => return false,
                    }
                }
            }
            _ => return false,
        }
    }
    literals > 0
}

/// Every `Controls.Label` whose `text:` binding mentions one of `data_exprs`
/// (daemon / subscription / remote-derived strings) must set
/// `textFormat: Text.PlainText`. `min_checked` stops the guard passing
/// vacuously if the matcher drifts from the page.
pub fn assert_data_labels_plain_text(
    page_name: &str,
    source: &str,
    data_exprs: &[&str],
    min_checked: usize,
) {
    let code = code_lines(source);
    let mut checked = 0;
    for block in blocks(&code, "Controls.Label {") {
        let Some(binding) = text_binding(&block) else {
            continue;
        };
        if !data_exprs.iter().any(|d| binding.contains(d)) {
            continue;
        }
        checked += 1;
        assert!(
            block.contains("textFormat: Text.PlainText"),
            "{page_name} label shows data without `textFormat: Text.PlainText` — \
             `Controls.Label` defaults to AutoText, which renders markup in daemon-derived \
             strings (issue #51):\n{block}"
        );
    }
    assert!(
        checked >= min_checked,
        "expected at least {min_checked} data-bearing labels in {page_name}, found {checked} — \
         did the guard's matcher drift from the page?"
    );
}

/// The one line of `block` that is exactly `line` (trimmed).
pub fn has_line(block: &str, line: &str) -> bool {
    block.lines().any(|l| l.trim() == line)
}
