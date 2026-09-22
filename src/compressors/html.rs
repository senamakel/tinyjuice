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
use crate::types::{CompressInput, CompressOptions, CompressOutput, CompressorKind};

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
    "address", "article", "aside", "blockquote", "dd", "div", "dl", "dt", "fieldset", "figcaption",
    "figure", "footer", "header", "hr", "main", "nav", "ol", "p", "section", "table", "tbody",
    "td", "tfoot", "th", "thead", "tr", "ul",
];

/// Inline tags that must not introduce a separator.
const MD_INLINE_TAGS: &[&str] = &[
    "a", "b", "i", "em", "strong", "span", "code", "small", "sub", "sup", "abbr", "cite", "q",
    "u", "s", "mark", "time", "var", "kbd", "samp", "label", "font",
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
    let mut cdata_depth = 0usize;
    // Open `<a>` elements: where their text began in `out`, and the href to
    // wrap it with on close.
    let mut links: Vec<(usize, String)> = Vec::new();
    let mut list_depth = 0usize;
    // Inside `<pre>` whitespace is content, so the normalizer must leave it
    // alone and inline markers must not fire.
    let mut pre_depth = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(skip_tag) = skip_until {
                // Inside a dropped body only the matching close tag is
                // markup; a lone `<` in inline JS or CSS is body text.
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
            let Some(rel_end) = find_tag_end(html, i) else {
                break;
            };
            let tag_raw = &html[i + 1..i + rel_end];
            let (name, is_close) = parse_tag_name(tag_raw);
            let self_closing = tag_raw.trim_end().ends_with('/');

            if !is_close && !self_closing && MD_DROP_BODY_TAGS.contains(&name.as_str()) {
                skip_until = Some(md_static_tag(&name));
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
                &mut pre_depth,
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
            out.push_str(&decoded);
            i += consumed;
            continue;
        }
        let ch = html[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    collapse_markdown(&out)
}

#[allow(clippy::too_many_arguments)]
fn emit_markdown_tag(
    out: &mut String,
    name: &str,
    tag_raw: &str,
    is_close: bool,
    links: &mut Vec<(usize, String)>,
    list_depth: &mut usize,
    pre_depth: &mut usize,
) {
    // Inside a fenced block every tag but `</pre>` is noise: `<span>`
    // syntax highlighting must not become Markdown.
    if *pre_depth > 0 && !(is_close && name == "pre") {
        return;
    }

    match name {
        "pre" => {
            if is_close {
                *pre_depth = pre_depth.saturating_sub(1);
                out.push_str("\n```");
            } else {
                *pre_depth += 1;
                md_break(out);
                out.push_str("```\n");
            }
        }
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
        "b" | "strong" => md_emphasis(out, "**"),
        "i" | "em" => md_emphasis(out, "*"),
        "code" => md_emphasis(out, "`"),
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
                out.push_str(alt.trim());
                out.push(']');
            }
        }
        other if MD_BLOCK_TAGS.contains(&other) => md_break(out),
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

/// Emphasis markers are only worth emitting around real text; an empty
/// `<b></b>` would otherwise leave `****` behind.
fn md_emphasis(out: &mut String, marker: &str) {
    if out.ends_with(marker) {
        out.truncate(out.len() - marker.len());
        return;
    }
    out.push_str(marker);
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
    let text = out[start..].trim().to_string();
    // A link with no text, or one pointing at a fragment or a script
    // handler, is navigation chrome: keep the words, drop the wrapper.
    if text.is_empty() || !md_useful_href(&href) {
        return;
    }
    out.truncate(start);
    out.push('[');
    out.push_str(&text);
    out.push_str("](");
    out.push_str(&href);
    out.push(')');
}

fn md_useful_href(href: &str) -> bool {
    let h = href.trim();
    if h.is_empty() || h.starts_with('#') || h.len() > MD_MAX_HREF_CHARS {
        return false;
    }
    let lower = h.to_ascii_lowercase();
    !(lower.starts_with("javascript:") || lower.starts_with("data:"))
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
    let mut in_fence = false;

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push_str(line.trim_end());
            out.push('\n');
            blanks = 0;
            continue;
        }
        if in_fence {
            out.push_str(line.trim_end());
            out.push('\n');
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
        assert_eq!(html_to_markdown(&format!(r#"<a href="{href}">click</a>"#)), "click");
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
            html.push_str(&format!("<script>function f{i}(){{return {i}*2;}}</script>"));
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
}
