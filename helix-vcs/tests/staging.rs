//! Property-based and edge-case tests for partial staging
//! (`helix_vcs::stage`). These encode the intended semantics ahead of the
//! editor commands that use them:
//!
//! - empty selection stages nothing; full selection stages everything
//! - staging a subset of hunks produces exactly `before` + those hunks,
//!   verified against an independent byte-splicing reference implementation
//! - stage-then-unstage of the same hunks is the identity
//! - contents written through the git CLI round-trip byte-for-byte, and
//!   `git add` composes on top of a partially staged index
//!
//! Hunks are computed with the same tokenization the editor's diff session
//! uses (ropey `lines()`, Histogram algorithm), so line indices here mean the
//! same thing they mean in production.

use helix_core::{Rope, RopeSlice};
use helix_vcs::stage::{
    apply_selected_hunks, hunk_selected, revert_selected_hunks, write_index_content, SelectionSide,
};
use helix_vcs::Hunk;
use imara_diff::{Algorithm, Diff, InternedInput};
use proptest::prelude::*;

struct RopeLines<'a>(RopeSlice<'a>);

impl<'a> imara_diff::TokenSource for RopeLines<'a> {
    type Token = RopeSlice<'a>;
    type Tokenizer = helix_core::ropey::iter::Lines<'a>;

    fn tokenize(&self) -> Self::Tokenizer {
        self.0.lines()
    }

    fn estimate_tokens(&self) -> u32 {
        self.0.len_lines() as u32
    }
}

/// Compute hunks exactly as `DiffSession::compute_hunks` does.
fn compute_hunks(before: &Rope, after: &Rope) -> Vec<Hunk> {
    let input = InternedInput::new(RopeLines(before.slice(..)), RopeLines(after.slice(..)));
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_with(
        &input.before,
        &input.after,
        imara_diff::IndentHeuristic::new(|token| {
            imara_diff::IndentLevel::for_ascii_line(input.interner[token].bytes(), 4)
        }),
    );
    diff.hunks().collect()
}

/// Byte offsets of each ropey-style line start, with a final sentinel at
/// `len`. `starts[i]` is where line `i` begins; `starts[len_lines]` == len.
fn line_starts(s: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    // ropey counts the empty segment after a trailing newline as a line, so
    // `starts` already has len_lines entries; add the sentinel.
    starts.push(s.len());
    starts
}

/// Independent reference: splice the given hunks (by index) into `before`
/// with plain byte-slicing, no selection predicate involved.
fn reference_splice(before: &str, after: &str, hunks: &[Hunk], chosen: &[usize]) -> String {
    let lb = line_starts(before);
    let la = line_starts(after);
    let mut out = String::new();
    let mut pos = 0usize;
    for &i in chosen {
        let h = &hunks[i];
        out += &before[pos..lb[h.before.start as usize]];
        out += &after[la[h.after.start as usize]..la[h.after.end as usize]];
        pos = lb[h.before.end as usize];
    }
    out += &before[pos..];
    out
}

/// Selection ranges (on the after side) that cover exactly the chosen hunks
/// under `hunk_selected` semantics: real ranges for non-empty hunks, the
/// anchor line for pure deletions.
fn ranges_covering(hunks: &[Hunk], chosen: &[usize]) -> Vec<(usize, usize)> {
    chosen
        .iter()
        .map(|&i| {
            let r = &hunks[i].after;
            if r.is_empty() {
                (r.start as usize, r.start as usize)
            } else {
                (r.start as usize, r.end as usize - 1)
            }
        })
        .collect()
}

/// True when the covering ranges for `chosen` accidentally also select other
/// hunks (adjacent hunks can share an anchor line). Such subsets are skipped
/// in exactness assertions.
fn selection_is_exact(hunks: &[Hunk], chosen: &[usize], ranges: &[(usize, usize)]) -> bool {
    hunks
        .iter()
        .enumerate()
        .all(|(i, h)| hunk_selected(&h.after, ranges) == chosen.contains(&i))
}

