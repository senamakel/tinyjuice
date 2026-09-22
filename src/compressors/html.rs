//! HTML → readable-text extractor.
//!
//! Strips markup and returns the readable text content, in the spirit of
//! Headroom's `HTMLExtractor`. Linear-time, allocation-light (no DOM, no
//! regex): it scans once, dropping `<script>`/`<style>`/`<head>` bodies and
//! comments, inserting newlines at block-level boundaries, and decoding the
//! handful of common HTML entities. Lossy — the router offloads the original
//! HTML to CCR so the exact markup is recoverable.

use async_trait::async_trait;

use super::Compressor;
use crate::cache::CcrStore;
use crate::pipeline::{OffloadOutput, OffloadTransform, PipelineInput, estimate_bloat};
use crate::types::{CompressInput, CompressOptions, CompressOutput, CompressorKind, ContentKind};

/// Block-level tags after which we emit a newline so the extracted text keeps
/// document structure (paragraphs, list items, headings, rows).
const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "br",
    "li",
    "ul",
    "ol",
    "tr",
    "table",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "section",
    "article",
    "header",
    "footer",
    "blockquote",
    "pre",
    "hr",
    "title",
];

/// Tags whose entire body is dropped (non-content). `head` is deliberately
/// not here: dropping it would lose `<title>` — often the highest-signal
/// string on the page — while scripts/styles inside the head are still
/// dropped by their own tags and meta/link carry no text.
const DROP_BODY_TAGS: &[&str] = &["script", "style", "noscript", "svg"];

/// Inline formatting tags that do not break the text flow. Any other tag acts
/// as a separator (space or newline) so adjacent element values — e.g. RSS
/// `<guid>` / `<comments>` siblings — don't run together in the output.
const INLINE_TAGS: &[&str] = &[
    "a", "b", "i", "em", "strong", "span", "code", "small", "sub", "sup",
];

pub struct HtmlCompressor;

/// Typed offload transform for lossy HTML-to-text extraction.
pub struct HtmlExtractTransform;

impl OffloadTransform for HtmlExtractTransform {
    fn name(&self) -> &'static str {
        "html_extract"
    }

    fn estimate_bloat(&self, input: &PipelineInput<'_>) -> f32 {
        if input.content_kind != ContentKind::Html {
            return 0.0;
        }
        f32::from(estimate_bloat(input.content, input.content_kind).score) / 100.0
    }

    fn apply(&self, input: &PipelineInput<'_>, store: &dyn CcrStore) -> Option<OffloadOutput> {
        if input.content_kind != ContentKind::Html {
            return None;
        }
        let compacted = compress(input.content)?;
        OffloadOutput::from_retained_put(
            compacted.text,
            CompressorKind::Html,
            store.put(input.original_content),
        )
    }
}

#[async_trait]
impl Compressor for HtmlCompressor {
    fn kind(&self) -> CompressorKind {
        CompressorKind::Html
    }

    async fn compress(
        &self,
        input: &CompressInput<'_>,
        _opts: &CompressOptions,
    ) -> Option<CompressOutput> {
        compress(input.content)
    }
}

/// Extract readable text from an HTML document. Returns `None` if extraction
/// wouldn't shrink the content or yields nothing useful.
pub fn compress(content: &str) -> Option<CompressOutput> {
    let text = html_to_text(content);
    let text = collapse_blank_lines(&text);
    if text.trim().is_empty() || text.len() >= content.len() {
        return None;
    }
    log::debug!(
        "[tinyjuice][html] {} -> {} bytes",
        content.len(),
        text.len()
    );
    // Extraction is an information-preserving reshape: every readable text node
    // and the title survive; only non-content bodies (script/style/svg) and the
    // markup scaffolding are dropped. Marking it a faithful reformat lets it
    // ship without CCR (nothing recoverable is lost) instead of being declined
    // as an unrecoverable partial view.
    Some(CompressOutput::reformatted(text, CompressorKind::Html))
}

/// Single-pass HTML tag stripper that drops non-content bodies, honours block
/// boundaries, and decodes common entities.
pub fn html_to_text(html: &str) -> String {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0usize;
    let mut skip_until: Option<&'static str> = None;
    // Number of `<![CDATA[` openers whose `]]>` closer we still owe. CDATA
    // contents are scanned by this same loop (HN RSS wraps HTML in CDATA, so
    // stripping its tags is what we want); only the delimiters are consumed
    // as markup. Tracked iteratively rather than recursing on the payload so
    // pathological nesting can't blow the stack.
    let mut cdata_depth = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(skip_tag) = skip_until {
                // Inside a dropped body only the matching close tag is markup.
                // Anything else — comparison operators in inline JS, stray
                // `<` in CSS or CDATA — is body text, so a lone `<` must not
                // consume up to the next `>` (that could swallow the real
                // close tag and drop the rest of the document).
                if html[i + 1..].starts_with('/')
                    && let Some(rel_end) = html[i..].find('>')
                {
                    let (name, is_close) = parse_tag_name(&html[i + 1..i + rel_end]);
                    if is_close && name == skip_tag {
                        skip_until = None;
                        i += rel_end + 1;
                        continue;
                    }
                }
                i += 1;
                continue;
            }
            // Comment?
            if html[i..].starts_with("<!--") {
                if let Some(end) = html[i..].find("-->") {
                    i += end + 3;
                    continue;
                }
                break;
            }
            // CDATA section: the delimiters are markup, the payload is
            // scanned by this same loop (see `cdata_depth`).
            if html[i..].starts_with("<![CDATA[") {
                cdata_depth += 1;
                i += "<![CDATA[".len();
                continue;
            }
            // Find the end of this tag, skipping `>` inside quoted attribute
            // values (e.g. `media="(width >= 40rem)"`).
            let Some(rel_end) = find_tag_end(html, i) else {
                break;
            };
            let tag_raw = &html[i + 1..i + rel_end];
            let (name, is_close) = parse_tag_name(tag_raw);

            if !is_close && DROP_BODY_TAGS.contains(&name.as_str()) && !tag_raw.ends_with('/') {
                skip_until = Some(static_tag(&name));
                i += rel_end + 1;
                continue;
            }

            if BLOCK_TAGS.contains(&name.as_str()) {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            } else if !INLINE_TAGS.contains(&name.as_str())
                && !out.is_empty()
                && !out.ends_with(|c: char| c.is_whitespace())
            {
                // Unrecognised tag: emit a separator so sibling element
                // values (RSS `<guid>`, `<pubDate>`, ...) don't concatenate.
                out.push(' ');
            }
            i += rel_end + 1;
            continue;
        }

        if skip_until.is_some() {
            i += 1;
            continue;
        }

        // Consume a pending CDATA closer as markup, not text.
        if cdata_depth > 0 && html[i..].starts_with("]]>") {
            cdata_depth -= 1;
            i += "]]>".len();
            continue;
        }

        // Decode an entity or copy the char.
        if bytes[i] == b'&'
            && let Some((decoded, consumed)) = decode_entity(&html[i..])
        {
            out.push_str(&decoded);
            i += consumed;
            continue;
        }
        let ch = html[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Find the offset (relative to `from`, which points at `<`) of the `>` that
/// terminates the tag, skipping `>` inside single- or double-quoted attribute
/// values. If a quote is left unterminated, falls back to the first raw `>`
/// so one malformed tag can't swallow the rest of the document.
fn find_tag_end(html: &str, from: usize) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut j = from + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'>' => return Some(j - from),
            quote @ (b'"' | b'\'') => match bytes[j + 1..].iter().position(|&b| b == quote) {
                Some(close) => j += close + 2,
                None => return html[from..].find('>'),
            },
            _ => j += 1,
        }
    }
    None
}

