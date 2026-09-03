//! Parses Process Street's free-text "Owner/District Manager/Manager
//! Level Users" blocks into individual records.
//!
//! Real production data uses at least three genuinely different
//! formats, all confirmed this session:
//! - **Comma-separated, one line per person** (Beau Ryan's facilities):
//!   `"Beau Ryan, beau@rockspring.com, 832-978-3228"`, one such line per
//!   person, no blank lines between them.
//! - **Multi-line per person, blank-line separated** (Prairie
//!   Enterprises' Highway 20): a name on its own line, then an email
//!   and/or phone each on their own line (sometimes prefixed
//!   `"Primary: "`), a blank line, then the next person.
//! - **Dash-separated name, comma-separated contact info** (a real
//!   single-facility business, run rZFNRpmLIxuOrb_8K9hICw):
//!   `"Irene Chen - (301) 787-9221, irene@chenlawgroup.com"` -- and,
//!   critically, the *next* person on the very same field reverses the
//!   order: `"Amanda Ibarra - chchenpropertymgmtteam1@gmail.com,
//!   (423) 314-2096"` (email before phone this time). A naive
//!   assume-the-second-comma-slot-is-phone parser silently glues the
//!   dash-separated contact value onto the name and mis-files whichever
//!   value happens to land in the wrong slot -- confirmed as the actual
//!   cause of this facility's people never turning up in person search
//!   (`clients.ps_person_index` held "Irene Chen - (301) 787-9221" as a
//!   literal name with no phone, and a phone number sitting in the
//!   *email* column for Amanda). `parse_comma_line` below content-sniffs
//!   every candidate value (does it contain `@`? does it have enough
//!   digits?) rather than assuming a fixed position, specifically so
//!   this kind of per-person order flip within the same field doesn't
//!   need a fourth special case.
//!
//! Boris's own framing: "I would comb through a healthy sample of PS's
//! various clients' forms to establish a pattern" -- this is that
//! comb-through's third real finding, not a hypothetical. This parser
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

/// One line of `"Name, email, phone"` -- or `"Name - phone, email"` /
/// `"Name - email, phone"`, the real dash-separated variant documented
/// in this module's own doc comment. Content-sniffed rather than
/// positional past the name: whichever candidate value contains `@` is
/// the email, whichever has at least 7 digits and no `@` is the phone,
/// regardless of which comma slot either landed in -- real data has
/// been seen alternating that order within the very same field.
/// Best-effort throughout: a line with fewer parts, or a dash-prefixed
/// name with no real contact info after it, still yields a person with
/// whatever it has.
fn parse_comma_line(line: &str) -> Option<ParsedPerson> {
    let mut parts: Vec<&str> = line.split(',').map(str::trim).collect();
    if parts.is_empty() {
        return None;
    }

    let mut full_name = parts.remove(0);
    // A dash-separated name ("Name - contact1") glues its first contact
    // value onto the name token instead of a comma -- split it back out
    // so every candidate value below gets sniffed the same way no
    // matter which format produced it.
    if let Some((name, extra)) = full_name.split_once(" - ") {
        full_name = name.trim();
        let extra = extra.trim();
        if !extra.is_empty() {
            parts.insert(0, extra);
        }
    }
    if full_name.is_empty() {
        return None;
    }

    let email = parts.iter().find(|p| p.contains('@')).map(|s| s.to_string());
    let phone = parts
        .iter()
        .find(|p| !p.is_empty() && !p.contains('@') && p.chars().filter(char::is_ascii_digit).count() >= 7)
        .map(|s| s.to_string());

    Some(ParsedPerson {
        full_name: full_name.to_string(),
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
    fn parses_a_real_dash_separated_two_person_block_with_reversed_contact_order() {
        // Sand-Sto Climate Controlled Storage (run rZFNRpmLIxuOrb_8K9hICw): a
        // third real format, name-dash-phone on one comma segment, and the
        // two people list phone/email in opposite order from each other --
        // this is what broke the old positional parser.
        let raw = "Irene Chen - (301) 787-9221, irene@chenlawgroup.com\n\
                   \n\
                   Amanda Ibarra - chchenpropertymgmtteam1@gmail.com,  (423) 314-2096";
        let people = parse_people_block(raw);
        assert_eq!(people.len(), 2);
        assert_eq!(people[0].full_name, "Irene Chen");
        assert_eq!(people[0].email.as_deref(), Some("irene@chenlawgroup.com"));
        assert_eq!(people[0].phone.as_deref(), Some("(301) 787-9221"));
        assert_eq!(people[1].full_name, "Amanda Ibarra");
        assert_eq!(people[1].email.as_deref(), Some("chchenpropertymgmtteam1@gmail.com"));
        assert_eq!(people[1].phone.as_deref(), Some("(423) 314-2096"));
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