fn line_pool() -> Vec<String> {
    [
        "fn main() {",
        "    let x = 1;",
        "    println!(\"hi\");",
        "}",
        "",
        "// comment",
        "use std::io;",
        "    return x;",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

prop_compose! {
    /// A document built from a small pool of lines (so diffs get real
    /// structure from repetition), with or without a trailing newline.
    fn doc()(
        lines in prop::collection::vec(prop::sample::select(line_pool()), 0..25),
        trailing in any::<bool>(),
    ) -> String {
        let mut s = lines.join("\n");
        if trailing && !s.is_empty() {
            s.push('\n');
        }
        s
    }
}

prop_compose! {
    /// A pair of related documents: `after` is `before` with random line
    /// drops, replacements, and insertions.
    fn doc_pair()(before in doc())(
        before in Just(before.clone()),
        ops in prop::collection::vec((any::<u16>(), prop::sample::select(line_pool())), 0..12),
        trailing in any::<bool>(),
    ) -> (String, String) {
        let mut lines: Vec<String> = before.lines().map(str::to_string).collect();
        for (seed, line) in ops {
            if lines.is_empty() {
                lines.push(line);
                continue;
            }
            let idx = seed as usize % lines.len();
            match seed % 3 {
                0 => { lines.remove(idx); }
                1 => lines[idx] = line,
                _ => lines.insert(idx, line),
            }
        }
        let mut after = lines.join("\n");
        if trailing && !after.is_empty() {
            after.push('\n');
        }
        (before, after)
    }
}

proptest! {
    #[test]
    fn empty_selection_stages_nothing((before, after) in doc_pair()) {
        let (rb, ra) = (Rope::from(before.as_str()), Rope::from(after.as_str()));
        let hunks = compute_hunks(&rb, &ra);
        let result = apply_selected_hunks(&rb, &ra, &hunks, &[], SelectionSide::After);
        prop_assert_eq!(result.to_string(), before);
    }

    #[test]
    fn full_selection_stages_everything((before, after) in doc_pair()) {
        let (rb, ra) = (Rope::from(before.as_str()), Rope::from(after.as_str()));
        let hunks = compute_hunks(&rb, &ra);
        let all = [(0usize, ra.len_lines())];
        let result = apply_selected_hunks(&rb, &ra, &hunks, &all, SelectionSide::After);
        prop_assert_eq!(result.to_string(), after);
    }

    #[test]
    fn subset_matches_reference_splice(
        (before, after) in doc_pair(),
        subset_seed in any::<u64>(),
    ) {
        let (rb, ra) = (Rope::from(before.as_str()), Rope::from(after.as_str()));
        let hunks = compute_hunks(&rb, &ra);
        let chosen: Vec<usize> = (0..hunks.len())
            .filter(|i| (subset_seed >> (i % 64)) & 1 == 1)
            .collect();
        let ranges = ranges_covering(&hunks, &chosen);
        prop_assume!(selection_is_exact(&hunks, &chosen, &ranges));

        let result = apply_selected_hunks(&rb, &ra, &hunks, &ranges, SelectionSide::After);
        let expected = reference_splice(&before, &after, &hunks, &chosen);
        prop_assert_eq!(result.to_string(), expected);
    }

    #[test]
    fn stage_then_unstage_is_identity(
        (before, after) in doc_pair(),
        subset_seed in any::<u64>(),
    ) {
        let (rb, ra) = (Rope::from(before.as_str()), Rope::from(after.as_str()));
        let hunks = compute_hunks(&rb, &ra);
        let chosen: Vec<usize> = (0..hunks.len())
            .filter(|i| (subset_seed >> (i % 64)) & 1 == 1)
            .collect();
        let ranges = ranges_covering(&hunks, &chosen);
        prop_assume!(selection_is_exact(&hunks, &chosen, &ranges));

        // Stage the chosen hunks: index goes before -> staged.
        let staged = apply_selected_hunks(&rb, &ra, &hunks, &ranges, SelectionSide::After);

        // Unstage everything that stage introduced, from the before↔staged
        // diff (the "staged changes" view): index returns to before.
        let hunks2 = compute_hunks(&rb, &staged);
        let all = [(0usize, staged.len_lines())];
        let unstaged = revert_selected_hunks(&rb, &staged, &hunks2, &all, SelectionSide::After);
        prop_assert_eq!(unstaged.to_string(), before);
    }
}

// ---- edge cases ----

fn stage_all(before: &str, after: &str) -> String {
    let (rb, ra) = (Rope::from(before), Rope::from(after));
    let hunks = compute_hunks(&rb, &ra);
    apply_selected_hunks(&rb, &ra, &hunks, &[(0, ra.len_lines())], SelectionSide::After).to_string()
}

#[test]
fn trailing_newline_combinations() {
    for before in ["a\nb\nc\n", "a\nb\nc"] {
        for after in ["a\nB\nc\n", "a\nB\nc"] {
            assert_eq!(stage_all(before, after), after, "{before:?} -> {after:?}");
        }
    }
}

#[test]
fn crlf_content_passes_through() {
    let before = "a\r\nb\r\n";
    let after = "a\r\nX\r\nb\r\n";
    assert_eq!(stage_all(before, after), after);
}

#[test]
fn adjacent_hunks_stage_independently() {
    // before -> after: line 1 replaced AND a line inserted right after it;
    // depending on the diff these may be one or two hunks. If two, staging
    // only the first must leave the second unstaged.
    let before = Rope::from("a\nb\nc\n");
    let after = Rope::from("a\nB\nB2\nc\n");
    let hunks = compute_hunks(&before, &after);
    if hunks.len() == 2 {
        let ranges = ranges_covering(&hunks, &[0]);
        if selection_is_exact(&hunks, &[0], &ranges) {
            let result = apply_selected_hunks(&before, &after, &hunks, &ranges, SelectionSide::After);
            let expected = reference_splice("a\nb\nc\n", "a\nB\nB2\nc\n", &hunks, &[0]);
            assert_eq!(result.to_string(), expected);
        }
    }
}

#[test]
fn pure_deletion_selected_by_anchor_line() {
    // Delete "b": gap anchors at after-line 1 (the "c" line).
    let before = Rope::from("a\nb\nc\n");
    let after = Rope::from("a\nc\n");
    let hunks = compute_hunks(&before, &after);
    assert_eq!(hunks.len(), 1);
    assert!(hunks[0].after.is_empty());
    let anchor = hunks[0].after.start as usize;

    // Selecting the anchor line stages the deletion.
    let result =
        apply_selected_hunks(&before, &after, &hunks, &[(anchor, anchor)], SelectionSide::After);
    assert_eq!(result.to_string(), "a\nc\n");

    // Selecting a line below the anchor does not.
    if anchor + 1 < after.len_lines() {
        let result = apply_selected_hunks(
            &before,
            &after,
            &hunks,
            &[(anchor + 1, anchor + 1)],
            SelectionSide::After,
        );
        assert_eq!(result.to_string(), "a\nb\nc\n");
    }
}

#[test]
fn deletion_above_selection_is_not_staged() {
    // Regression guard for the intersection predicate: a deletion anchored
    // well above the selection must not match it.
    let before = Rope::from("a\nb\nc\nd\ne\nf\n");
    let after = Rope::from("a\nc\nd\ne\nF\n"); // deleted "b" (anchor 1), changed f (line 4)
    let hunks = compute_hunks(&before, &after);
    assert_eq!(hunks.len(), 2);
    // Select only the f change.
    let ranges = [(4usize, 4usize)];
    assert!(!hunk_selected(&hunks[0].after, &ranges));
    let result = apply_selected_hunks(&before, &after, &hunks, &ranges, SelectionSide::After);
    assert_eq!(result.to_string(), "a\nb\nc\nd\ne\nF\n");
}

// ---- git CLI round-trip ----

fn init_repo(dir: &std::path::Path) {
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "test"]);
    run(&["config", "commit.gpgsign", "false"]);
}