/// Parse `<...>` inner text into `(lowercased name, is_closing)`.
fn parse_tag_name(tag_raw: &str) -> (String, bool) {
    let trimmed = tag_raw.trim();
    let (is_close, rest) = if let Some(r) = trimmed.strip_prefix('/') {
        (true, r)
    } else {
        (false, trimmed)
    };
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    (name, is_close)
}

/// Return the `'static` slice matching a recognised drop-body tag name.
fn static_tag(name: &str) -> &'static str {
    DROP_BODY_TAGS
        .iter()
        .copied()
        .find(|t| *t == name)
        .unwrap_or("script")
}

/// Decode a leading HTML entity at the start of `s`. Returns the decoded text
/// and the number of bytes consumed (including `&` and `;`).
fn decode_entity(s: &str) -> Option<(std::borrow::Cow<'static, str>, usize)> {
    const ENTITIES: &[(&str, &str)] = &[
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&nbsp;", " "),
        ("&mdash;", "—"),
        ("&ndash;", "–"),
        ("&hellip;", "…"),
        ("&copy;", "©"),
    ];
    for (ent, decoded) in ENTITIES {
        if s.starts_with(ent) {
            return Some(((*decoded).into(), ent.len()));
        }
    }
    // Numeric character references: &#8212; and &#x27;
    let rest = s.strip_prefix("&#")?;
    let (digits, radix) = match rest.strip_prefix(['x', 'X']) {
        Some(hex) => (hex, 16),
        None => (rest, 10),
    };
    let end = digits
        .char_indices()
        .take(8)
        .take_while(|(_, c)| c.is_ascii_hexdigit())
        .last()
        .map(|(i, c)| i + c.len_utf8())?;
    if !digits[end..].starts_with(';') {
        return None;
    }
    let code = u32::from_str_radix(&digits[..end], radix).ok()?;
    let ch = char::from_u32(code).filter(|c| !c.is_control() || *c == '\n' || *c == '\t')?;
    let consumed = s.len() - digits.len() + end + 1;
    Some((ch.to_string().into(), consumed))
}

/// Collapse runs of blank lines and trim trailing whitespace per line.
fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks = 0usize;
    for line in text.lines() {
        let trimmed = line.trim_end();
        let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            blanks += 1;
            if blanks <= 1 {
                out.push('\n');
            }
        } else {
            blanks = 0;
            out.push_str(&collapsed);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

// ---------------------------------------------------------------------------
// HTML -> Markdown
// ---------------------------------------------------------------------------
//
// `html_to_text` answers "what would a reader see". Callers that feed an LLM
// want one more thing: the *roles* those bytes played. Headings tell a model
// which section answers its question and link targets are what it feeds the
// next fetch, so a text extractor throws away exactly the structure the model
// acts on. `html_to_markdown` keeps headings, links, lists, code blocks and
// emphasis, and is otherwise the same single-pass scanner — same CDATA
// handling, same quoted-attribute tolerance, same entity table.

/// Tags whose entire body is dropped in Markdown mode. Wider than
/// `DROP_BODY_TAGS` because a document rendered for a model has no use for
/// form controls or embedded objects either.
const MD_DROP_BODY_TAGS: &[&str] = &[
    "script", "style", "noscript", "svg", "template", "canvas", "iframe", "object", "embed",
    "math", "form", "select",
];

/// Tags that force a line break so stripping markup doesn't run a heading
/// into the paragraph after it. Headings and list items are handled
/// separately because they also emit a marker.
const MD_BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "dd",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hr",
    "main",
    "nav",
    "ol",
    "p",
    "section",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
];

/// Inline tags that must not introduce a separator.
const MD_INLINE_TAGS: &[&str] = &[
    "a", "b", "i", "em", "strong", "span", "code", "small", "sub", "sup", "abbr", "cite", "q", "u",
    "s", "mark", "time", "var", "kbd", "samp", "label", "font",
];

/// Upper bound on one extracted link target. A tracking URL can run to
/// several KB of query string, which is pure cost in a transcript.
const MD_MAX_HREF_CHARS: usize = 300;

