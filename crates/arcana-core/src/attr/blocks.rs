//! Leaf blocks of a markdown note: the unit of ownership.
//!
//! An agent may not insert words inside a block that contains any of the
//! human's words; that becomes a suggestion. Blocks are paragraphs, headings,
//! list items, table rows, fenced code, and the frontmatter.

/// A leaf block as a byte range of the note, excluding its trailing newline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub start: usize,
    pub end: usize,
    pub kind: BlockKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Frontmatter,
    Heading,
    Paragraph,
    ListItem,
    TableRow,
    Code,
}

pub fn blocks(content: &str) -> Vec<Block> {
    let lines: Vec<(usize, &str)> = {
        let mut pos = 0;
        content
            .split_inclusive('\n')
            .map(|l| {
                let start = pos;
                pos += l.len();
                (start, l.trim_end_matches(['\n', '\r']))
            })
            .collect()
    };
    let end_of = |i: usize| lines[i].0 + lines[i].1.len();
    let mut out = Vec::new();
    let mut i = 0;

    // Frontmatter: a leading `---` line up to the next `---` line.
    if lines.first().map(|l| l.1) == Some("---") {
        if let Some(close) = (1..lines.len()).find(|&j| lines[j].1 == "---") {
            out.push(Block {
                start: 0,
                end: end_of(close),
                kind: BlockKind::Frontmatter,
            });
            i = close + 1;
        }
    }

    while i < lines.len() {
        let (start, line) = lines[i];
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let fence = &trimmed[..3];
            let close = (i + 1..lines.len())
                .find(|&j| lines[j].1.trim_start().starts_with(fence))
                .unwrap_or(lines.len() - 1);
            out.push(Block {
                start,
                end: end_of(close),
                kind: BlockKind::Code,
            });
            i = close + 1;
            continue;
        }
        if trimmed.starts_with('#') {
            out.push(Block {
                start,
                end: end_of(i),
                kind: BlockKind::Heading,
            });
            i += 1;
            continue;
        }
        if trimmed.starts_with('|') {
            out.push(Block {
                start,
                end: end_of(i),
                kind: BlockKind::TableRow,
            });
            i += 1;
            continue;
        }
        // Paragraph or list item: runs until a blank line, a new list item,
        // a heading, a fence or a table row.
        let kind = if is_list_item(trimmed) {
            BlockKind::ListItem
        } else {
            BlockKind::Paragraph
        };
        let mut j = i + 1;
        while j < lines.len() {
            let t = lines[j].1.trim_start();
            if t.is_empty()
                || is_list_item(t)
                || t.starts_with('#')
                || t.starts_with("```")
                || t.starts_with("~~~")
                || t.starts_with('|')
            {
                break;
            }
            j += 1;
        }
        out.push(Block {
            start,
            end: end_of(j - 1),
            kind,
        });
        i = j;
    }
    out
}

fn is_list_item(t: &str) -> bool {
    let bullet = ["- ", "* ", "+ "].iter().any(|b| t.starts_with(b));
    let numbered = {
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "))
    };
    bullet || numbered
}

/// The block containing byte `pos`, if any (block ends are exclusive).
pub fn block_at(blocks: &[Block], pos: usize) -> Option<&Block> {
    blocks.iter().find(|b| pos >= b.start && pos < b.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_markdown() {
        let s = "---\ntitle: x\n---\n# Head\n\nPara one\ncontinues.\n\n- item a\n- item b\n\n```\ncode\n```\n";
        let kinds: Vec<_> = blocks(s).iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::Frontmatter,
                BlockKind::Heading,
                BlockKind::Paragraph,
                BlockKind::ListItem,
                BlockKind::ListItem,
                BlockKind::Code
            ]
        );
        let b = blocks(s);
        assert_eq!(&s[b[2].start..b[2].end], "Para one\ncontinues.");
    }
}
