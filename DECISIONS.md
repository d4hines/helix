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