/// Convert an HTML document to Markdown, preserving the structure a model
/// acts on: headings, link targets, list nesting, fenced code and emphasis.
///
/// Lossy in the same way `html_to_text` is — attributes other than `href` and
/// `alt` are dropped, as are the bodies of `MD_DROP_BODY_TAGS`. An `img` keeps
/// its caption but never its `src`, so a multi-KB base64 `data:` blob can't
/// reach the caller.
pub fn html_to_markdown(html: &str) -> String {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len() / 4);
    let mut i = 0usize;
    let mut skip_until: Option<&'static str> = None;
    // Depth of same-tag nesting inside a dropped body, e.g. a `<template>`
    // nested inside another dropped `<template>`. The first matching close
    // tag only ends skipping once every nested opener has been accounted
    // for; otherwise inner content past that close tag leaks as text.
    let mut skip_depth = 0usize;
    let mut cdata_depth = 0usize;
    // Open `<a>` elements: where their text began in `out`, and the href to
    // wrap it with on close.
    let mut links: Vec<(usize, String)> = Vec::new();
    let mut list_depth = 0usize;
    // Inside `<pre>` whitespace and markup are content, so it is buffered
    // separately: literal `<`/`>` survive, nested highlighting tags are
    // dropped, and the fence length is only chosen once the whole body is
    // known (it must exceed the longest backtick run inside it).
    let mut pre_depth = 0usize;
    let mut pre_buffer = String::new();
    // Currently open `<b>/<strong>/<i>/<em>/<code>` markers: which marker
    // and where in `out` it was opened, so a close tag can undo exactly the
    // marker it opened rather than guessing from whatever `out` currently
    // ends with (that guess breaks on nested same-type emphasis and on
    // literal marker text in the content, see `md_close_emphasis`).
    let mut emphasis: Vec<(&'static str, usize)> = Vec::new();

    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(skip_tag) = skip_until {
                // Inside a dropped body only a tag matching the dropped name
                // is markup; a lone `<` in inline JS or CSS is body text.
                if let Some(rel_end) = find_tag_end(html, i) {
                    let (name, is_close) = parse_tag_name(&html[i + 1..i + rel_end]);
                    if name == skip_tag {
                        let self_closing = html[i + 1..i + rel_end].trim_end().ends_with('/');
                        if is_close {
                            if skip_depth == 0 {
                                skip_until = None;
                            } else {
                                skip_depth -= 1;
                            }
                        } else if !self_closing {
                            skip_depth += 1;
                        }
                        i += rel_end + 1;
                        continue;
                    }
                }
                i += 1;
                continue;
            }
            if html[i..].starts_with("<!--") {
                match html[i..].find("-->") {
                    Some(end) => {
                        i += end + 3;
                        continue;
                    }
                    None => break,
                }
            }
            if html[i..].starts_with("<![CDATA[") {
                cdata_depth += 1;
                i += "<![CDATA[".len();
                continue;
            }
            // A `<` not followed by a name/`/`/`!`/`?` start is not a tag
            // opener (e.g. a bare comparison inside `<pre>` text); treat it
            // as a literal character instead of hunting for the next `>`,
            // which would otherwise swallow unrelated content up to it.
            let looks_like_tag = matches!(
                html[i + 1..].chars().next(),
                Some(c) if c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?'
            );
            if !looks_like_tag {
                if pre_depth > 0 {
                    pre_buffer.push('<');
                } else {
                    out.push('<');
                }
                i += 1;
                continue;
            }
            let Some(rel_end) = find_tag_end(html, i) else {
                break;
            };
            let tag_raw = &html[i + 1..i + rel_end];
            let (name, is_close) = parse_tag_name(tag_raw);
            let self_closing = tag_raw.trim_end().ends_with('/');

            // `embed` has no closing tag in real markup (it is a void
            // element); treating it like the other drop-body tags would arm
            // `skip_until` until EOF and silently discard everything after
            // it. Skip only the tag itself.
            if !is_close && name == "embed" {
                i += rel_end + 1;
                continue;
            }

            if !is_close && !self_closing && MD_DROP_BODY_TAGS.contains(&name.as_str()) {
                skip_until = Some(md_static_tag(&name));
                skip_depth = 0;
                i += rel_end + 1;
                continue;
            }

            if name == "pre" {
                if is_close {
                    if pre_depth > 0 {
                        pre_depth -= 1;
                        if pre_depth == 0 {
                            flush_pre_block(&mut out, &mut pre_buffer);
                        }
                    }
                } else if !self_closing {
                    pre_depth += 1;
                }
                i += rel_end + 1;
                continue;
            }
            if pre_depth > 0 {
                // Inside a fenced block every other tag is noise: `<span>`
                // syntax highlighting must not become Markdown.
                i += rel_end + 1;
                continue;
            }

            emit_markdown_tag(
                &mut out,
                &name,
                tag_raw,
                is_close,
                &mut links,
                &mut list_depth,
                &mut emphasis,
            );
            i += rel_end + 1;
            continue;
        }

        if skip_until.is_some() {
            i += 1;
            continue;
        }
        if cdata_depth > 0 && html[i..].starts_with("]]>") {
            cdata_depth -= 1;
            i += "]]>".len();
            continue;
        }
        if bytes[i] == b'&'
            && let Some((decoded, consumed)) = decode_entity(&html[i..])
        {
            if pre_depth > 0 {
                pre_buffer.push_str(&decoded);
            } else {
                push_markdown_text(&mut out, &decoded);
            }
            i += consumed;
            continue;
        }
        let ch = html[i..].chars().next().unwrap();
        if pre_depth > 0 {
            pre_buffer.push(ch);
        } else {
            push_markdown_text_char(&mut out, ch);
        }
        i += ch.len_utf8();
    }

    // Unterminated `<pre>`: still emit whatever was captured instead of
    // silently dropping it, matching this module's tolerance elsewhere for
    // malformed/truncated input.
    if pre_depth > 0 && !pre_buffer.is_empty() {
        flush_pre_block(&mut out, &mut pre_buffer);
    }

    collapse_markdown(&out)
}

/// Emit a buffered `<pre>` body as a fenced block, choosing a fence longer
/// than any backtick run the body itself contains so embedded ``` content
/// (a documentation example, say) can't prematurely close the block.
fn flush_pre_block(out: &mut String, pre_buffer: &mut String) {
    let fence_len = longest_backtick_run(pre_buffer).max(2) + 1;
    let fence = "`".repeat(fence_len);
    md_break(out);
    out.push_str(&fence);
    out.push('\n');
    out.push_str(pre_buffer.trim_end_matches('\n'));
    out.push('\n');
    out.push_str(&fence);
    out.push('\n');
    pre_buffer.clear();
}

fn longest_backtick_run(s: &str) -> usize {
    let mut max_run = 0usize;
    let mut run = 0usize;
    for b in s.bytes() {
        if b == b'`' {
            run += 1;
            max_run = max_run.max(run);
        } else {
            run = 0;
        }
    }
    max_run
}

