use std::cell::Cell;
use std::convert::identity;
use std::ops::Range;

use crate::syntax::Highlight;

#[derive(Debug)]
pub struct InlineAnnotation<'t> {
    pub highlight: Highlight,
    pub text: &'t str,
    pub char_idx: usize,
}

#[derive(Debug)]
pub struct Overlay<'t> {
    pub char_idx: usize,
    pub grapheme: &'t str,
    pub highlight: Option<Highlight>,
}

#[derive(Debug)]
pub struct LineAnnotation {
    pub anchor_char_idx: usize,
    pub height: usize,
}

#[derive(Debug)]
struct Layer<'a, T> {
    annotations: &'a [T],
    current_index: Cell<usize>,
}

impl<'a, T> Clone for Layer<'a, T> {
    fn clone(&self) -> Self {
        Layer {
            annotations: self.annotations,
            current_index: self.current_index.clone(),
        }
    }
}

impl<'a, T> Layer<'a, T> {
    pub fn reset_pos(&self, char_idx: usize, get_char_idx: impl Fn(&T) -> usize) {
        let new_index = self
            .annotations
            .binary_search_by_key(&char_idx, get_char_idx)
            .unwrap_or_else(identity);

        self.current_index.set(new_index);
    }

    pub fn consume(&self, char_idx: usize, get_char_idx: impl Fn(&T) -> usize) -> Option<&'a T> {
        let annot = self.annotations.get(self.current_index.get())?;
        debug_assert!(get_char_idx(annot) >= char_idx);
        if get_char_idx(annot) == char_idx {
            self.current_index.set(self.current_index.get() + 1);
            Some(annot)
        } else {
            None
        }
    }
}

impl<'a, T> From<&'a [T]> for Layer<'a, T> {
    fn from(annotations: &'a [T]) -> Layer<'a, T> {
        Layer {
            annotations,
            current_index: Cell::new(0),
        }
    }
}

fn reset_pos<T>(layers: &[Layer<T>], pos: usize, get_pos: impl Fn(&T) -> usize) {
    for layer in layers {
        layer.reset_pos(pos, &get_pos)
    }
}

#[derive(Default, Debug, Clone)]
pub struct TextAnnotations<'t> {
    inline_annotations: Vec<Layer<'t, InlineAnnotation<'t>>>,
    overlays: Vec<Layer<'t, Overlay<'t>>>,
    line_annotations: Vec<Layer<'t, LineAnnotation>>,
}

impl<'t> TextAnnotations<'t> {
    pub fn reset_pos(&self, char_idx: usize) {
        reset_pos(&self.inline_annotations, char_idx, |annot| annot.char_idx);
        reset_pos(&self.overlays, char_idx, |annot| annot.char_idx);
        reset_pos(&self.line_annotations, char_idx, |annot| {
            annot.anchor_char_idx
        });
    }

    pub fn collect_overlay_highlights(
        &self,
        char_range: Range<usize>,
    ) -> Vec<(usize, Range<usize>)> {
        let mut highlights = Vec::new();
        for char_idx in char_range {
            if let Some(Overlay {
                highlight: Some(highlight),
                ..
            }) = self.overlay_at(char_idx)
            {
                // we don't know the number of chars the original grapheme takes
                // however it doesn't matter as highlight bounderies are automatically
                // aligned to grapheme boundaries in the rendering code
                highlights.push((highlight.0, char_idx..char_idx + 1))
            }
        }

        highlights
    }

    pub fn add_inline_annotations(&mut self, layer: &'t [InlineAnnotation<'t>]) -> &mut Self {
        self.inline_annotations.push(layer.into());
        self
    }

    pub fn add_overlay(&mut self, layer: &'t [Overlay<'t>]) -> &mut Self {
        self.overlays.push(layer.into());
        self
    }

    pub fn add_line_annotation(&mut self, layer: &'t [LineAnnotation]) -> &mut Self {
        self.line_annotations.push(layer.into());
        self
    }

    pub fn clear_line_annotations(&mut self) {
        self.line_annotations.clear();
    }

    pub(crate) fn next_inline_annotation_at(
        &self,
        char_idx: usize,
    ) -> Option<&'t InlineAnnotation<'t>> {
        self.inline_annotations
            .iter()
            .find_map(|layer| layer.consume(char_idx, |annot| annot.char_idx))
    }

    pub(crate) fn overlay_at(&self, char_idx: usize) -> Option<&'t Overlay<'t>> {
        let mut overlay = None;
        for layer in &self.overlays {
            if let Some(new_overlay) = layer.consume(char_idx, |annot| annot.char_idx) {
                overlay = Some(new_overlay)
            }
        }
        overlay
    }

    pub(crate) fn annotation_lines_at(&self, char_idx: usize) -> usize {
        self.line_annotations
            .iter()
            .map(|layer| {
                let mut lines = 0;
                while let Some(annot) = layer.annotations.get(layer.current_index.get()) {
                    if annot.anchor_char_idx == char_idx {
                        layer.current_index.set(layer.current_index.get() + 1);
                        lines += annot.height
                    } else {
                        break;
                    }
                }
                lines
            })
            .sum()
    }
}
