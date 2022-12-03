use crate::doc_formatter::{DocumentFormatter, TextFormat};
use crate::syntax::Highlight;
use crate::text_annotations::{InlineAnnotation, Overlay, TextAnnotations};

const WRAP_INDENT: u16 = 1;
impl TextFormat {
    fn new_test(softwrap: bool) -> Self {
        TextFormat {
            soft_wrap: softwrap,
            tab_width: 2,
            max_wrap: 3,
            max_indent_retain: 4,
            wrap_indent: WRAP_INDENT,
            // use a prime number to allow linging up too often with repear
            viewport_width: 17,
        }
    }
}

impl<'t> DocumentFormatter<'t> {
    fn new_test(
        text: &'t str,
        char_pos: usize,
        softwrap: bool,
        annotations: &'t TextAnnotations<'t>,
    ) -> Self {
        Self::new_at_prev_block(
            text.into(),
            TextFormat::new_test(softwrap),
            annotations,
            char_pos,
        )
        .0
    }

    fn collect_to_str(&mut self) -> String {
        use std::fmt::Write;
        let mut res = String::new();
        let viewport_width = self.config.viewport_width;
        let mut line = 0;

        for (grapheme, pos) in self {
            if pos.row != line {
                line += 1;
                assert_eq!(pos.row, line);
                write!(res, "\n{}", ".".repeat(pos.col)).unwrap();
                assert!(
                    pos.col <= viewport_width as usize,
                    "softwrapped failed {}<={viewport_width}",
                    pos.col
                );
            }
            write!(res, "{}", grapheme.grapheme).unwrap();
        }

        res
    }
}

fn softwrap_text(text: &str, char_pos: usize) -> String {
    DocumentFormatter::new_test(text, char_pos, true, &TextAnnotations::default()).collect_to_str()
}

#[test]
fn basic_softwrap() {
    assert_eq!(
        softwrap_text(&"foo ".repeat(10), 0),
        "foo foo foo foo \n.foo foo foo foo \n.foo foo  "
    );
    assert_eq!(
        softwrap_text(&"fooo ".repeat(10), 0),
        "fooo fooo fooo \n.fooo fooo fooo \n.fooo fooo fooo \n.fooo  "
    );

    // check that we don't wrap unecessarly
    assert_eq!(
        softwrap_text("\t\txxxx1xxxx2xx\n", 0),
        "    xxxx1xxxx2xx \n "
    );
}

#[test]
fn softwrap_indentation() {
    assert_eq!(
        softwrap_text("\t\tfoo1 foo2 foo3 foo4 foo5 foo6\n", 0),
        "    foo1 foo2 \n.....foo3 foo4 \n.....foo5 foo6 \n "
    );
    assert_eq!(
        softwrap_text("\t\t\tfoo1 foo2 foo3 foo4 foo5 foo6\n", 0),
        "      foo1 foo2 \n.foo3 foo4 foo5 \n.foo6 \n "
    );
}

#[test]
fn long_word_softwrap() {
    assert_eq!(
        softwrap_text("\t\txxxx1xxxx2xxxx3xxxx4xxxx5xxxx6xxxx7xxxx8xxxx9xxx\n", 0),
        "    xxxx1xxxx2xxx\n.....x3xxxx4xxxx5\n.....xxxx6xxxx7xx\n.....xx8xxxx9xxx \n "
    );
    assert_eq!(
        softwrap_text("xxxxxxxx1xxxx2xxx\n", 0),
        "xxxxxxxx1xxxx2xxx\n. \n "
    );
    assert_eq!(
        softwrap_text("\t\txxxx1xxxx 2xxxx3xxxx4xxxx5xxxx6xxxx7xxxx8xxxx9xxx\n", 0),
        "    xxxx1xxxx \n.....2xxxx3xxxx4x\n.....xxx5xxxx6xxx\n.....x7xxxx8xxxx9\n.....xxx \n "
    );
    assert_eq!(
        softwrap_text("\t\txxxx1xxx 2xxxx3xxxx4xxxx5xxxx6xxxx7xxxx8xxxx9xxx\n", 0),
        "    xxxx1xxx 2xxx\n.....x3xxxx4xxxx5\n.....xxxx6xxxx7xx\n.....xx8xxxx9xxx \n "
    );
}

fn overlay_text(text: &str, char_pos: usize, softwrap: bool, overlays: &[Overlay]) -> String {
    DocumentFormatter::new_test(
        text,
        char_pos,
        softwrap,
        TextAnnotations::default().add_overlay(overlays),
    )
    .collect_to_str()
}

#[test]
fn overlay() {
    assert_eq!(
        overlay_text(
            "foobar",
            0,
            false,
            &[
                Overlay {
                    char_idx: 0,
                    grapheme: "X",
                    highlight: None
                },
                Overlay {
                    char_idx: 2,
                    grapheme: "\t",
                    highlight: None
                },
            ]
        ),
        "Xo  bar "
    );
    assert_eq!(
        overlay_text(
            &"foo ".repeat(10),
            0,
            true,
            &[
                Overlay {
                    char_idx: 2,
                    grapheme: "\t",
                    highlight: None
                },
                Overlay {
                    char_idx: 5,
                    grapheme: "\t",
                    highlight: None
                },
                Overlay {
                    char_idx: 16,
                    grapheme: "X",
                    highlight: None
                },
            ]
        ),
        "fo   f  o foo \n.foo Xoo foo foo \n.foo foo foo  "
    );
}

fn annotate_text(
    text: &str,
    char_pos: usize,
    softwrap: bool,
    annotations: &[InlineAnnotation],
) -> String {
    DocumentFormatter::new_test(
        text,
        char_pos,
        softwrap,
        TextAnnotations::default().add_inline_annotations(annotations),
    )
    .collect_to_str()
}

#[test]
fn annotation() {
    assert_eq!(
        annotate_text(
            "bar",
            0,
            false,
            &[InlineAnnotation {
                char_idx: 0,
                text: "foo",
                highlight: Highlight(0)
            }]
        ),
        "foobar "
    );
    assert_eq!(
        annotate_text(
            &"foo ".repeat(10),
            0,
            true,
            &[InlineAnnotation {
                char_idx: 0,
                text: "foo ",
                highlight: Highlight(0)
            }]
        ),
        "foo foo foo foo \n.foo foo foo foo \n.foo foo foo  "
    );
}
#[test]
fn annotation_and_overlay() {
    assert_eq!(
        DocumentFormatter::new_test(
            "bbar",
            0,
            false,
            TextAnnotations::default()
                .add_inline_annotations(&[InlineAnnotation {
                    char_idx: 0,
                    text: "fooo",
                    highlight: Highlight(0),
                }])
                .add_overlay(&[Overlay {
                    char_idx: 0,
                    grapheme: "\t",
                    highlight: None
                }]),
        )
        .collect_to_str(),
        "fooo  bar "
    );
}
