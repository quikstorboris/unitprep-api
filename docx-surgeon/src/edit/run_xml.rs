use crate::read::RunSpan;

use super::fragment::RunStyle;

/// Rebuilds one run's whole `<w:r>...</w:r>` XML with `text` as its
/// content, reusing every other byte of the run's original structure
/// (its opening tag's own attributes, its `<w:t>` tag's own attributes,
/// its closing tags) verbatim. For an underlined fragment, the run's
/// `<w:rPr>` is cloned and given an underline override; a run with no
/// `<w:rPr>` at all gets a brand new one inserted.
pub(super) fn build_run_xml(
    document_xml: &str,
    run: &RunSpan,
    text: &str,
    style: RunStyle,
) -> String {
    let prefix = match style {
        RunStyle::Original => document_xml[run.run_start..run.t_open_start].to_string(),
        RunStyle::Underlined => underlined_run_prefix_xml(document_xml, run),
    };
    let t_open = &document_xml[run.t_open_start..run.xml_content_start];
    let t_and_r_close = &document_xml[run.xml_content_end..run.run_end];

    format!("{prefix}{t_open}{}{t_and_r_close}", xml_escape_text(text))
}

/// The run's own opening tag plus a `<w:rPr>` guaranteed to carry a
/// `<w:u w:val="single"/>` -- cloning and augmenting the original
/// `<w:rPr>` if the run has one, otherwise inserting a new, minimal one
/// right after the opening tag.
fn underlined_run_prefix_xml(document_xml: &str, run: &RunSpan) -> String {
    match run.rpr_range {
        Some((rpr_start, rpr_end)) => {
            let before_rpr = &document_xml[run.run_start..rpr_start];
            let rpr_xml = &document_xml[rpr_start..rpr_end];
            let after_rpr = &document_xml[rpr_end..run.t_open_start];
            format!(
                "{before_rpr}{}{after_rpr}",
                underline_run_props_xml(rpr_xml)
            )
        }
        None => {
            let opening_tag = &document_xml[run.run_start..run.t_open_start];
            format!("{opening_tag}<w:rPr><w:u w:val=\"single\"/></w:rPr>")
        }
    }
}

/// Returns `rpr_xml` (a whole `<w:rPr>...</w:rPr>` or self-closing
/// `<w:rPr/>`) with any existing `<w:u>` child removed and a
/// `<w:u w:val="single"/>` inserted as the *last* child. Last is
/// schema-safe for every `<w:rPr>` shape seen in this project's real
/// corpus so far (`sz`/`szCs` both sort *before* `u` in OOXML's
/// declared `CT_RPr` child sequence) -- a hypothetical run that already
/// overrides a later-sequenced property (`effect`, `bdr`, `shd`, ...)
/// is not specially handled, since none has been seen in practice yet.
fn underline_run_props_xml(rpr_xml: &str) -> String {
    let without_existing_underline = remove_element(rpr_xml, "<w:u");
    let underline_element = "<w:u w:val=\"single\"/>";

    if let Some(stripped) = without_existing_underline.strip_suffix("/>") {
        // Self-closing <w:rPr/> (no children) -- becomes a paired tag
        // with the underline as its only child.
        format!("{stripped}>{underline_element}</w:rPr>")
    } else if let Some(prefix) = without_existing_underline.strip_suffix("</w:rPr>") {
        format!("{prefix}{underline_element}</w:rPr>")
    } else {
        // Doesn't match either known <w:rPr> shape -- leave it
        // untouched rather than guess at one that doesn't apply here.
        without_existing_underline
    }
}

/// Strips any existing self-closing element whose tag starts with
/// `tag_prefix` (e.g. `"<w:u"` matches both `<w:u w:val="single"/>` and
/// the bare `<w:u/>` shorthand) out of `rpr_xml`. Word always writes
/// these run-property toggles self-closing (they never have children),
/// so a paired open/close form is not expected and not handled. Leaving
/// a stale one behind would either produce schema-invalid XML (two
/// sibling elements with the same tag name) or have the wrong one win,
/// depending on the consumer.
fn remove_element(rpr_xml: &str, tag_prefix: &str) -> String {
    let Some(start) = rpr_xml.find(tag_prefix) else {
        return rpr_xml.to_string();
    };
    let Some(relative_end) = rpr_xml[start..].find("/>") else {
        return rpr_xml.to_string();
    };
    let end = start + relative_end + "/>".len();

    let mut result = String::with_capacity(rpr_xml.len());
    result.push_str(&rpr_xml[..start]);
    result.push_str(&rpr_xml[end..]);
    result
}

pub(super) fn xml_escape_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(c),
        }
    }
    escaped
}
