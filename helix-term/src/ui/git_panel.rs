//! Docked git status panel: a built-in, non-moveable panel on the left edge
//! (the shape of VS Code's Source Control view). Lists staged/unstaged
//! changes with per-file click glyphs and keyboard actions; Enter (or a
//! click on the file name) opens the appropriate diff view. Toggled with
//! `git_panel_toggle` (default `C-S-d`) or `:git-panel`.
//!
//! Keys (while the panel is focused):
//!   j/k, Up/Down   move    g/G  first/last     space  mark for batch op
//!   s  stage       u  unstage                  d      discard (confirm: d)
//!   a  stage all   c  commit (prompt)          P      push
//!   R  refresh     Enter  open diff            Esc    cancel/unfocus
//!   q              close panel

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use helix_view::input::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use helix_view::theme::Modifier;
use helix_view::Editor;
use tui::buffer::Buffer as Surface;

use crate::commands;
use crate::compositor::{self, Context as CompositorContext};
use crate::ui;

pub const PANEL_WIDTH: u16 = 36;

/// Nerd-font devicon + color for a file name. Colors approximate the
/// devicons defaults, chosen to sit well on the catppuccin grounds; the
/// fallback is a plain neutral file glyph.
fn file_icon(name: &str) -> (&'static str, helix_view::theme::Color) {
    use helix_view::theme::Color::Rgb;
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "nix" => ("\u{f313}", Rgb(0x7e, 0xbA, 0xe4)),
        "rs" => ("\u{e7a8}", Rgb(0xef, 0x97, 0x76)),
        "ts" | "tsx" => ("\u{e628}", Rgb(0x51, 0x9a, 0xba)),
        "js" | "jsx" | "mjs" => ("\u{e74e}", Rgb(0xe5, 0xc8, 0x90)),
        "json" => ("\u{e60b}", Rgb(0xe5, 0xc8, 0x90)),
        "md" | "markdown" => ("\u{f48a}", Rgb(0x8c, 0xaa, 0xee)),
        "toml" => ("\u{e6b2}", Rgb(0xef, 0x97, 0x76)),
        "yaml" | "yml" => ("\u{e6a8}", Rgb(0xca, 0x9e, 0xe6)),
        "lock" => ("\u{f023}", Rgb(0x94, 0x9c, 0xbb)),
        "sh" | "bash" | "zsh" => ("\u{f489}", Rgb(0xa6, 0xd1, 0x89)),
        "py" => ("\u{e73c}", Rgb(0xe5, 0xc8, 0x90)),
        "go" => ("\u{e627}", Rgb(0x81, 0xc8, 0xbe)),
        "html" => ("\u{e736}", Rgb(0xef, 0x97, 0x76)),
        "css" | "scss" => ("\u{e749}", Rgb(0x8c, 0xaa, 0xee)),
        "c" | "h" => ("\u{e61e}", Rgb(0x8c, 0xaa, 0xee)),
        "cpp" | "hpp" | "cc" => ("\u{e61d}", Rgb(0x8c, 0xaa, 0xee)),
        "rb" => ("\u{e739}", Rgb(0xe7, 0x82, 0x84)),
        "txt" => ("\u{f15c}", Rgb(0xa5, 0xad, 0xce)),
        _ => ("\u{f016}", Rgb(0x94, 0x9c, 0xbb)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Staged,
    Unstaged,
}

#[derive(Debug, Clone)]
struct FileItem {
    /// Repo-relative path.
    rela: String,
    /// Status letter shown at the right edge (A/M/D/R/U/C/T...).
    letter: char,
    section: Section,
}

#[derive(Debug, Clone, Copy)]
enum Row {
    Header,
    Blank,
    Section(Section),
    File(usize),
}

pub struct GitPanel {
    pub focused: bool,
    /// Set by the panel to ask EditorView to drop it.
    pub close_requested: bool,
    /// Screen area of the panel, set each render; used for mouse mapping.
    area: helix_view::graphics::Rect,
    workdir: PathBuf,
    branch: String,
    ahead: Option<u32>,
    behind: Option<u32>,
    files: Vec<FileItem>,
    rows: Vec<Row>,
    /// Cursor as an index into `rows` (kept on a File row when one exists).
    cursor: usize,
    scroll: usize,
    /// Marked file indices for batch operations.
    marked: HashSet<usize>,
    /// File indices awaiting a second `d` to confirm discard.
    pending_discard: Option<Vec<usize>>,
    error: Option<String>,
    /// When the last `F` fetch completed, for the header's freshness note.
    last_fetch: Option<std::time::Instant>,
}

impl GitPanel {
    pub fn new(workdir: PathBuf) -> Self {
        let mut panel = Self {
            focused: true,
            close_requested: false,
            area: helix_view::graphics::Rect::default(),
            workdir,
            branch: String::new(),
            ahead: None,
            behind: None,
            files: Vec::new(),
            rows: Vec::new(),
            cursor: 0,
            scroll: 0,
            marked: HashSet::new(),
            pending_discard: None,
            error: None,
            last_fetch: None,
        };
        panel.refresh();
        panel
    }

    /// Re-read `git status` and rebuild the rows. Clears marks and any
    /// pending confirmation.
    pub fn refresh(&mut self) {
        self.marked.clear();
        self.pending_discard = None;
        self.error = None;
        match helix_vcs::stage::run_git(
            &self.workdir,
            &["status", "--porcelain=v2", "--branch", "-z"],
            None,
        ) {
            Ok(output) => self.parse_status(&output),
            Err(err) => {
                self.files.clear();
                self.error = Some(err.to_string());
            }
        }
        self.rebuild_rows();
    }

    fn parse_status(&mut self, input: &str) {
        self.branch.clear();
        self.ahead = None;
        self.behind = None;
        self.files.clear();

        let mut toks = input.split('\0');
        while let Some(tok) = toks.next() {
            if tok.is_empty() {
                continue;
            }
            if let Some(head) = tok.strip_prefix("# branch.head ") {
                self.branch = head.to_string();
            } else if let Some(ab) = tok.strip_prefix("# branch.ab ") {
                for part in ab.split_whitespace() {
                    if let Some(n) = part.strip_prefix('+') {
                        self.ahead = n.parse().ok();
                    } else if let Some(n) = part.strip_prefix('-') {
                        self.behind = n.parse().ok();
                    }
                }
            } else if tok.starts_with("1 ") {
                // 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
                let Some(path) = tok.splitn(9, ' ').nth(8) else {
                    continue;
                };
                let mut xy = tok[2..4].chars();
                let (x, y) = (xy.next().unwrap_or('.'), xy.next().unwrap_or('.'));
                self.push_entry(path, x, y);
            } else if tok.starts_with("2 ") {
                // 2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path> NUL <orig>
                let _orig = toks.next(); // consume the rename source
                let Some(path) = tok.splitn(10, ' ').nth(9) else {
                    continue;
                };
                let mut xy = tok[2..4].chars();
                let (x, y) = (xy.next().unwrap_or('.'), xy.next().unwrap_or('.'));
                self.push_entry(path, x, y);
            } else if let Some(path) = tok.strip_prefix("? ") {
                self.files.push(FileItem {
                    rela: path.to_string(),
                    letter: 'U',
                    section: Section::Unstaged,
                });
            } else if tok.starts_with("u ") {
                // u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>
                if let Some(path) = tok.splitn(11, ' ').nth(10) {
                    self.files.push(FileItem {
                        rela: path.to_string(),
                        letter: 'C',
                        section: Section::Unstaged,
                    });
                }
            }
        }

        // Stable order: staged first, each section sorted by path.
        self.files.sort_by(|a, b| {
            (a.section == Section::Unstaged, &a.rela)
                .cmp(&(b.section == Section::Unstaged, &b.rela))
        });
    }

    fn push_entry(&mut self, path: &str, x: char, y: char) {
        if x != '.' && x != '?' {
            self.files.push(FileItem {
                rela: path.to_string(),
                letter: x,
                section: Section::Staged,
            });
        }
        if y != '.' {
            self.files.push(FileItem {
                rela: path.to_string(),
                letter: y,
                section: Section::Unstaged,
            });
        }
    }

    fn section_len(&self, section: Section) -> usize {
        self.files.iter().filter(|f| f.section == section).count()
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        self.rows.push(Row::Header);
        if self.section_len(Section::Staged) > 0 {
            self.rows.push(Row::Blank);
            self.rows.push(Row::Section(Section::Staged));
            for i in 0..self.files.len() {
                if self.files[i].section == Section::Staged {
                    self.rows.push(Row::File(i));
                }
            }
        }
        self.rows.push(Row::Blank);
        self.rows.push(Row::Section(Section::Unstaged));
        for i in 0..self.files.len() {
            if self.files[i].section == Section::Unstaged {
                self.rows.push(Row::File(i));
            }
        }

        if !self.row_is_file(self.cursor) {
            self.cursor = self
                .rows
                .iter()
                .position(|r| matches!(r, Row::File(_)))
                .unwrap_or(0);
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    fn row_is_file(&self, idx: usize) -> bool {
        matches!(self.rows.get(idx), Some(Row::File(_)))
    }

    fn file_at(&self, row_idx: usize) -> Option<usize> {
        match self.rows.get(row_idx) {
            Some(Row::File(i)) => Some(*i),
            _ => None,
        }
    }

    /// Files an action applies to: the marked set when non-empty, else the
    /// file under the cursor.
    fn action_targets(&self) -> Vec<usize> {
        if !self.marked.is_empty() {
            let mut v: Vec<usize> = self.marked.iter().copied().collect();
            v.sort_unstable();
            v
        } else {
            self.file_at(self.cursor).into_iter().collect()
        }
    }

    fn cursor_move(&mut self, down: bool) {
        let mut idx = self.cursor;
        loop {
            let next = if down {
                idx + 1
            } else if idx == 0 {
                return;
            } else {
                idx - 1
            };
            if next >= self.rows.len() {
                return;
            }
            idx = next;
            if self.row_is_file(idx) {
                self.cursor = idx;
                self.scroll_into_view();
                return;
            }
        }
    }

    fn cursor_to_edge(&mut self, last: bool) {
        let pos = if last {
            self.rows.iter().rposition(|r| matches!(r, Row::File(_)))
        } else {
            self.rows.iter().position(|r| matches!(r, Row::File(_)))
        };
        if let Some(pos) = pos {
            self.cursor = pos;
            self.scroll_into_view();
        }
    }

    fn content_height(&self) -> usize {
        self.area.height.saturating_sub(1) as usize // -1 for the footer
    }

    fn scroll_into_view(&mut self) {
        let height = self.content_height();
        if height == 0 {
            return;
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
    }

    // ---- actions ----

    fn stage_or_unstage_targets(&mut self, editor: &mut Editor, unstage: bool) {
        let from_section = if unstage {
            Section::Staged
        } else {
            Section::Unstaged
        };
        let mut touched = Vec::new();
        for i in self.action_targets() {
            let file = &self.files[i];
            if file.section != from_section {
                continue;
            }
            let rela = PathBuf::from(&file.rela);
            let result = if unstage {
                helix_vcs::stage::unstage_file(&self.workdir, &rela)
            } else {
                helix_vcs::stage::stage_file(&self.workdir, &rela)
            };
            match result {
                Ok(()) => touched.push(self.workdir.join(&file.rela)),
                Err(err) => editor.set_error(err.to_string()),
            }
        }
        self.refresh();
        for path in touched {
            let trust = commands::typed::git_trust(editor, &path);
            commands::typed::refresh_index_panes_from_git(editor, &path, trust);
        }
    }

    fn stage_all(&mut self, editor: &mut Editor) {
        if let Err(err) = helix_vcs::stage::run_git(&self.workdir, &["add", "-A"], None) {
            editor.set_error(err.to_string());
        }
        self.refresh();
    }

    fn request_discard(&mut self) {
        let targets: Vec<usize> = self
            .action_targets()
            .into_iter()
            .filter(|&i| self.files[i].section == Section::Unstaged)
            .collect();
        if !targets.is_empty() {
            self.pending_discard = Some(targets);
        }
    }

    fn confirm_discard(&mut self, editor: &mut Editor) {
        let Some(targets) = self.pending_discard.take() else {
            return;
        };
        let mut restored = Vec::new();
        for i in targets {
            let file = &self.files[i];
            let abs = self.workdir.join(&file.rela);
            let result = if file.letter == 'U' {
                // Discarding an untracked file deletes it.
                std::fs::remove_file(&abs).map_err(anyhow::Error::from)
            } else {
                helix_vcs::stage::run_git(&self.workdir, &["checkout", "--", &file.rela], None)
                    .map(|_| ())
            };
            match result {
                Ok(()) => restored.push(abs),
                Err(err) => editor.set_error(err.to_string()),
            }
        }
        // Reload any open buffers for restored files so the editor shows the
        // restored content instead of the discarded edits.
        for abs in restored {
            reload_doc_by_path(editor, &abs);
        }
        self.refresh();
    }

    /// Enter / click on a file name: open the most useful view for the entry.
    fn open_entry(&mut self, editor: &mut Editor, file_idx: usize) {
        let file = self.files[file_idx].clone();
        let abs = self.workdir.join(&file.rela);
        self.focused = false;
        let result = match (file.section, file.letter) {
            // Untracked: plain open.
            (Section::Unstaged, 'U') => editor
                .open(&abs, helix_view::editor::Action::Replace)
                .map(|_| ())
                .map_err(anyhow::Error::from),
            // Deleted from the worktree: show the last (index) content.
            (Section::Unstaged, 'D') => {
                commands::typed::open_git_scratch(editor, &abs, helix_vcs::FileRevision::Index)
            }
            // Deleted in the index: show the HEAD content.
            (Section::Staged, 'D') => {
                commands::typed::open_git_scratch(editor, &abs, helix_vcs::FileRevision::Head)
            }
            // Newly added to the index: no HEAD side to diff against.
            (Section::Staged, 'A') => {
                commands::typed::open_git_scratch(editor, &abs, helix_vcs::FileRevision::Index)
            }
            (Section::Unstaged, _) => commands::typed::open_git_diff_session(
                editor,
                &abs,
                commands::typed::GitDiffMode::Unstaged,
            ),
            (Section::Staged, _) => commands::typed::open_git_diff_session(
                editor,
                &abs,
                commands::typed::GitDiffMode::Staged,
            ),
        };
        if let Err(err) = result {
            editor.set_error(err.to_string());
            self.focused = true;
        }
    }

    fn commit_prompt(&self, cx: &mut commands::Context) {
        let workdir = self.workdir.clone();
        cx.callback.push(Box::new(move |compositor, _cx| {
            let prompt = ui::Prompt::new(
                "commit message:".into(),
                None,
                |_editor: &Editor, _input: &str| Vec::new(),
                move |cx: &mut CompositorContext, input: &str, event: ui::PromptEvent| {
                    if event != ui::PromptEvent::Validate || input.trim().is_empty() {
                        return;
                    }
                    match helix_vcs::stage::run_git(
                        &workdir,
                        &["commit", "-m", input.trim()],
                        None,
                    ) {
                        Ok(_) => cx.editor.set_status("Committed"),
                        Err(err) => cx.editor.set_error(err.to_string()),
                    }
                },
            );
            compositor.push(Box::new(prompt));
        }));
        // Refresh once the prompt closes; done lazily on the next key the
        // panel sees (R also forces it). Cheap safety net:
        cx.callback.push(Box::new(|compositor, _cx| {
            if let Some(editor_view) = compositor.find::<ui::EditorView>() {
                if let Some(panel) = &mut editor_view.git_panel {
                    panel.refresh();
                }
            }
        }));
    }

    /// Async `git fetch` then refresh, so ahead/behind is trustworthy.
    fn fetch(&self, cx: &mut commands::Context) {
        let workdir = self.workdir.clone();
        cx.editor.set_status("Fetching...");
        cx.jobs.callback(async move {
            let result = tokio::task::spawn_blocking(move || {
                helix_vcs::stage::run_git(&workdir, &["fetch", "--quiet"], None)
            })
            .await?;
            let call: crate::job::Callback = crate::job::Callback::EditorCompositor(Box::new(
                move |editor: &mut Editor, compositor: &mut compositor::Compositor| {
                    match result {
                        Ok(_) => editor.set_status("Fetched"),
                        Err(err) => editor.set_error(err.to_string()),
                    }
                    if let Some(editor_view) = compositor.find::<ui::EditorView>() {
                        if let Some(panel) = &mut editor_view.git_panel {
                            panel.last_fetch = Some(std::time::Instant::now());
                            panel.refresh();
                        }
                    }
                },
            ));
            Ok(call)
        });
    }

    fn push(&self, cx: &mut commands::Context) {
        let workdir = self.workdir.clone();
        cx.editor.set_status("Pushing...");
        cx.jobs.callback(async move {
            let result = tokio::task::spawn_blocking(move || {
                helix_vcs::stage::run_git(&workdir, &["push"], None)
            })
            .await?;
            let call: crate::job::Callback = crate::job::Callback::EditorCompositor(Box::new(
                move |editor: &mut Editor, compositor: &mut compositor::Compositor| {
                    match result {
                        Ok(_) => editor.set_status("Pushed"),
                        Err(err) => editor.set_error(err.to_string()),
                    }
                    if let Some(editor_view) = compositor.find::<ui::EditorView>() {
                        if let Some(panel) = &mut editor_view.git_panel {
                            panel.refresh();
                        }
                    }
                },
            ));
            Ok(call)
        });
    }

    // ---- input ----

    /// Handle a key while the panel is focused. Compositor-level effects
    /// (prompts) are pushed onto `cx.callback`.
    pub fn handle_key(&mut self, key: KeyEvent, cx: &mut commands::Context) {
        // A pending discard confirmation swallows the next key.
        if self.pending_discard.is_some() {
            match key.code {
                KeyCode::Char('d') => self.confirm_discard(cx.editor),
                _ => self.pending_discard = None,
            }
            return;
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.cursor_move(true),
            KeyCode::Char('k') | KeyCode::Up => self.cursor_move(false),
            KeyCode::Char('g') => self.cursor_to_edge(false),
            KeyCode::Char('G') => self.cursor_to_edge(true),
            KeyCode::Char(' ') => {
                if let Some(i) = self.file_at(self.cursor) {
                    if !self.marked.remove(&i) {
                        self.marked.insert(i);
                    }
                    self.cursor_move(true);
                }
            }
            KeyCode::Char('s') => self.stage_or_unstage_targets(cx.editor, false),
            KeyCode::Char('u') => self.stage_or_unstage_targets(cx.editor, true),
            KeyCode::Char('a') => self.stage_all(cx.editor),
            KeyCode::Char('d') => self.request_discard(),
            KeyCode::Char('c') => self.commit_prompt(cx),
            KeyCode::Char('P') => self.push(cx),
            KeyCode::Char('F') => self.fetch(cx),
            KeyCode::Char('R') => self.refresh(),
            KeyCode::Enter => {
                if let Some(i) = self.file_at(self.cursor) {
                    self.open_entry(cx.editor, i);
                }
            }
            KeyCode::Esc => self.focused = false,
            KeyCode::Char('q') => self.close_requested = true,
            _ => {}
        }
    }

    /// Returns true when the event was consumed by the panel.
    pub fn handle_mouse(&mut self, event: &MouseEvent, cx: &mut commands::Context) -> bool {
        let (col, row) = (event.column, event.row);
        let inside = col >= self.area.x
            && col < self.area.right()
            && row >= self.area.y
            && row < self.area.bottom();

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if inside => {
                self.focused = true;
                let row_idx = (row - self.area.y) as usize + self.scroll;
                if let Some(i) = self.file_at(row_idx) {
                    self.cursor = row_idx;
                    // The status letter cell toggles stage/unstage; the rest
                    // of the row opens the entry (VS Code single-click).
                    let letter_x = self.area.x + self.area.width - 3;
                    if col == letter_x || col == letter_x + 1 {
                        let unstage = self.files[i].section == Section::Staged;
                        self.marked.clear();
                        self.marked.insert(i);
                        self.stage_or_unstage_targets(cx.editor, unstage);
                    } else {
                        self.open_entry(cx.editor, i);
                    }
                }
                true
            }
            MouseEventKind::ScrollDown if inside => {
                let max = self.rows.len().saturating_sub(self.content_height());
                self.scroll = (self.scroll + 3).min(max);
                true
            }
            MouseEventKind::ScrollUp if inside => {
                self.scroll = self.scroll.saturating_sub(3);
                true
            }
            MouseEventKind::Down(_) => {
                // Click outside the panel: yield focus back to the editor.
                self.focused = false;
                false
            }
            _ => inside,
        }
    }

    // ---- rendering ----

    pub fn render(
        &mut self,
        area: helix_view::graphics::Rect,
        surface: &mut Surface,
        editor: &Editor,
    ) {
        self.area = area;
        let theme = &editor.theme;
        let base = theme.get("ui.background");
        let text = theme.get("ui.text");
        let dim = theme.get("ui.text.inactive");
        let selected = theme.get("ui.menu.selected");
        let border = theme.get("ui.window");
        let plus = theme.get("diff.plus");
        let minus = theme.get("diff.minus");
        let warning = theme.get("warning");

        surface.set_style(area, base);

        // Right border separating the panel from the editor.
        for y in area.top()..area.bottom() {
            surface[(area.right() - 1, y)]
                .set_symbol(tui::symbols::line::VERTICAL)
                .set_style(border);
        }

        let content_width = (area.width - 1) as usize;
        let height = self.content_height();
        // VS Code letter colors: modifications amber, additions green,
        // deletions/conflicts red.
        let modified = theme.get("warning");

        for (screen_row, row_idx) in (self.scroll..self.rows.len()).enumerate() {
            if screen_row >= height {
                break;
            }
            let y = area.y + screen_row as u16;
            match self.rows[row_idx] {
                Row::Blank => {}
                Row::Header => {
                    // "  master ↑1 ↓0 · fetched 2m ago" with a hairline below.
                    let branch_style = theme.get("keyword").patch(base);
                    surface.set_stringn(
                        area.x,
                        y,
                        &format!(" \u{e0a0} {}", self.branch),
                        content_width,
                        branch_style,
                    );
                    let branch_cols = 3 + self.branch.chars().count() as u16 + 1;
                    let mut meta = String::new();
                    if let (Some(a), Some(b)) = (self.ahead, self.behind) {
                        meta.push_str(&format!("↑{a} ↓{b}"));
                    }
                    if let Some(at) = self.last_fetch {
                        let mins = at.elapsed().as_secs() / 60;
                        let ago = if mins == 0 {
                            "fetched just now".to_string()
                        } else {
                            format!("fetched {mins}m ago")
                        };
                        if !meta.is_empty() {
                            meta.push_str(" · ");
                        }
                        meta.push_str(&ago);
                    }
                    if !meta.is_empty() && (branch_cols as usize) < content_width {
                        surface.set_stringn(
                            area.x + branch_cols,
                            y,
                            &meta,
                            content_width - branch_cols as usize,
                            dim,
                        );
                    }
                    if screen_row + 1 < height && row_idx + 1 <= self.rows.len() {
                        surface.set_stringn(
                            area.x,
                            y + 1,
                            &"─".repeat(content_width),
                            content_width,
                            border,
                        );
                    }
                }
                Row::Section(section) => {
                    let (label, count) = match section {
                        Section::Staged => ("STAGED CHANGES", self.section_len(Section::Staged)),
                        Section::Unstaged => ("CHANGES", self.section_len(Section::Unstaged)),
                    };
                    surface.set_stringn(area.x, y, &format!(" {label}"), content_width, dim);
                    let label_cols = 1 + label.chars().count() as u16;
                    surface.set_stringn(
                        area.x + label_cols + 1,
                        y,
                        &count.to_string(),
                        content_width.saturating_sub(label_cols as usize + 1),
                        text,
                    );
                }
                Row::File(i) => {
                    let file = &self.files[i];
                    let is_cursor = row_idx == self.cursor;
                    let row_style = if is_cursor && self.focused {
                        selected
                    } else if is_cursor {
                        selected.remove_modifier(Modifier::BOLD)
                    } else {
                        base
                    };
                    if is_cursor {
                        let row_rect =
                            helix_view::graphics::Rect::new(area.x, y, area.width - 1, 1);
                        surface.set_style(row_rect, row_style);
                    }

                    // batch marker
                    let marker = if self.marked.contains(&i) { "•" } else { " " };
                    surface.set_string(
                        area.x,
                        y,
                        marker,
                        theme.get("keyword").patch(row_style),
                    );

                    // devicon
                    let path = Path::new(&file.rela);
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| file.rela.clone());
                    let (icon, icon_color) = file_icon(&name);
                    surface.set_string(
                        area.x + 2,
                        y,
                        icon,
                        helix_view::theme::Style::default()
                            .fg(icon_color)
                            .patch(row_style),
                    );

                    // file name + dimmed directory
                    let dir = path
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let name_x = area.x + 4;
                    let max_name = content_width.saturating_sub(6);
                    surface.set_stringn(name_x, y, &name, max_name, text.patch(row_style));
                    let after_name = name_x + (name.chars().count().min(max_name)) as u16;
                    let letter_x = area.x + area.width - 3;
                    if !dir.is_empty() && after_name + 2 < letter_x {
                        surface.set_stringn(
                            after_name + 1,
                            y,
                            &dir,
                            (letter_x - after_name - 2) as usize,
                            dim.patch(row_style),
                        );
                    }

                    // Inline action hints on the cursor row (storybook S2),
                    // drawn over the directory tail when space is tight.
                    if is_cursor && self.focused && self.pending_discard.is_none() {
                        let hint = match file.section {
                            Section::Staged => "[u]nstage",
                            Section::Unstaged => "[s]tage [d]iscard",
                        };
                        let hint_len = hint.chars().count() as u16;
                        let hint_x = letter_x.saturating_sub(hint_len + 1);
                        if hint_x > after_name + 1 {
                            surface.set_string(hint_x, y, hint, dim.patch(row_style));
                        }
                    }

                    // status letter (also the mouse target for stage/unstage)
                    let letter_style = match file.letter {
                        'A' | 'U' => plus,
                        'D' | 'C' => minus,
                        _ => modified,
                    };
                    surface.set_string(
                        letter_x,
                        y,
                        &file.letter.to_string(),
                        letter_style.patch(row_style),
                    );
                }
            }
        }

        // Footer: pending confirmation, error, or key hints.
        let footer_y = area.bottom() - 1;
        if let Some(targets) = &self.pending_discard {
            surface.set_stringn(
                area.x,
                footer_y,
                &format!(" discard {} file(s)? d=yes esc=no", targets.len()),
                content_width,
                warning.add_modifier(Modifier::BOLD),
            );
        } else if let Some(err) = &self.error {
            surface.set_stringn(
                area.x,
                footer_y,
                &format!(" {err}"),
                content_width,
                theme.get("error"),
            );
        } else if self.focused {
            surface.set_stringn(
                area.x,
                footer_y,
                " s:stage u:unstage d:discard c:commit",
                content_width,
                dim,
            );
        }
    }
}

/// Reload the open document for `path`, if any, so discarded edits disappear
/// from the buffer. Mirrors the `:reload` flow.
fn reload_doc_by_path(editor: &mut Editor, path: &Path) {
    let Some((doc_id, view_ids)) = editor.documents().find_map(|doc| {
        (doc.path().map(|p| p == path).unwrap_or(false)).then(|| {
            (
                doc.id(),
                doc.selections().keys().copied().collect::<Vec<_>>(),
            )
        })
    }) else {
        return;
    };
    let Some(&view_id) = view_ids.first() else {
        return;
    };
    let trust_full = {
        let doc = match editor.document(doc_id) {
            Some(doc) => doc,
            None => return,
        };
        editor
            .workspace_trust
            .query(
                doc.workspace_root(),
                helix_loader::workspace_trust::TrustQuery::Git,
            )
            .is_trusted()
    };
    let diff_providers = editor.diff_providers.clone();
    if editor.tree.try_get(view_id).is_none() {
        return;
    }
    let view = editor.tree.get_mut(view_id);
    let Some(doc) = editor.documents.get_mut(&doc_id) else {
        return;
    };
    view.sync_changes(doc);
    if let Err(err) = doc.reload(view, &diff_providers, trust_full) {
        log::error!("failed to reload {}: {err}", path.display());
    }
}