fn git_stdout(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn partial_stage_roundtrips_through_git() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init_repo(dir);

    let before = "a\nb\nc\nd\ne\n";
    let after = "a\nB\nc\nd\nE\n"; // two separate hunks
    std::fs::write(dir.join("f.txt"), before).unwrap();
    git_stdout(dir, &["add", "f.txt"]);
    git_stdout(dir, &["commit", "-qm", "init"]);
    std::fs::write(dir.join("f.txt"), after).unwrap();

    let (rb, ra) = (Rope::from(before), Rope::from(after));
    let hunks = compute_hunks(&rb, &ra);
    assert_eq!(hunks.len(), 2);

    // Stage only the first hunk (the B change).
    let ranges = ranges_covering(&hunks, &[0]);
    let staged = apply_selected_hunks(&rb, &ra, &hunks, &ranges, SelectionSide::After);
    write_index_content(dir, std::path::Path::new("f.txt"), staged.to_string().as_bytes()).unwrap();

    // The index now holds exactly our computed content...
    assert_eq!(git_stdout(dir, &["show", ":f.txt"]), staged.to_string());
    // ...git sees the file as partially staged (both staged and unstaged changes)...
    assert_eq!(git_stdout(dir, &["status", "--porcelain", "--", "f.txt"]).trim(), "MM f.txt");
    // ...and a plain `git add` composes on top, reaching the worktree state.
    git_stdout(dir, &["add", "f.txt"]);
    assert_eq!(git_stdout(dir, &["show", ":f.txt"]), after);
    assert_eq!(git_stdout(dir, &["status", "--porcelain", "--", "f.txt"]).trim(), "M  f.txt");
}

#[test]
fn no_trailing_newline_roundtrips_through_git() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init_repo(dir);

    let before = "a\nb";
    let after = "a\nB";
    std::fs::write(dir.join("f.txt"), before).unwrap();
    git_stdout(dir, &["add", "f.txt"]);
    git_stdout(dir, &["commit", "-qm", "init"]);
    std::fs::write(dir.join("f.txt"), after).unwrap();

    let (rb, ra) = (Rope::from(before), Rope::from(after));
    let hunks = compute_hunks(&rb, &ra);
    let staged = apply_selected_hunks(&rb, &ra, &hunks, &[(0, 5)], SelectionSide::After);
    write_index_content(dir, std::path::Path::new("f.txt"), staged.to_string().as_bytes()).unwrap();

    assert_eq!(git_stdout(dir, &["show", ":f.txt"]), after);
    // Index matches worktree exactly: nothing left unstaged.
    assert_eq!(git_stdout(dir, &["diff", "--", "f.txt"]).trim(), "");
}
