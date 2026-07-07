//! Partial staging: compute new index contents from a line selection over a
//! diff, and write them into the git index.
//!
//! The content computation is pure (and property-tested); the write path
//! shells out to the `git` binary the same way VS Code's git extension does,
//! writing a blob with `git hash-object -w` and pointing the index entry at it
//! with `git update-index --cacheinfo`. No patch application is involved, so
//! there is no context matching to go wrong.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use helix_core::{Rope, RopeSlice};

use crate::Hunk;

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

/// Line-level hunks between two ropes, using the same algorithm and
/// tokenization as the editor's diff session (Histogram with the indent
/// heuristic postprocess), so line indices agree with what a diff view shows.
pub fn diff_hunks(before: &Rope, after: &Rope) -> Vec<Hunk> {
    let input = imara_diff::InternedInput::new(
        RopeLines(before.slice(..)),
        RopeLines(after.slice(..)),
    );
    let mut diff = imara_diff::Diff::compute(imara_diff::Algorithm::Histogram, &input);
    diff.postprocess_with(
        &input.before,
        &input.after,
        imara_diff::IndentHeuristic::new(|token| {
            imara_diff::IndentLevel::for_ascii_line(input.interner[token].bytes(), 4)
        }),
    );
    diff.hunks().collect()
}

/// Inclusive, 0-indexed line ranges of a selection, as produced by
/// `Selection::line_ranges`.
pub type LineRanges = [(usize, usize)];

/// Which side of the hunks the selection line ranges refer to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionSide {
    /// Ranges are line numbers in the `before` text.
    Before,
    /// Ranges are line numbers in the `after` text.
    After,
}

/// Whether a hunk range (on the side the selection refers to) is covered by
/// the selection.
///
/// Non-empty ranges match on real line overlap. An empty range is a pure
/// deletion on this side: it renders as a gap anchored just above line
/// `start`, and matches when the selection contains that anchor line. This is
/// the same anchor the diff gutter uses for deletion marks, so "cursor on the
/// mark" selects the deletion.
pub fn hunk_selected(hunk_range: &Range<u32>, ranges: &LineRanges) -> bool {
    ranges.iter().any(|&(start, end)| {
        if hunk_range.is_empty() {
            start <= hunk_range.start as usize && hunk_range.start as usize <= end
        } else {
            hunk_range.start as usize <= end && hunk_range.end as usize > start
        }
    })
}

/// Returns `before` with every hunk that [`hunk_selected`] matches applied,
/// i.e. those line ranges replaced by the corresponding `after` content.
///
/// `hunks` must be sorted and non-overlapping, exactly as produced by
/// imara-diff over (`before`, `after`) with line tokenization.
///
/// Staging: `before` = index text, `after` = worktree text, selection on
/// [`SelectionSide::After`] (the editable pane). The result is the new index
/// content with exactly the selected changes staged.
///
/// Unstaging (from a HEAD↔index diff): `before` = HEAD text, `after` = index
/// text, selection on [`SelectionSide::After`]; apply the *unselected* hunks
/// instead by inverting the subset at the call site — or equivalently, use
/// [`revert_selected_hunks`].
pub fn apply_selected_hunks(
    before: &Rope,
    after: &Rope,
    hunks: &[Hunk],
    ranges: &LineRanges,
    side: SelectionSide,
) -> Rope {
    splice_hunks(before, after, hunks, |hunk| {
        let sel_range = match side {
            SelectionSide::Before => &hunk.before,
            SelectionSide::After => &hunk.after,
        };
        hunk_selected(sel_range, ranges)
    })
}

/// Returns `after` with every hunk that [`hunk_selected`] matches reverted to
/// the `before` content. Used to unstage a selection: `before` = HEAD text,
/// `after` = index text, selection on the index side yields the new index
/// content with the selected changes unstaged.
pub fn revert_selected_hunks(
    before: &Rope,
    after: &Rope,
    hunks: &[Hunk],
    ranges: &LineRanges,
    side: SelectionSide,
) -> Rope {
    // Reverting selected hunks of before→after is applying them to the
    // swapped diff after→before.
    let swapped: Vec<Hunk> = hunks.iter().map(Hunk::invert).collect();
    let swapped_side = match side {
        SelectionSide::Before => SelectionSide::After,
        SelectionSide::After => SelectionSide::Before,
    };
    apply_selected_hunks(after, before, &swapped, ranges, swapped_side)
}

