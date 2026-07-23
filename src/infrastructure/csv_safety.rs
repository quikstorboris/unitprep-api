//! Guards against CSV/XLSX formula injection (CWE-1236): a cell value
//! beginning with `=`, `+`, `-`, `@`, tab, or carriage return is treated
//! as a live formula (or DDE command, for the older prefixes) by Excel
//! and similar spreadsheet apps when the file is opened. Export data
//! here ultimately comes from tenant/unit records in uploaded facility
//! files, and these exports are handed straight to facility managers to
//! open in Excel — a crafted field value must not become a live formula.
//! Prefixing with a leading apostrophe is the standard mitigation
//! (OWASP): it forces the cell to be read as literal text.

const RISKY_PREFIXES: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

pub fn sanitize_cell(value: &str) -> String {
    match value.chars().next() {
        Some(c) if RISKY_PREFIXES.contains(&c) => format!("'{value}"),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_ordinary_values_untouched() {
        assert_eq!(sanitize_cell("Jane Smith"), "Jane Smith");
        assert_eq!(sanitize_cell(""), "");
    }

    #[test]
    fn neutralizes_each_risky_prefix() {
        assert_eq!(sanitize_cell("=cmd|'/c calc'!A1"), "'=cmd|'/c calc'!A1");
        assert_eq!(sanitize_cell("+1234"), "'+1234");
        assert_eq!(sanitize_cell("-1234"), "'-1234");
        assert_eq!(sanitize_cell("@SUM(A1:A2)"), "'@SUM(A1:A2)");
        assert_eq!(sanitize_cell("\tHYPERLINK"), "'\tHYPERLINK");
        assert_eq!(sanitize_cell("\rHYPERLINK"), "'\rHYPERLINK");
    }
}
