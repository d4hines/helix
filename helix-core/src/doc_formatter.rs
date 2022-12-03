//! The `DocumentFormatter` forms the bridge between the raw document text
//! and onscreen positioning. It yields the text graphemes as an iterator
//! and transverses (part) of the document text. During that transversal it
//! handles grapheme detection, softwrapping and annotation.
//! It yields `FormattedGrapheme`s and their corresponding visual coortdinates.
//!
//! As both virtual text and softwrapping can insert additional lines into the document
//! it is generally not possible to find the start of the previous visual line.
//! Instead the `DocumentFormatter` starts at the last "checkpoint" (usually a linebreak)
//! called a "block" and the caller must advance it as needed.

use std::borrow::Cow;
use std::fmt::Debug;
use std::mem::{replace, take};

#[cfg(test)]
mod test;

use unicode_segmentation::{Graphemes, UnicodeSegmentation};

use crate::graphemes::Grapheme;
use crate::syntax::Highlight;
use crate::text_annotations::TextAnnotations;
use crate::{Position, RopeGraphemes, RopeSlice};

/// A preprossed Grapheme that is ready for rendering
/// with attachted styling data
#[derive(Debug, Clone)]
pub struct FormattedGrapheme<'a> {
    pub grapheme: Grapheme<'a>,
    pub highlight: Option<Highlight>,
    // the number of chars in the document required by this grapheme
    pub doc_chars: u16,
}

impl<'a> FormattedGrapheme<'a> {
    /// Returns whether this grapheme is virtual inline text
    pub fn is_virtual(&self) -> bool {
        // The highlight field is only used for inline virtual text
        // so it's save to reuse that.
        // We can not use doc_chars here as that is also 0 for the EOF space
        let is_virtual = self.highlight.is_some();
        if is_virtual {
            debug_assert_eq!(self.doc_chars, 0);
        }
        is_virtual
    }
    pub fn placeholder() -> Self {
        FormattedGrapheme {
            grapheme: Grapheme::Space,
            highlight: None,
            doc_chars: 0,
        }
    }

