//! Parses Process Street's free-text "Owner/District Manager/Manager
//! Level Users" blocks into individual records.
//!
//! Real production data uses at least two genuinely different formats,
//! both confirmed this session:
//! - **Comma-separated, one line per person** (Beau Ryan's facilities):
//!   `"Beau Ryan, beau@rockspring.com, 832-978-3228"`, one such line per
//!   person, no blank lines between them.
//! - **Multi-line per person, blank-line separated** (Prairie
//!   Enterprises' Highway 20): a name on its own line, then an email
//!   and/or phone each on their own line (sometimes prefixed
//!   `"Primary: "`), a blank line, then the next person.
//!
//! Boris's own framing: "I would comb through a healthy sample of PS's
//! various clients' forms to establish a pattern" -- this is that
//! comb-through's first real finding, not a hypothetical. This parser
//! detects which format a given chunk of text uses (by whether its
//! lines contain commas) rather than assuming one universally.

// Phase 1 only -- no HTTP handler calls into `clients::*` yet. Remove
// once a real caller exists.
#![allow(dead_code)]

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPerson {
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
}

/// Groups lines into "chunks" separated by one or more blank lines.
/// A chunk with no internal blank lines but multiple people
/// (comma-separated format) stays as one chunk here -- format
/// detection happens per-chunk in `parse_people_block`, not here.
fn split_into_chunks(raw: &str) -> Vec<Vec<&str>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
        } else {
            current.push(trimmed);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// One line of `"Name, email, phone"` -- best-effort, a line with fewer
/// than 3 comma-separated parts still yields a person with whatever it
/// has.
fn parse_comma_line(line: &str) -> Option<ParsedPerson> {
    let mut parts = line.split(',').map(str::trim);
    let full_name = parts.next().unwrap_or_default().to_string();
    if full_name.is_empty() {
        return None;
    }
    let email = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
    let phone = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
    Some(ParsedPerson {
        full_name,
        email,
        phone,
    })
}

/// A whole chunk (multiple lines, no commas) as one person: first line
/// is the name, the first line containing `@` is the email (stripping
/// any `"Label: "` prefix via the text after the last `:`), the first
/// remaining line with at least 7 digits and no `@` is the phone.
fn parse_multiline_record(lines: &[&str]) -> Option<ParsedPerson> {
    let (name, rest) = lines.split_first()?;
    if name.is_empty() {
        return None;
    }

    let email = rest
        .iter()
        .find(|l| l.contains('@'))
        .map(|l| l.rsplit(':').next().unwrap_or(l).trim().to_string());

    let phone = rest
        .iter()
        .find(|l| !l.contains('@') && l.chars().filter(char::is_ascii_digit).count() >= 7)
        .map(|l| l.trim().to_string());

    Some(ParsedPerson {
        full_name: name.to_string(),
        email,
        phone,
    })
}

pub fn parse_people_block(raw: &str) -> Vec<ParsedPerson> {
    let mut people = Vec::new();
    for chunk in split_into_chunks(raw) {
        if chunk.iter().all(|line| line.contains(',')) {
            people.extend(chunk.iter().filter_map(|line| parse_comma_line(line)));
        } else if let Some(person) = parse_multiline_record(&chunk) {
            people.push(person);
        }
    }
    people
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_comma_separated_two_person_block() {
        // Real (non-sensitive) text captured from Beau Ryan's facilities this session.
        let raw = "Beau Ryan, beau@rockspring.com, 832-978-3228\n\
                   Brad Ryan, bryan@capitalrp.com, 281-222-7946";
        let people = parse_people_block(raw);
        assert_eq!(people.len(), 2);
        assert_eq!(people[0].full_name, "Beau Ryan");
        assert_eq!(people[0].email.as_deref(), Some("beau@rockspring.com"));
        assert_eq!(people[0].phone.as_deref(), Some("832-978-3228"));
        assert_eq!(people[1].full_name, "Brad Ryan");
    }

    #[test]
    fn parses_a_real_multiline_blank_line_separated_three_person_block() {
        // Real (non-sensitive) text captured from Prairie Enterprises'
        // Highway 20 facility this session -- note the "Primary: "
        // label prefix and a second, ignored email on Kyle's record.
        let raw = "Kyle Lindley \n\
                   Primary: k.lindley@prairie-enterprises.com\n\
                   kyle.lindley@outlook.com\n\
                   630-650-0137 \n\
                   \n\
                   Juanita Fleener \n\
                   j.fleener@prairie-enterprises.com\n\
                   815-568-1307 \n\
                   \n\
                   Judy Armstrong\n\
                   j.armstrong@prairie-enterprises.com\n\
                   815-568-1307 ";
        let people = parse_people_block(raw);
        assert_eq!(people.len(), 3, "three blank-line-separated records must yield three people");
        assert_eq!(people[0].full_name, "Kyle Lindley");
        assert_eq!(people[0].email.as_deref(), Some("k.lindley@prairie-enterprises.com"));
        assert_eq!(people[0].phone.as_deref(), Some("630-650-0137"));
        assert_eq!(people[1].full_name, "Juanita Fleener");
        assert_eq!(people[2].full_name, "Judy Armstrong");
    }

    #[test]
    fn skips_blank_lines_and_trailing_whitespace() {
        let raw = "Beau Ryan, beau@rockspring.com, 832-978-3228\n\n   \n";
        assert_eq!(parse_people_block(raw).len(), 1);
    }

    #[test]
    fn handles_a_name_only_comma_line_without_email_or_phone() {
        let people = parse_people_block("Bre Alford");
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].full_name, "Bre Alford");
        assert_eq!(people[0].email, None);
        assert_eq!(people[0].phone, None);
    }

    #[test]
    fn an_empty_block_yields_no_people() {
        assert!(parse_people_block("").is_empty());
        assert!(parse_people_block("   \n  \n").is_empty());
    }
}