/// Core splice: emit `before`, substituting `after`'s range for every hunk
/// `selected` returns true for.
fn splice_hunks(
    before: &Rope,
    after: &Rope,
    hunks: &[Hunk],
    mut selected: impl FnMut(&Hunk) -> bool,
) -> Rope {
    let mut result = Rope::new();
    // Next before-line not yet emitted.
    let mut emitted = 0usize;
    for hunk in hunks {
        if !selected(hunk) {
            continue;
        }
        append_lines(&mut result, before, emitted, hunk.before.start as usize);
        append_lines(
            &mut result,
            after,
            hunk.after.start as usize,
            hunk.after.end as usize,
        );
        emitted = hunk.before.end as usize;
    }
    append_lines(&mut result, before, emitted, before.len_lines());
    result
}

fn append_lines(dst: &mut Rope, src: &Rope, start_line: usize, end_line: usize) {
    if start_line >= end_line {
        return;
    }
    let start = src.line_to_char(start_line);
    let end = src.line_to_char(end_line);
    dst.append(src.slice(start..end).into());
}

/// Run `git` in `workdir` with `args`, feeding `stdin` if given; returns
/// trimmed stdout. Fails with git's stderr on non-zero exit.
pub fn run_git(workdir: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-c")
        .arg("core.quotepath=off")
        .args(args)
        .current_dir(workdir)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("failed to spawn git")?;
    if let Some(bytes) = stdin {
        use std::io::Write;
        child
            .stdin
            .take()
            .context("no stdin handle")?
            .write_all(bytes)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Point the index entry for `rela_path` at `content`, preserving the entry's
/// current file mode (default `100644` when the file is new to the index).
///
/// This is the VS Code write path: `git hash-object -w --stdin --path <p>`
/// (so clean filters and CRLF conversion apply, exactly as if the content had
/// been read from the worktree) followed by
/// `git update-index --add --cacheinfo <mode>,<oid>,<p>`.
pub fn write_index_content(workdir: &Path, rela_path: &Path, content: &[u8]) -> Result<()> {
    let rela = rela_path
        .to_str()
        .context("non-UTF8 paths are not supported for staging")?;

    let mode = run_git(workdir, &["ls-files", "--stage", "--", rela], None)?
        .split_whitespace()
        .next()
        .filter(|m| m.len() == 6)
        .unwrap_or("100644")
        .to_string();

    let oid = run_git(
        workdir,
        &["hash-object", "-w", "--stdin", "--path", rela],
        Some(content),
    )?;

    run_git(
        workdir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("{mode},{oid},{rela}"),
        ],
        None,
    )?;
    Ok(())
}

/// The repository worktree root containing `path` (`git rev-parse
/// --show-toplevel`).
pub fn repo_workdir(path: &Path) -> Result<PathBuf> {
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().context("path has no parent")?
    };
    let top = run_git(dir, &["rev-parse", "--show-toplevel"], None)?;
    Ok(PathBuf::from(top))
}

/// Stage a whole file (`git add`). Works for modifications, new files, and
/// deletions (`add` records deletions since git 2.0).
pub fn stage_file(workdir: &Path, rela_path: &Path) -> Result<()> {
    let rela = rela_path
        .to_str()
        .context("non-UTF8 paths are not supported for staging")?;
    run_git(workdir, &["add", "--", rela], None).map(|_| ())
}

/// Unstage a whole file (`git reset -q HEAD -- <p>`), returning the index
/// entry to the HEAD version (or removing it for newly added files).
pub fn unstage_file(workdir: &Path, rela_path: &Path) -> Result<()> {
    let rela = rela_path
        .to_str()
        .context("non-UTF8 paths are not supported for staging")?;
    run_git(workdir, &["reset", "-q", "HEAD", "--", rela], None).map(|_| ())
}