    pub fn new(
        raw: Cow<'a, str>,
        highlight: Option<Highlight>,
        visual_x: usize,
        tab_width: u16,
        chars: u16,
    ) -> FormattedGrapheme<'a> {
        FormattedGrapheme {
            grapheme: Grapheme::new(raw, visual_x, tab_width),
            highlight,
            doc_chars: chars,
        }
    }

    pub fn is_whitespace(&self) -> bool {
        self.grapheme.is_whitespace()
    }

    pub fn is_breaking_space(&self) -> bool {
        self.grapheme.is_breaking_space()
    }

    /// Returns the approximate visual width of this grapheme,
    pub fn width(&self) -> u16 {
        self.grapheme.width()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TextFormat {
    pub soft_wrap: bool,
    pub tab_width: u16,
    pub max_wrap: u16,
    pub max_indent_retain: u16,
    pub wrap_indent: u16,
    pub viewport_width: u16,
}

// test implementation is basically only used for testing or when softwrap is always disabled
impl Default for TextFormat {
    fn default() -> Self {
        TextFormat {
            soft_wrap: false,
            tab_width: 4,
            max_wrap: 3,
            max_indent_retain: 4,
            wrap_indent: 1,
            viewport_width: 17,
        }
    }
}

#[derive(Debug)]
pub struct DocumentFormatter<'t> {
    config: TextFormat,
    annotations: &'t TextAnnotations<'t>,

    /// The visual position at the end of the last yielded word boundary
    visual_pos: Position,
    graphemes: RopeGraphemes<'t>,
    /// The character pos of the `graphemes` iter used for inserting annotations
    char_pos: usize,
    /// The line pos of the `graphemes` iter used for inserting annotations
    line_pos: usize,
    exhausted: bool,

    /// Line breaks to be reserved for virtual text
    /// at the next line break
    virtual_lines: usize,
    inline_anntoation_graphemes: Option<(Graphemes<'t>, Highlight)>,

    // softwrap specific
    /// The indentation of the current line
    /// Is set to `None` if the indentation level is not yet know
    /// because no non-whitespace grahemes has been encountered yet
    indent_level: Option<usize>,
    /// In case a long word needs to be split a single grapheme might need to be wrapped
    /// while the rest of the word stays on the same line
    peeked_grapheme: Option<(FormattedGrapheme<'t>, usize)>,
    /// A first-in first-out (fifo) buffer for the Graphemes of any given word
    word_buf: Vec<FormattedGrapheme<'t>>,
    /// The index of the next grapheme that will be yielded from the `word_buf`
    word_i: usize,
}

impl<'t> DocumentFormatter<'t> {
    /// Creates a new formatter at the last block before `char_idx`.
    /// A block is a chunk which always ends with a linebreak.
    /// This is usally just a normal line break.
    /// However very long lines are always wrapped at constant intervals that can be cheaply calculated
    /// to avoid pathological behaviour.
    pub fn new_at_prev_block(
        text: RopeSlice<'t>,
        config: TextFormat,
        annotations: &'t TextAnnotations<'t>,
        char_idx: usize,
    ) -> (Self, usize) {
        // TODO divide long lines into blocks to avoid bad performance for long lines
        let block_line_idx = text.char_to_line(char_idx);
        let block_char_idx = text.line_to_char(block_line_idx);
        annotations.reset_pos(block_char_idx);
        (
            DocumentFormatter {
                config,
                annotations,
                visual_pos: Position { row: 0, col: 0 },
                graphemes: RopeGraphemes::new(text.slice(block_char_idx..)),
                char_pos: 0,
                exhausted: false,
                virtual_lines: 0,
                indent_level: None,
                peeked_grapheme: None,
                word_buf: Vec::with_capacity(64),
                word_i: 0,
                line_pos: block_line_idx,
                inline_anntoation_graphemes: None,
            },
            block_char_idx,
        )
    }

    fn next_inline_annotation_grapheme(&mut self) -> Option<(&'t str, Highlight)> {
        loop {
            if let Some(&mut (ref mut annotation, highlight)) =
                self.inline_anntoation_graphemes.as_mut()
            {
                if let Some(grapheme) = annotation.next() {
                    return Some((grapheme, highlight));
                }
            }

            if let Some(annotation) = self.annotations.next_inline_annotation_at(self.char_pos) {
                self.inline_anntoation_graphemes = Some((
                    UnicodeSegmentation::graphemes(annotation.text, true),
                    annotation.highlight,
                ))
            } else {
                return None;
            }
        }
    }

    fn advance_grapheme(&mut self, col: usize) -> Option<FormattedGrapheme<'t>> {
        let (grapheme, style, doc_chars) =
            if let Some((grapheme, highlight)) = self.next_inline_annotation_grapheme() {
                (grapheme.into(), Some(highlight), 0)
            } else if let Some(grapheme) = self.graphemes.next() {
                self.virtual_lines += self.annotations.annotation_lines_at(self.char_pos);
                let codepoints = grapheme.len_chars();
                let overlay = self.annotations.overlay_at(self.char_pos);
                let grapheme = match overlay {
                    Some(overlay) => overlay.grapheme.into(),
                    None => grapheme.into(),
                };
                (grapheme, None, codepoints as u16)
            } else {
                if self.exhausted {
                    return None;
                }
                self.exhausted = true;
                // EOF grapheme is required for rendering
                // and correct position computations
                return Some(FormattedGrapheme {
                    grapheme: Grapheme::Space,
                    highlight: None,
                    doc_chars: 0,
                });
            };

        let grapheme =
            FormattedGrapheme::new(grapheme, style, col, self.config.tab_width, doc_chars);

        self.char_pos += doc_chars as usize;
        Some(grapheme)
    }

    fn advance_to_next_word(&mut self) {
        self.word_buf.clear();
        let mut word_width = 0;
        let virtual_lines_before_word = self.virtual_lines;
        let mut virtual_lines_before_grapheme = self.virtual_lines;
        loop {
            // softwrap word if necessary
            if word_width + self.visual_pos.col >= self.config.viewport_width as usize {
                // wrapping this word would move too much text to the next line
                // split the word at the line end instead
                if word_width > self.config.max_wrap as usize {
                    // Usually we stop accomulating graphemes as soon as softwrapping becomes necessary.
                    // However if the last grapheme is multiple columns wide it might extend beyond the EOL.
                    // The condition below ensures that this grapheme is not cutoff and instead wrapped to the next line
                    if word_width + self.visual_pos.col > self.config.viewport_width as usize {
                        self.peeked_grapheme = self.word_buf.pop().map(|grapheme| {
                            (grapheme, self.virtual_lines - virtual_lines_before_grapheme)
                        });
                        self.virtual_lines = virtual_lines_before_grapheme;
                    }
                    return;
                }

                // softwrap this word to the next line
                let indent_carry_over = if let Some(indent) = self.indent_level {
                    if indent as u16 <= self.config.max_indent_retain {
                        indent as u16
                    } else {
                        0
                    }
                } else {
                    0
                };
                let line_indent = indent_carry_over + self.config.wrap_indent;
                self.visual_pos.col = line_indent as usize;
                self.virtual_lines -= virtual_lines_before_word;
                self.visual_pos.row += 1 + virtual_lines_before_word;
            }

            virtual_lines_before_grapheme = self.virtual_lines;

            let grapheme = if let Some((grapheme, virtual_lines)) = self.peeked_grapheme.take() {
                self.virtual_lines += virtual_lines;
                grapheme
            } else if let Some(grapheme) = self.advance_grapheme(self.visual_pos.col + word_width) {
                grapheme
            } else {
                return;
            };

            word_width += grapheme.width() as usize;

            match grapheme.grapheme {
                Grapheme::Newline => {
                    self.indent_level = None;
                    self.word_buf.push(grapheme);
                    return;
                }
                Grapheme::Space | Grapheme::Tab { .. } => {
                    self.word_buf.push(grapheme);
                    return;
                }
                Grapheme::Other { .. } if self.indent_level.is_none() => {
                    self.indent_level = Some(self.visual_pos.col);
                }
                _ => (),
            }
            self.word_buf.push(grapheme);
        }
    }

    /// returns the document line pos of the **next** grapheme that will be yielded
    pub fn line_pos(&self) -> usize {
        self.line_pos
    }
}

impl<'t> Iterator for DocumentFormatter<'t> {
    type Item = (FormattedGrapheme<'t>, Position);

    fn next(&mut self) -> Option<Self::Item> {
        let grapheme = if self.config.soft_wrap {
            if self.word_i >= self.word_buf.len() {
                self.advance_to_next_word();
                self.word_i = 0;
            }
            let grapheme = replace(
                self.word_buf.get_mut(self.word_i)?,
                FormattedGrapheme::placeholder(),
            );
            self.word_i += 1;
            grapheme
        } else {
            self.advance_grapheme(self.visual_pos.col)?
        };

        let pos = self.visual_pos;
        // println!("{pos:?} {}", grapheme.grapheme);
        if grapheme.grapheme == Grapheme::Newline {
            self.visual_pos.row += 1;
            self.visual_pos.row += take(&mut self.virtual_lines);
            self.visual_pos.col = 0;
            self.line_pos += 1;
        } else {
            self.visual_pos.col += grapheme.width() as usize;
        }
        Some((grapheme, pos))
    }
}