/// Push a literal HTML text-node character into Markdown output, escaping
/// characters that would otherwise be read as Markdown syntax the source
/// document never intended: ordinary page text like "# Title" or
/// "[not a link]" must not turn into a heading or a link just because it
/// passed through the converter. Never called for `<pre>` content, which is
/// pushed to `pre_buffer` verbatim (escaping code would corrupt it), or for
/// the markers this module emits itself (those go through direct
/// `push_str` calls, not this function).
fn push_markdown_text_char(out: &mut String, ch: char) {
    let at_line_start = out.is_empty() || out.ends_with('\n');
    let needs_escape = matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']')
        || (at_line_start && matches!(ch, '#' | '-' | '+' | '>'));
    if needs_escape {
        out.push('\\');
    }
    out.push(ch);
}

fn push_markdown_text(out: &mut String, s: &str) {
    for ch in s.chars() {
        push_markdown_text_char(out, ch);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_markdown_tag(
    out: &mut String,
    name: &str,
    tag_raw: &str,
    is_close: bool,
    links: &mut Vec<(usize, String)>,
    list_depth: &mut usize,
    emphasis: &mut Vec<(&'static str, usize)>,
) {
    match name {
        "br" => out.push('\n'),
        "title" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            md_break(out);
            if !is_close {
                let level = match name {
                    "title" | "h1" => 1,
                    other => other[1..].parse::<usize>().unwrap_or(1),
                };
                out.push_str(&"#".repeat(level));
                out.push(' ');
            }
        }
        "ul" | "ol" => {
            *list_depth = if is_close {
                list_depth.saturating_sub(1)
            } else {
                *list_depth + 1
            };
            md_break(out);
        }
        "li" => {
            if !is_close {
                md_break(out);
                out.push_str(&"  ".repeat(list_depth.saturating_sub(1)));
                out.push_str("- ");
            }
        }
        "b" | "strong" => md_toggle_emphasis(out, emphasis, is_close, "**"),
        "i" | "em" => md_toggle_emphasis(out, emphasis, is_close, "*"),
        "code" => md_toggle_emphasis(out, emphasis, is_close, "`"),
        "a" => {
            if is_close {
                md_close_link(out, links);
            } else if let Some(href) = md_attribute(tag_raw, "href") {
                links.push((out.len(), href));
            }
        }
        "img" => {
            if let Some(alt) = md_attribute(tag_raw, "alt").filter(|a| !a.trim().is_empty()) {
                // Caption yes, source no: an `img` src is either a URL the
                // model can't read or a multi-KB base64 blob.
                out.push_str("[IMAGE: ");
                push_markdown_text(out, alt.trim());
                out.push(']');
            }
        }
        other if MD_BLOCK_TAGS.contains(&other) => {
            // A block boundary right after a still-pending list marker (no
            // text emitted between "- " and this tag) must not push the
            // marker onto its own line, or the marker-only line then reads
            // as leftover markup and gets dropped by `collapse_markdown`,
            // silently losing the bullet. `<li><p>one</p></li>` must stay
            // `- one`.
            if !out.ends_with("- ") {
                md_break(out);
            }
        }
        other if MD_INLINE_TAGS.contains(&other) => {}
        _ => {
            // Unrecognised tag: separate sibling element values (RSS
            // `<guid>`/`<pubDate>`) instead of concatenating them.
            if !out.is_empty() && !out.ends_with(|c: char| c.is_whitespace()) {
                out.push(' ');
            }
        }
    }
}

/// Open or close one emphasis marker, tracking the stack of currently-open
/// markers rather than inferring direction from whatever `out` ends with.
/// The previous suffix-based approach broke both on nested same-type
/// emphasis (`<strong>outer <strong>inner</strong></strong>` — the outer
/// close saw the inner close's own marker and deleted it) and on literal
/// marker text in content (`<strong>foo**</strong>` truncated the literal
/// `**` instead of the marker). `push_markdown_text_char` already escapes
/// literal marker characters in text nodes, so `out` only ever ends with a
/// marker when this function put it there.
fn md_toggle_emphasis(
    out: &mut String,
    emphasis: &mut Vec<(&'static str, usize)>,
    is_close: bool,
    marker: &'static str,
) {
    if is_close {
        let Some(pos) = emphasis.iter().rposition(|(m, _)| *m == marker) else {
            return;
        };
        let (_, start) = emphasis.remove(pos);
        if start > out.len() {
            return;
        }
        // An empty element (`<b></b>`, or one whose only content was itself
        // dropped) would otherwise leave a stray `**` behind.
        if out.len() == start + marker.len() && &out[start..] == marker {
            out.truncate(start);
        } else {
            out.push_str(marker);
        }
    } else {
        emphasis.push((marker, out.len()));
        out.push_str(marker);
    }
}

fn md_break(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn md_close_link(out: &mut String, links: &mut Vec<(usize, String)>) {
    let Some((start, href)) = links.pop() else {
        return;
    };
    if start > out.len() {
        return;
    }
    let raw = out[start..].to_string();
    let text = raw.trim();
    // A link with no text, or one pointing at a fragment or a script
    // handler, is navigation chrome: keep the words, drop the wrapper.
    if text.is_empty() || !md_useful_href(&href) {
        return;
    }
    // Whitespace separating the link from adjacent text belongs outside the
    // wrapper, or `<p>Hello<a> world </a>again</p>` loses the separation
    // and renders as `Helloworldagain`.
    let leading_len = raw.len() - raw.trim_start().len();
    let trailing_len = raw.len() - raw.trim_end().len();
    let leading = raw[..leading_len].to_string();
    let trailing = raw[raw.len() - trailing_len..].to_string();
    let text = text.to_string();
    out.truncate(start);
    out.push_str(&leading);
    out.push('[');
    out.push_str(&text);
    out.push_str("](");
    out.push_str(&md_format_href(&href));
    out.push(')');
    out.push_str(&trailing);
}

fn md_useful_href(href: &str) -> bool {
    let h = href.trim();
    if h.is_empty() || h.starts_with('#') || h.len() > MD_MAX_HREF_CHARS {
        return false;
    }
    let lower = h.to_ascii_lowercase();
    !(lower.starts_with("javascript:") || lower.starts_with("data:"))
}

/// Query-parameter names commonly used to carry a bearer credential rather
/// than a resource identifier. Redacted before a link target reaches the
/// output, matching this crate's existing policy of never putting a full
/// URL with its query string into anything a model, log, or cache can see
/// (`web_extract::source_host` keeps only the host for the same reason).
const MD_SENSITIVE_QUERY_PARAMS: &[&str] = &[
    "token",
    "access_token",
    "refresh_token",
    "id_token",
    "auth",
    "authorization",
    "session",
    "sessionid",
    "session_id",
    "sid",
    "api_key",
    "apikey",
    "key",
    "secret",
    "client_secret",
    "password",
    "passwd",
    "pwd",
    "credential",
    "signature",
    "sig",
    "jwt",
];

/// Redact the value of any sensitive query parameter in `href`. Structure
/// (host, path, non-sensitive params, fragment) is kept so the target is
/// still meaningfully a "somewhere to go next" for the model, but a
/// `?token=...`/`?session=...` value never reaches the output.
fn md_redact_href(href: &str) -> std::borrow::Cow<'_, str> {
    let Some(q_pos) = href.find('?') else {
        return href.into();
    };
    let (base, rest) = href.split_at(q_pos);
    let rest = &rest[1..];
    let (query, fragment) = match rest.find('#') {
        Some(h) => (&rest[..h], &rest[h..]),
        None => (rest, ""),
    };
    let mut changed = false;
    let redacted: Vec<String> = query
        .split('&')
        .map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next().unwrap_or("");
            if parts.next().is_none() {
                return pair.to_string();
            }
            let sensitive = MD_SENSITIVE_QUERY_PARAMS
                .iter()
                .any(|p| p.eq_ignore_ascii_case(key));
            if sensitive {
                changed = true;
                format!("{key}=REDACTED")
            } else {
                pair.to_string()
            }
        })
        .collect();
    if !changed {
        return href.into();
    }
    let mut out = String::with_capacity(href.len());
    out.push_str(base);
    out.push('?');
    out.push_str(&redacted.join("&"));
    out.push_str(fragment);
    out.into()
}

/// Redact secrets from a link target, then guard against the destination
/// itself breaking the `(...)` it is about to sit inside: a target
/// containing a literal space or parenthesis is wrapped in angle brackets,
/// which Markdown treats as an unambiguous destination delimiter.
fn md_format_href(href: &str) -> String {
    let redacted = md_redact_href(href);
    if redacted.contains(' ') || redacted.contains('(') || redacted.contains(')') {
        format!("<{redacted}>")
    } else {
        redacted.into_owned()
    }
}

/// Return the `'static` slice matching a recognised Markdown drop-body tag.
fn md_static_tag(name: &str) -> &'static str {
    MD_DROP_BODY_TAGS
        .iter()
        .copied()
        .find(|t| *t == name)
        .unwrap_or("script")
}

