//! Parses Process Street's free-text "Owner/District Manager/Manager
//! Level Users" blocks into individual records.
//!
//! Real production data uses at least four genuinely different
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
//! - **Space-separated, one line per person, no commas or dashes** (LG
//!   Squared RV & Ministorage): `"Name email@x.com 555-010-0101"`, one
//!   such line per person. `parse_space_line` below; a chunk uses this
//!   format only if EVERY line parses as one, so a multi-line record's
//!   bare name/email/phone lines can never be mistaken for it. Before
//!   this existed the block fell into the multi-line path and came out
//!   as one garbled person (the whole first line as a "name").
//!
//! Boris's own framing: "I would comb through a healthy sample of PS's
//! various clients' forms to establish a pattern" -- this is that
//! comb-through's third real finding, not a hypothetical. This parser
//! detects which format a given chunk of text uses (by whether its
//! lines contain commas) rather than assuming one universally.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedPerson {
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
}

/// A reviewed, facility-scoped person + role -- the wire shape for the
/// confirmation screen's People chips (`api::clients_preview`'s output,
/// echoed back, possibly edited, on `api::clients_create`'s request).
/// PS's own free-text owner/DM/manager fields carry no facility-level
/// attribution at all (the same raw text is copy-pasted onto every
/// sister facility's own run -- see the vault's sister-site finding),
/// so which facility a real person actually belongs to is a call only
/// a human reviewing the batch can make; this is that call, recorded.
/// `role` is one of `clients.facility_people.role`'s Intake-sourced
/// values -- `"owner"` | `"district_manager"` | `"manager"` -- the only
/// three this screen ever assigns (Merchant Account's `"signer"` and
/// Contract Order's POC roles aren't part of this review at all).
///
/// Also doubles as `api::clients_facility_people`'s wire shape for an
/// "Add User" candidate straight off `clients.ps_person_index` (that
/// table's own `full_name`/`email`/`phone`/`role` columns match this
/// struct's fields by name) -- the `sqlx::FromRow` derive is for that
/// query, not for anything Intake-mapping related.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct PersonAssignment {
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: String,
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

    let email = parts
        .iter()
        .find(|p| p.contains('@'))
        .map(|s| s.to_string());
    let phone = parts
        .iter()
        .find(|p| {
            !p.is_empty() && !p.contains('@') && p.chars().filter(char::is_ascii_digit).count() >= 7
        })
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

/// One line of `"Name email phone"` separated by spaces only -- no
/// commas, no dash (LG Squared RV & Ministorage's Intake run, six owners
/// in one block). The name is everything before the first token that is
/// an email (`@`) or starts like a phone number (digit, `(` or `+`);
/// the email is the `@` token; the phone is whatever non-email tokens
/// follow the name, re-joined with single spaces so `"(555) 010 0102"`
/// survives, and kept only if it has at least 7 digits. Either contact
/// order works.
///
/// Returns `None` unless there's a non-empty name AND at least one
/// contact value. That's deliberate: it's what lets
/// `parse_people_block` use "every line parses" as its detector for this
/// format, since a line in a multi-line record (a bare name, a bare
/// email, a bare phone, `"Primary: x@y"`) fails one way or another.
fn parse_space_line(line: &str) -> Option<ParsedPerson> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let starts_a_phone =
        |t: &str| t.starts_with(|c: char| c.is_ascii_digit() || c == '(' || c == '+');
    let name_end = tokens
        .iter()
        .position(|t| t.contains('@') || starts_a_phone(t))?;
    if name_end == 0 {
        return None;
    }

    let contact = &tokens[name_end..];
    let email = contact.iter().find(|t| t.contains('@')).map(|t| {
        t.trim_matches(|c: char| c == ';' || c == '<' || c == '>')
            .to_string()
    });
    let phone_text = contact
        .iter()
        .filter(|t| !t.contains('@'))
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let phone =
        (phone_text.chars().filter(char::is_ascii_digit).count() >= 7).then_some(phone_text);

    if email.is_none() && phone.is_none() {
        return None;
    }
    Some(ParsedPerson {
        full_name: tokens[..name_end].join(" "),
        email,
        phone,
    })
}

pub fn parse_people_block(raw: &str) -> Vec<ParsedPerson> {
    let mut people = Vec::new();
    for chunk in split_into_chunks(raw) {
        if chunk.iter().all(|line| line.contains(',')) {
            people.extend(chunk.iter().filter_map(|line| parse_comma_line(line)));
        } else if let Some(space_separated) = chunk
            .iter()
            .map(|line| parse_space_line(line))
            .collect::<Option<Vec<_>>>()
        {
            people.extend(space_separated);
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
        assert_eq!(
            people.len(),
            3,
            "three blank-line-separated records must yield three people"
        );
        assert_eq!(people[0].full_name, "Kyle Lindley");
        assert_eq!(
            people[0].email.as_deref(),
            Some("k.lindley@prairie-enterprises.com")
        );
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
        assert_eq!(
            people[1].email.as_deref(),
            Some("chchenpropertymgmtteam1@gmail.com")
        );
        assert_eq!(people[1].phone.as_deref(), Some("(423) 314-2096"));
    }

    #[test]
    fn parses_a_space_separated_one_person_per_line_block() {
        // LG Squared RV & Ministorage's Intake run: one person per line,
        // "Name email phone" separated by spaces only -- no commas, no
        // dashes, no blank lines. Previously fell through to the
        // multi-line path, which made the whole first line the "name"
        // and yielded ONE garbled person instead of several. Synthetic
        // data standing in for the real shape (this repo is public).
        let raw = "Pat Sample pat.sample@example.com 555-010-0101\n\
                   Sam Example sam.example@example.com 555-010-0102\n\
                   Alex Placeholder alex.p@example.com 555-010-0103";
        let people = parse_people_block(raw);
        assert_eq!(people.len(), 3, "one person per line, not one per block");
        assert_eq!(people[0].full_name, "Pat Sample");
        assert_eq!(people[0].email.as_deref(), Some("pat.sample@example.com"));
        assert_eq!(people[0].phone.as_deref(), Some("555-010-0101"));
        assert_eq!(people[2].full_name, "Alex Placeholder");
        assert_eq!(people[2].phone.as_deref(), Some("555-010-0103"));
    }

    #[test]
    fn space_separated_lines_tolerate_either_contact_order_and_spaced_phones() {
        let raw = "Pat Sample 555-010-0101 pat.sample@example.com\n\
                   Sam Q. Example (555) 010 0102 sam@example.com\n\
                   Alex Placeholder alex.p@example.com\n\
                   Robin Phoneonly 555-010-0104";
        let people = parse_people_block(raw);
        assert_eq!(people.len(), 4);
        assert_eq!(people[0].full_name, "Pat Sample");
        assert_eq!(people[0].email.as_deref(), Some("pat.sample@example.com"));
        assert_eq!(people[0].phone.as_deref(), Some("555-010-0101"));
        assert_eq!(people[1].full_name, "Sam Q. Example");
        assert_eq!(people[1].phone.as_deref(), Some("(555) 010 0102"));
        assert_eq!(people[2].phone, None);
        assert_eq!(people[3].full_name, "Robin Phoneonly");
        assert_eq!(people[3].email, None);
        assert_eq!(people[3].phone.as_deref(), Some("555-010-0104"));
    }

    #[test]
    fn a_single_space_separated_line_is_one_person() {
        let people = parse_people_block("Pat Sample pat.sample@example.com 555-010-0101");
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].full_name, "Pat Sample");
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
