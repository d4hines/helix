# Git integration — decisions & judgment calls

Running log of choices made during autonomous implementation that Daniel may
want to review or override. Newest at the bottom. See the derisking artifact
for the full spec.

## P1b: git-targeted diff sessions (`:diff-git`)

- **Command shape**: `:diff-git` = working tree vs index (unstaged, right pane
  editable). `:diff-git <rev>` = working tree vs rev. `:diff-git --staged
  [rev]` = index vs HEAD/rev (both panes read-only). Alias `:dgit`.
- **Left pane is the base** (older content), matching VS Code and vimdiff.
- **Scratch panes and `readonly`**: helix's `readonly` flag only guards saves,
  not edits — so you *can* type into a scratch pane and the diff recomputes
  live (harmless; content is never written anywhere). `:diffget`/`:diffput`
  refuse to write into read-only panes. Blocking edits entirely would need a
  new Document capability; deferred.
- **Conflicted files**: index lookups use the stage-0 entry only, so
  `:diff-git` on a conflicted file errors ("file is not in the index").
  Proper conflict UX is a later phase.
- **Non-UTF8 blobs** go through the same `read_to_string` decoding as normal
  file opens (encoding detection incl. BOM); binary files will render as
  garbage rather than being refused. TODO before upstreaming: refuse binaries.
- **gix `revision` feature** added to helix-vcs so `:diff-git <rev>` can parse
  arbitrary revspecs (`HEAD~2`, branches, hashes).
- **Statusline**: `file-name` element now uses `Document::display_name()`, so
  scratch diff panes show "app.nix (index)" instead of "[scratch]". The
  `file-base-name` element keeps its path-derived behavior unless an override
  is set.

## P2a: staging semantics (property-tested before implementation)

- **Selection→hunk matching**: non-empty hunks match on real line overlap; a
  pure deletion anchors at `after.start` (where the gutter renders its mark /
  the gap sits) and is selected only when the selection contains that line.
  Selecting the line above the gap does NOT stage the deletion.
- **Fixed a #15631 bug** (upstream-worthy): `hunks_intersecting` in
  diff_session.rs matched pure deletions anywhere above the selection's end,
  so `:diffget` with a selection on lines 10-12 would also pull a deletion
  anchored at line 2. Canonical helix-vcs semantics restored + regression test.
- **Write path**: full-content index writes (`git hash-object -w --stdin
  --path` + `git update-index --add --cacheinfo`), VS Code's approach — no
  patch application, no context matching. `--path` means clean/CRLF filters
  apply as if the content came from the worktree.
- **Property suite** (helix-vcs/tests/staging.rs): empty/full selection
  identities, random hunk subsets vs an independent byte-splicing reference,
  stage∘unstage identity, git CLI round-trips incl. no-trailing-newline and
  partial-stage MM status. proptest added as helix-vcs dev-dependency.

## P2b: staging commands

- **`:stage-selection` (alias `:stage`)** works in ANY file-backed buffer, not
  just diff views: it always reads the index fresh from git, diffs it against
  the live buffer, and stages the hunks under the selection. A bare cursor
  stages the hunk under it — so there is no separate stage-hunk command.
- **`:unstage-selection` (alias `:unstage`)** is only valid in the index pane
  of `:diff-git --staged`, mirroring VS Code (unstaging needs HEAD↔index
  coordinates). It uses the session's displayed ropes/hunks so what you see is
  what gets unstaged; if the user edited the (soft) read-only HEAD pane, the
  reverted content is whatever is displayed — their own index, recoverable.
- **`:stage-file` / `:unstage-file`**: `git add` / `git reset -q HEAD --`,
  current buffer's file or an explicit path argument.
- After any staging op, open diff panes showing the index for that file are
  refreshed in place (hunks recompute automatically).
- **UTF-8 only** for selection staging (content is recomposed via ropes);
  non-UTF8 encodings bail with a clear error. Untracked files bail toward
  :stage-file (no index entry to partially stage against).
- **No default keybindings yet** — typed commands only. Keys land with the
  status panel phase so the whole map is designed at once.