/// Pull a quoted (or bare) attribute value out of a tag's interior.
fn md_attribute(tag_raw: &str, name: &str) -> Option<String> {
    let lower = tag_raw.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find(name) {
        let at = from + rel;
        // Must be preceded by whitespace and followed by `=`, so `href`
        // doesn't match inside `data-href` or a stray text run.
        let before_ok = at == 0 || tag_raw[..at].ends_with(char::is_whitespace);
        let rest = tag_raw[at + name.len()..].trim_start();
        if before_ok && let Some(value) = rest.strip_prefix('=') {
            let value = value.trim_start();
            let raw = match value.chars().next() {
                Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or(""),
                _ => value
                    .split(|c: char| c.is_whitespace() || c == '>')
                    .next()
                    .unwrap_or(""),
            };
            return Some(decode_all_entities(raw));
        }
        from = at + name.len();
    }
    None
}

/// Decode every entity in a string (attribute values are short; the scanner
/// decodes inline as it goes, but attributes are extracted whole).
fn decode_all_entities(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0usize;
    while i < raw.len() {
        if raw.as_bytes()[i] == b'&'
            && let Some((decoded, consumed)) = decode_entity(&raw[i..])
        {
            out.push_str(&decoded);
            i += consumed;
            continue;
        }
        let ch = raw[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Collapse the whitespace markup leaves behind, preserving the two things
/// Markdown encodes in it: fenced-block contents and list indentation.
fn collapse_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks = 0usize;
    // The exact fence text (e.g. "```" or "````") that opened the current
    // fenced block, not just "starts with 3 backticks": a `<pre>` body can
    // itself contain a line of bare backticks (a documentation example
    // showing a fenced block), and only a line matching the real fence
    // exactly may close it. `flush_pre_block` already chose that fence to
    // be longer than anything in the body, so an embedded bare-backtick
    // line can never collide with it.
    let mut fence_marker: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(marker) = &fence_marker {
            out.push_str(line.trim_end());
            out.push('\n');
            if trimmed == marker {
                fence_marker = None;
                blanks = 0;
            }
            continue;
        }
        if trimmed.len() >= 3 && trimmed.chars().all(|c| c == '`') {
            fence_marker = Some(trimmed.to_string());
            out.push_str(line.trim_end());
            out.push('\n');
            blanks = 0;
            continue;
        }

        let body = line.trim_start();
        let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
        // A bullet or heading marker with nothing after it is a leftover
        // from stripped markup, not content.
        if collapsed.is_empty() {
            blanks += 1;
            if blanks <= 1 {
                out.push('\n');
            }
            continue;
        }
        if collapsed == "-" || collapsed.chars().all(|c| c == '#') {
            continue;
        }
        blanks = 0;
        // List indentation is meaning, not stray whitespace.
        if body.starts_with("- ") {
            out.push_str(&line[..line.len() - body.len()]);
        }
        out.push_str(&collapsed);
        out.push('\n');
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CcrStore, MemoryCcrStore};
    use crate::pipeline::PipelineInput;

    #[test]
    fn strips_tags_and_scripts() {
        let html = "<html><head><style>.a{color:red}</style></head><body>\
            <script>alert('x')</script><h1>Title</h1><p>Hello <b>world</b>.</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
        assert!(
            !text.contains("alert"),
            "script body must be dropped: {text}"
        );
        assert!(!text.contains("color:red"), "style body must be dropped");
    }

    #[test]
    fn stray_lt_in_script_body_does_not_swallow_document() {
        // `a<b` inside the script must not be parsed as a tag whose end is
        // the `>` of `</script>` — that would leave skip mode armed forever
        // and drop everything after the script.
        let html = "<body><script>if(a<b){run()}</script><h1>Title</h1><p>Body text.</p></body>";
        let text = html_to_text(html);
        assert!(text.contains("Title"), "content after script lost: {text}");
        assert!(text.contains("Body text."), "{text}");
        assert!(!text.contains("run()"), "script body leaked: {text}");
    }

    #[test]
    fn stray_lt_in_style_and_uppercase_close_tag() {
        let html = "<style>a{width:calc(1<2?1px:2px)}</style><p>kept</p>\
            <SCRIPT>x<y</SCRIPT><p>also kept</p>";
        let text = html_to_text(html);
        assert!(text.contains("kept"), "{text}");
        assert!(text.contains("also kept"), "{text}");
        assert!(!text.contains("calc"), "{text}");
    }

    #[test]
    fn non_matching_close_tag_inside_script_stays_dropped() {
        let html = "<script>document.write('</b>')</script><p>after</p>";
        let text = html_to_text(html);
        assert!(text.contains("after"), "{text}");
        assert!(!text.contains("document.write"), "{text}");
    }

    #[test]
    fn decodes_entities() {
        let text = html_to_text("<p>a &amp; b &lt; c &gt; d &nbsp;e</p>");
        assert!(text.contains("a & b < c > d"), "{text}");
    }

    #[test]
    fn decodes_numeric_entities() {
        let text = html_to_text("<p>em&#8212;dash it&#x27;s &#169;</p>");
        assert!(text.contains("em—dash"), "{text}");
        assert!(text.contains("it's"), "{text}");
        assert!(text.contains("©"), "{text}");
        // Malformed references pass through as text rather than panicking.
        let text = html_to_text("<p>&#xZZ; &#; &#999999999;</p>");
        assert!(text.contains("&#xZZ;"), "{text}");
    }

    #[test]
    fn title_survives_head() {
        let html = "<html><head><title>Deploy Status — prod</title>\
            <meta charset=\"utf-8\"><style>.x{}</style></head>\
            <body><p>body text</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Deploy Status"), "title dropped: {text}");
        assert!(text.contains("body text"), "{text}");
        assert!(!text.contains(".x{}"), "style leaked: {text}");
    }

    #[test]
    fn block_tags_insert_newlines() {
        let text = html_to_text("<p>one</p><p>two</p><li>three</li>");
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        assert!(lines.len() >= 3, "expected separate lines, got {lines:?}");
    }

    #[test]
    fn gt_inside_quoted_attribute_does_not_split_tag() {
        // Discourse ships `<link media="(width >= 40rem)" ...>`; the `>` in
        // `>=` must not terminate the tag and leak the tail as text.
        let html = "<head><link href=\"a.css\" media=\"(width >= 40rem)\" \
            rel=\"stylesheet\" data-target=\"desktop\" /></head><body><p>real text</p></body>";
        let text = html_to_text(html);
        assert!(text.contains("real text"), "{text}");
        assert!(!text.contains("40rem"), "attribute leaked: {text}");
        assert!(!text.contains("stylesheet"), "attribute leaked: {text}");
        // `<` inside quotes must also be harmless.
        let text = html_to_text("<link media=\"(width < 40rem)\" /><p>kept</p>");
        assert!(text.contains("kept"), "{text}");
        assert!(!text.contains("40rem"), "{text}");
    }

    #[test]
    fn unterminated_quote_falls_back_and_terminates() {
        // A quote that never closes must not hang or swallow the document:
        // fall back to the next `>` and keep going.
        let text = html_to_text("<a href=\"broken>after</a> tail");
        assert!(text.contains("after"), "{text}");
        assert!(text.contains("tail"), "{text}");
        // Unterminated quote with no `>` at all: stop cleanly.
        let text = html_to_text("before<a href=\"never closed");
        assert!(text.contains("before"), "{text}");
    }

    #[test]
    fn cdata_delimiters_are_stripped_payload_kept() {
        let html = "<item><title><![CDATA[Big <b>payout</b> story]]></title>\
            <pubDate>Sun, 05 Jul 2026</pubDate></item>";
        let text = html_to_text(html);
        assert!(!text.contains("CDATA"), "{text}");
        assert!(!text.contains("]]>"), "CDATA closer leaked: {text}");
        assert!(text.contains("Big"), "{text}");
        assert!(text.contains("payout"), "{text}");
        assert!(
            !text.contains("<b>"),
            "markup in CDATA not stripped: {text}"
        );
        assert!(text.contains("Sun, 05 Jul 2026"), "{text}");
        // Unterminated CDATA: payload still emitted, no hang.
        let text = html_to_text("<title><![CDATA[open ended");
        assert!(text.contains("open ended"), "{text}");
    }

    #[test]
    fn rss_sibling_elements_are_separated() {
        let html = "<item><guid>https://news.ycombinator.com/item?id=48793726</guid>\
            <comments>https://news.ycombinator.com/item?id=48793726</comments>\
            <dc:creator>alice</dc:creator></item>";
        let text = html_to_text(html);
        assert!(
            !text.contains("48793726https"),
            "sibling values ran together: {text}"
        );
        assert!(!text.contains("48793726alice"), "{text}");
        // Inline tags still don't split words.
        let text = html_to_text("<p>Hello <b>world</b>.</p>");
        assert!(text.contains("Hello world."), "{text}");
    }

    #[test]
    fn compress_shrinks_real_doc() {
        let mut html = String::from("<html><body>");
        for i in 0..50 {
            html.push_str(&format!(
                "<div class=\"row item-{i}\"><span>cell {i}</span></div>"
            ));
        }
        html.push_str("</body></html>");
        let out = compress(&html).expect("compresses");
        // Extraction is an information-preserving reshape, not a drop: every
        // cell's text survives, so it reports as a faithful reformat and ships
        // without needing CCR recovery.
        assert!(!out.lossy, "html extraction is a faithful reshape");
        assert!(out.text.len() < html.len());
        assert!(out.text.contains("cell 7"));
        for i in 0..50 {
            assert!(out.text.contains(&format!("cell {i}")), "cell {i} kept");
        }
    }

    // --- html_to_markdown -------------------------------------------------

    #[test]
    fn markdown_drops_script_and_style_bodies_entirely() {
        let html = r#"<html><head><style>body{color:red}</style></head>
            <body><script>var x = 1 < 2 && 3 > 2;</script><p>Hello</p></body></html>"#;
        let md = html_to_markdown(html);
        assert_eq!(md, "Hello");
        assert!(!md.contains("color"), "style body leaked: {md}");
        assert!(!md.contains("var x"), "script body leaked: {md}");
    }

    #[test]
    fn markdown_keeps_headings_at_their_level() {
        let html = "<h1>Title</h1><p>Intro.</p><h3>Detail</h3><p>Body.</p>";
        assert_eq!(html_to_markdown(html), "# Title\nIntro.\n### Detail\nBody.");
    }

    #[test]
    fn markdown_keeps_the_document_title_as_a_top_level_heading() {
        let html = "<html><head><title>Rust Docs</title></head><body><p>x</p></body></html>";
        assert!(html_to_markdown(html).starts_with("# Rust Docs"));
    }

    #[test]
    fn markdown_keeps_link_targets_so_a_model_can_follow_them() {
        let html = r#"<p>See <a href="https://example.com/spec">the spec</a> for more.</p>"#;
        assert_eq!(
            html_to_markdown(html),
            "See [the spec](https://example.com/spec) for more."
        );
    }

    #[test]
    fn markdown_unwraps_navigation_chrome_links_but_keeps_their_words() {
        for href in ["#", "#section-2", "javascript:void(0)", "data:text/html,x"] {
            let html = format!(r#"<p>Go <a href="{href}">here</a> now.</p>"#);
            assert_eq!(html_to_markdown(&html), "Go here now.", "href={href}");
        }
    }

    #[test]
    fn markdown_drops_an_overlong_tracking_url_rather_than_paying_for_it() {
        let href = format!("https://e.com/?{}", "utm=x&".repeat(100));
        assert!(href.len() > MD_MAX_HREF_CHARS);
        assert_eq!(
            html_to_markdown(&format!(r#"<a href="{href}">click</a>"#)),
            "click"
        );
    }

    #[test]
    fn markdown_nests_list_items_by_depth() {
        let html = "<ul><li>one</li><li>two<ul><li>inner</li></ul></li></ul>";
        assert_eq!(html_to_markdown(html), "- one\n- two\n  - inner");
    }

    #[test]
    fn markdown_fences_pre_blocks_and_keeps_their_indentation() {
        let html = "<pre><code>fn main() {\n    println!(\"hi\");\n}</code></pre>";
        assert_eq!(
            html_to_markdown(html),
            "```\nfn main() {\n    println!(\"hi\");\n}\n```"
        );
    }

    #[test]
    fn markdown_keeps_emphasis() {
        let html = "<p><strong>bold</strong> and <em>italic</em> and <code>lit</code></p>";
        assert_eq!(html_to_markdown(html), "**bold** and *italic* and `lit`");
    }

    #[test]
    fn markdown_keeps_an_image_caption_and_never_its_base64_payload() {
        let html = r#"<p><img alt="A chart" src="data:image/png;base64,AAAAAAAAAAAA"></p>"#;
        let md = html_to_markdown(html);
        assert_eq!(md, "[IMAGE: A chart]");
        assert!(!md.contains("base64"), "data URI leaked: {md}");
    }

    #[test]
    fn markdown_ignores_an_image_with_no_alt_text() {
        assert_eq!(html_to_markdown(r#"<p>a<img src="/x.png">b</p>"#), "ab");
    }

    #[test]
    fn markdown_decodes_entities_including_numeric_and_hex_forms() {
        let html = "<p>A &amp; B &lt;tag&gt; &#39;q&#39; &#x2014; &nbsp;done&hellip;</p>";
        assert_eq!(html_to_markdown(html), "A & B <tag> 'q' — done…");
    }

    #[test]
    fn markdown_does_not_mistake_a_data_href_attribute_for_href() {
        assert_eq!(
            html_to_markdown(r#"<a data-href="/wrong" href="/right">t</a>"#),
            "[t](/right)"
        );
    }

    #[test]
    fn markdown_reads_unquoted_attribute_values() {
        assert_eq!(html_to_markdown("<a href=/plain>t</a>"), "[t](/plain)");
    }

    #[test]
    fn markdown_tolerates_a_greater_than_inside_a_quoted_attribute() {
        let html = r#"<div media="(width >= 40rem)"><p>kept</p></div>"#;
        assert_eq!(html_to_markdown(html), "kept");
    }

    #[test]
    fn markdown_does_not_emit_markup_from_inside_a_fenced_block() {
        let html = "<pre><span class=\"k\">let</span> x = 1;</pre>";
        assert_eq!(html_to_markdown(html), "```\nlet x = 1;\n```");
    }

    #[test]
    fn markdown_shrinks_a_markup_heavy_page_by_an_order_of_magnitude() {
        // The property that matters for cost: a page whose bytes are mostly
        // machinery must come out near the size of its prose.
        let mut html = String::from("<html><head>");
        for i in 0..200 {
            html.push_str(&format!(
                "<script>function f{i}(){{return {i}*2;}}</script>"
            ));
        }
        html.push_str("</head><body>");
        for _ in 0..10 {
            html.push_str("<div class=\"a b c\"><span>Real sentence of prose.</span></div>");
        }
        html.push_str("</body></html>");

        let md = html_to_markdown(&html);
        assert!(
            md.len() * 10 < html.len(),
            "expected >10x shrink, got {} -> {}",
            html.len(),
            md.len()
        );
        assert!(md.contains("Real sentence of prose."));
        assert!(!md.contains("return"));
    }

    #[test]
    fn markdown_treats_embed_as_a_void_element() {
        // `embed` has no closing tag in real HTML. Treating it like the
        // other drop-body tags arms skip-until-EOF and silently discards
        // everything after it.
        let html = r#"<embed src="movie.mp4"><h1>Details</h1><p>Body text.</p>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("# Details"), "{md}");
        assert!(md.contains("Body text."), "{md}");
    }

    #[test]
    fn markdown_tracks_nesting_depth_of_dropped_elements() {
        let html = "<template><template>inner</template>secret</template><p>after</p>";
        let md = html_to_markdown(html);
        assert!(!md.contains("secret"), "{md}");
        assert!(!md.contains("inner"), "{md}");
        assert!(md.contains("after"), "{md}");
    }

    #[test]
    fn markdown_escapes_markdown_syntax_in_text_nodes() {
        let html = "<p># This is paragraph text</p><p>- not a list item</p>\
            <p>See [internal](https://example.com) reference and a_var*b*c and `code`.</p>";
        let md = html_to_markdown(html);
        assert!(md.contains("\\# This is paragraph text"), "{md}");
        assert!(md.contains("\\- not a list item"), "{md}");
        assert!(
            md.contains("See \\[internal\\](https://example.com) reference"),
            "{md}"
        );
        assert!(md.contains("a\\_var\\*b\\*c and \\`code\\`."), "{md}");
        // Structural markers this module emits itself are untouched.
        let md = html_to_markdown("<h1>Title</h1><ul><li>item</li></ul>");
        assert_eq!(md, "# Title\n- item");
    }

    #[test]
    fn markdown_escaping_applies_to_text_nodes_only_and_leaves_emitted_syntax_valid() {
        // Text-node escaping (push_markdown_text) must never reach the
        // structural markers the emitters themselves push_str directly:
        // `[text](href)`, `## Heading`, `- bullet`, and a ``` fence. If
        // escaping ever migrated onto those call sites, the output would
        // stop parsing as the Markdown construct it claims to be.
        let html = concat!(
            r#"<h2>Section [1] * notes</h2>"#,
            r#"<p>See <a href="https://example.com/a_b">the [spec] *here*</a> for more.</p>"#,
            r#"<ul><li>item # one *important*</li><li>item [two]</li></ul>"#,
            r#"<pre>fn f() { let x = a * b; a[0] = 1; }</pre>"#,
        );
        let md = html_to_markdown(html);

        // Heading marker is a literal "## ", not escaped, and the text after
        // it still carries the escaped metacharacters.
        assert!(
            md.contains("## Section \\[1\\] \\* notes"),
            "heading marker must stay unescaped: {md}"
        );

        // The link wrapper `[...](...)` is intact — only the link text's
        // interior metacharacters are escaped, not the brackets/parens the
        // emitter itself wrote.
        assert!(
            md.contains("[the \\[spec\\] \\*here\\*](https://example.com/a_b)"),
            "link wrapper must stay valid and unescaped: {md}"
        );

        // List markers are literal "- ", not "\\- ", while item text is
        // escaped.
        // "#" is only escaped at line start (where it would read as a
        // heading); mid-line it needs no escaping to stay literal text.
        assert!(
            md.contains("- item # one \\*important\\*"),
            "list marker must stay unescaped: {md}"
        );
        assert!(
            md.contains("- item \\[two\\]"),
            "list marker must stay unescaped: {md}"
        );

        // The fenced block is untouched: no escaping inside `<pre>`, and the
        // fence delimiters themselves are the literal backtick run.
        assert!(
            md.contains("```\nfn f() { let x = a * b; a[0] = 1; }\n```"),
            "pre content and fence must stay unescaped: {md}"
        );
    }

    #[test]
    fn markdown_keeps_literal_comparison_operators_inside_pre() {
        let html = "<pre>if a < b && c > d</pre>";
        assert_eq!(html_to_markdown(html), "```\nif a < b && c > d\n```");
    }

    #[test]
    fn markdown_selects_a_fence_longer_than_embedded_backtick_runs() {
        let html = "<pre>```\nhello\n```</pre>";
        let md = html_to_markdown(html);
        assert_eq!(md, "````\n```\nhello\n```\n````");
    }

    #[test]
    fn markdown_terminates_the_closing_fence_with_a_newline() {
        let html = "<pre>x</pre>tail";
        let md = html_to_markdown(html);
        assert_eq!(md, "```\nx\n```\ntail");
    }

    #[test]
    fn markdown_keeps_a_list_marker_when_the_item_wraps_a_block_child() {
        let html = "<ul><li><p>one</p></li></ul>";
        assert_eq!(html_to_markdown(html), "- one");
    }

    #[test]
    fn markdown_handles_nested_emphasis_of_the_same_type() {
        let html = "<strong>outer <strong>inner</strong></strong>";
        assert_eq!(html_to_markdown(html), "**outer **inner****");
    }

    #[test]
    fn markdown_preserves_literal_marker_text_when_closing_emphasis() {
        let html = "<strong>foo**</strong>";
        assert_eq!(html_to_markdown(html), "**foo\\*\\***");
    }

    #[test]
    fn markdown_keeps_link_edge_whitespace_outside_the_wrapper() {
        let html = r#"<p>Hello<a href="/x"> world </a>again</p>"#;
        assert_eq!(html_to_markdown(html), "Hello [world](/x) again");
    }

    #[test]
    fn markdown_redacts_secrets_from_link_targets() {
        let html =
            r#"<a href="https://example.test/reset?token=secret&session=private&page=2">click</a>"#;
        let md = html_to_markdown(html);
        assert!(!md.contains("secret"), "{md}");
        assert!(!md.contains("private"), "{md}");
        assert!(md.contains("token=REDACTED"), "{md}");
        assert!(md.contains("session=REDACTED"), "{md}");
        assert!(md.contains("page=2"), "non-sensitive params survive: {md}");
    }

    #[test]
    fn markdown_wraps_link_destinations_with_spaces_or_parens_in_angle_brackets() {
        let html = r#"<a href="/a path (final)">click</a>"#;
        assert_eq!(html_to_markdown(html), "[click](</a path (final)>)");
    }

    #[test]
    fn html_extract_transform_requires_retained_ccr() {
        let mut html = String::from("<html><body>");
        for i in 0..50 {
            html.push_str(&format!(
                "<div class=\"row item-{i}\"><span>cell {i}</span></div>"
            ));
        }
        html.push_str("</body></html>");
        let input = PipelineInput {
            content: &html,
            original_content: &html,
            content_kind: ContentKind::Html,
            original_bytes: html.len(),
        };
        let transform = HtmlExtractTransform;

        let rejecting_store = MemoryCcrStore::new(1, 1);
        assert!(transform.apply(&input, &rejecting_store).is_none());

        let store = MemoryCcrStore::default();
        let out = transform.apply(&input, &store).expect("retained HTML");

        assert_eq!(out.kind(), CompressorKind::Html);
        assert!(out.text().contains("cell 7"), "{}", out.text());
        assert_eq!(store.get(out.token()).as_deref(), Some(html.as_str()));
    }

    #[test]
    fn html_extract_transform_skips_non_html_input() {
        let input = PipelineInput {
            content: "plain text",
            original_content: "plain text",
            content_kind: ContentKind::PlainText,
            original_bytes: "plain text".len(),
        };
        let transform = HtmlExtractTransform;
        let store = MemoryCcrStore::default();

        assert_eq!(transform.estimate_bloat(&input), 0.0);
        assert!(transform.apply(&input, &store).is_none());
    }
}
