//! Which files in a folder are dedup inputs, which one to pre-select, and
//! whether a chosen set of files can run together. Pure logic over file
//! headers and the vendor registry -- no I/O, no HTTP -- so the rules can
//! be tested without a server and reused by every caller (local folder
//! scan, Dropbox folder scan, the run itself).
//!
//! The registry has one row per *file format* (a report a PMS can
//! export). Each row also carries file metadata: the PMS it belongs to,
//! the report's name, a role, and a selection priority. Today every
//! usable format is `Primary` (a self-contained tenant file) and the only
//! cross-file rule is "pick one of the alternatives". `Supporting` marks a
//! recognized file that can't be checked on its own yet (e.g. QuikStor
//! Cloud's alternate-contacts file); it is the seam a future join-capable
//! vendor plugs into.

use serde::{Deserialize, Serialize};
use unitprep_core::csv_document::CsvDocument;
use unitprep_core::vendor_format::{detect_vendor, VendorFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileRole {
    /// A self-contained tenant file: one of these is enough to run a check.
    Primary,
    /// Recognized, but not usable on its own.
    Supporting,
    /// More fields for the tenants in the same system's primary file (the
    /// email report, the report holding the customer id). Joined onto the
    /// primary by unit and name; useless without one.
    Join,
}

impl FileRole {
    pub fn from_db_str(value: &str) -> Self {
        match value {
            "supporting" => FileRole::Supporting,
            "join" => FileRole::Join,
            _ => FileRole::Primary,
        }
    }
}

/// The registry row's file-level metadata, keyed by `VendorFormat::name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileFormatMeta {
    pub name: String,
    pub pms: String,
    pub report_name: String,
    pub role: FileRole,
    /// Higher is preferred when several primaries of one PMS are present.
    pub selection_priority: i32,
    pub guidance: String,
}

impl FileFormatMeta {
    /// Metadata for a format the registry has no file-level row for
    /// (cache lag, or a hand-built registry in a test): a standalone
    /// primary file named after the format.
    pub fn fallback(format_name: &str) -> Self {
        Self {
            name: format_name.to_string(),
            pms: format_name.to_string(),
            report_name: format_name.to_string(),
            role: FileRole::Primary,
            selection_priority: 0,
            guidance: String::new(),
        }
    }
}

/// The registry's file metadata for `format_name`, or a standalone-primary
/// fallback when the snapshot has none.
pub fn meta_for(format_name: &str, metas: &[FileFormatMeta]) -> FileFormatMeta {
    metas
        .iter()
        .find(|m| m.name == format_name)
        .cloned()
        .unwrap_or_else(|| FileFormatMeta::fallback(format_name))
}

/// One file as seen before any content is read: its name, where it came
/// from (a Dropbox path, or `None` for a local file), and its header row.
/// `headers` is `None` when the header row couldn't be read (a legacy
/// `.xls` the browser can't open).
#[derive(Debug, Clone)]
pub struct FileHeaders {
    pub file_name: String,
    pub path: Option<String>,
    pub headers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Recognized,
    Unrecognized,
    Unreadable,
}

/// For a readable file no registered format matched: the registered format
/// it most resembles and which of that format's required headers it lacks --
/// so the UI can say "looks like X, but is missing: ..." instead of a bare
/// "not a dedup file". Only produced when the resemblance is real (see
/// `closest_vendor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NearMiss {
    pub vendor_name: String,
    /// The vendor's own spelling of each required header the file lacks, in
    /// the vendor's order.
    pub missing_headers: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ClassifiedFile {
    pub file_name: String,
    pub path: Option<String>,
    pub status: FileStatus,
    pub format: Option<FileFormatMeta>,
    /// `Some` only for an `Unrecognized` file that resembles a registered
    /// format.
    pub near_miss: Option<NearMiss>,
}

/// The registered vendor whose signature `document` satisfies best without
/// satisfying it fully, or `None` when nothing resembles it.
///
/// "Resembles" is deliberately strict -- a wrong "looks like X" is worse
/// than saying nothing: at least two of the vendor's required headers must be
/// present (a one-header signature can never be a near miss, it either
/// matches or it does not), and at least half of them. Ranked by headers
/// matched, then fewest missing, then registry order, so the answer never
/// depends on anything but the registry's own ordering.
fn closest_vendor(document: &CsvDocument, vendors: &[VendorFormat]) -> Option<NearMiss> {
    vendors
        .iter()
        .enumerate()
        .filter_map(|(index, vendor)| {
            let missing: Vec<String> = vendor
                .signature_headers
                .iter()
                .filter(|header| document.header_index(header).is_none())
                .cloned()
                .collect();
            let total = vendor.signature_headers.len();
            let matched = total - missing.len();

            (matched >= 2 && matched * 2 >= total && !missing.is_empty())
                .then_some((matched, missing, index, vendor))
        })
        // `max_by_key` keeps the LAST maximum, so the registry-order
        // tiebreak is part of the key (reversed: the earliest-registered of
        // otherwise-equal formats must win).
        .max_by_key(|(matched, missing, index, _)| {
            (
                *matched,
                std::cmp::Reverse(missing.len()),
                std::cmp::Reverse(*index),
            )
        })
        .map(|(_, missing, _, vendor)| NearMiss {
            vendor_name: vendor.name.clone(),
            missing_headers: missing,
        })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Suggestion {
    /// The PMS the panel should show first.
    pub pms: Option<String>,
    /// File names to pre-check.
    pub selected: Vec<String>,
    /// `(file, preferred file)`: a primary the user may tick instead,
    /// which loses to the pre-selected one.
    pub alternatives: Vec<(String, String)>,
}

/// Classifies each file by its headers against the registry (same
/// case/separator-insensitive matching as a real ingest) and works out
/// the pre-selection.
pub fn classify(
    files: &[FileHeaders],
    vendors: &[VendorFormat],
    metas: &[FileFormatMeta],
) -> (Vec<ClassifiedFile>, Suggestion) {
    let classified: Vec<ClassifiedFile> = files
        .iter()
        .map(|file| {
            let (status, format, near_miss) = match &file.headers {
                None => (FileStatus::Unreadable, None, None),
                Some(headers) => {
                    let document = CsvDocument {
                        file_name: file.file_name.clone(),
                        headers: headers.clone(),
                        rows: Vec::new(),
                        modified_at: None,
                    };
                    match detect_vendor(&document, vendors) {
                        Some(vendor) => (
                            FileStatus::Recognized,
                            Some(meta_for(&vendor.name, metas)),
                            None,
                        ),
                        None => (
                            FileStatus::Unrecognized,
                            None,
                            closest_vendor(&document, vendors),
                        ),
                    }
                }
            };
            ClassifiedFile {
                file_name: file.file_name.clone(),
                path: file.path.clone(),
                status,
                format,
                near_miss,
            }
        })
        .collect();

    let suggestion = suggest(&classified);
    (classified, suggestion)
}

fn suggest(classified: &[ClassifiedFile]) -> Suggestion {
    let recognized: Vec<(&ClassifiedFile, &FileFormatMeta)> = classified
        .iter()
        .filter_map(|f| f.format.as_ref().map(|m| (f, m)))
        .collect();

    let primaries: Vec<(&ClassifiedFile, &FileFormatMeta)> = recognized
        .iter()
        .copied()
        .filter(|(_, m)| m.role == FileRole::Primary)
        .collect();

    if primaries.is_empty() {
        return Suggestion {
            pms: recognized.first().map(|(_, m)| m.pms.clone()),
            ..Suggestion::default()
        };
    }

    // The PMS with the best-priority primary wins; more primaries, then
    // name order, break ties so the result never depends on input order.
    let mut pms_names: Vec<&str> = primaries.iter().map(|(_, m)| m.pms.as_str()).collect();
    pms_names.sort_unstable();
    pms_names.dedup();
    let best_pms = pms_names
        .into_iter()
        .max_by_key(|pms| {
            let of_pms = primaries.iter().filter(|(_, m)| m.pms == *pms);
            let top = of_pms.clone().map(|(_, m)| m.selection_priority).max();
            (top, of_pms.count(), std::cmp::Reverse(pms.to_string()))
        })
        .expect("primaries is non-empty");

    let mut in_pms: Vec<&(&ClassifiedFile, &FileFormatMeta)> = primaries
        .iter()
        .filter(|(_, m)| m.pms == best_pms)
        .collect();
    // Highest priority first; file name keeps equal priorities stable.
    in_pms.sort_by(|a, b| {
        b.1.selection_priority
            .cmp(&a.1.selection_priority)
            .then_with(|| a.0.file_name.cmp(&b.0.file_name))
    });
    let chosen = in_pms[0].0.file_name.clone();

    // The join files of the chosen system ride along, one per kind.
    let mut joins: Vec<&(&ClassifiedFile, &FileFormatMeta)> = recognized
        .iter()
        .filter(|(_, m)| m.role == FileRole::Join && m.pms == best_pms)
        .collect();
    joins.sort_by(|a, b| {
        b.1.selection_priority
            .cmp(&a.1.selection_priority)
            .then_with(|| a.0.file_name.cmp(&b.0.file_name))
    });
    let mut seen_kinds: Vec<&str> = Vec::new();
    let mut selected = vec![chosen.clone()];
    for (file, meta) in joins {
        if !seen_kinds.contains(&meta.name.as_str()) {
            seen_kinds.push(meta.name.as_str());
            selected.push(file.file_name.clone());
        }
    }

    Suggestion {
        pms: Some(best_pms.to_string()),
        alternatives: in_pms[1..]
            .iter()
            .map(|(f, _)| (f.file_name.clone(), chosen.clone()))
            .collect(),
        selected,
    }
}

/// A file picked for a run, with what the registry made of it
/// (`None` = no registered format matched).
#[derive(Debug, Clone)]
pub struct DetectedFile<'a> {
    pub file_name: &'a str,
    pub format: Option<FileFormatMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionError {
    NoFiles,
    Unrecognized {
        file: String,
    },
    Supporting {
        file: String,
        report: String,
    },
    JoinWithoutPrimary {
        file: String,
        report: String,
    },
    MixedSystems {
        first: String,
        second: String,
    },
    DuplicateFormat {
        first: String,
        second: String,
        report: String,
    },
    Alternatives {
        first: String,
        second: String,
    },
}

impl std::fmt::Display for SelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelectionError::NoFiles => write!(f, "No file was selected."),
            SelectionError::Unrecognized { file } => write!(
                f,
                "'{file}' does not match any known tenant export format, so it can't be checked."
            ),
            SelectionError::Supporting { file, report } => write!(
                f,
                "'{file}' is a supporting file ({report}) and can't be checked on its own yet. Select the main tenant file instead."
            ),
            SelectionError::JoinWithoutPrimary { file, report } => write!(
                f,
                "'{file}' ({report}) only adds details to the main tenant file of its system. Select the main tenant file as well."
            ),
            SelectionError::MixedSystems { first, second } => write!(
                f,
                "The selected files come from different systems ({first} and {second}). Select files from one system."
            ),
            SelectionError::DuplicateFormat { first, second, report } => write!(
                f,
                "'{first}' and '{second}' are the same kind of file ({report}). Select only one."
            ),
            SelectionError::Alternatives { first, second } => write!(
                f,
                "'{first}' and '{second}' hold the same tenants, so using both would count them twice. Select only one."
            ),
        }
    }
}

impl std::error::Error for SelectionError {}

/// What a run reads: the main file, and the files joined onto it. Indices
/// are into the slice passed to `plan_ingest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestPlan {
    pub primary: usize,
    pub joins: Vec<usize>,
}

/// Decides whether `files` can run together. A run takes exactly one
/// primary file, plus any number of join files of the same system (one of
/// each kind). The error cases are what stop a multi-select from silently
/// double-counting tenants or mixing two systems' data.
pub fn plan_ingest(files: &[DetectedFile<'_>]) -> Result<IngestPlan, SelectionError> {
    if files.is_empty() {
        return Err(SelectionError::NoFiles);
    }

    for file in files {
        let Some(meta) = &file.format else {
            return Err(SelectionError::Unrecognized {
                file: file.file_name.to_string(),
            });
        };
        if meta.role == FileRole::Supporting {
            return Err(SelectionError::Supporting {
                file: file.file_name.to_string(),
                report: meta.report_name.clone(),
            });
        }
    }

    let meta_of = |i: usize| files[i].format.as_ref().expect("checked above");

    let primaries: Vec<usize> = (0..files.len())
        .filter(|&i| meta_of(i).role == FileRole::Primary)
        .collect();
    let joins: Vec<usize> = (0..files.len())
        .filter(|&i| meta_of(i).role == FileRole::Join)
        .collect();

    let Some(&primary) = primaries.first() else {
        let first = joins[0];
        return Err(SelectionError::JoinWithoutPrimary {
            file: files[first].file_name.to_string(),
            report: meta_of(first).report_name.clone(),
        });
    };

    if let Some(&other) = primaries.get(1) {
        let first_meta = meta_of(primary);
        let other_meta = meta_of(other);
        return Err(if other_meta.pms != first_meta.pms {
            SelectionError::MixedSystems {
                first: first_meta.pms.clone(),
                second: other_meta.pms.clone(),
            }
        } else if other_meta.name == first_meta.name {
            SelectionError::DuplicateFormat {
                first: files[primary].file_name.to_string(),
                second: files[other].file_name.to_string(),
                report: first_meta.report_name.clone(),
            }
        } else {
            SelectionError::Alternatives {
                first: files[primary].file_name.to_string(),
                second: files[other].file_name.to_string(),
            }
        });
    }

    let pms = &meta_of(primary).pms;
    for (n, &join) in joins.iter().enumerate() {
        let meta = meta_of(join);
        if &meta.pms != pms {
            return Err(SelectionError::MixedSystems {
                first: pms.clone(),
                second: meta.pms.clone(),
            });
        }
        if let Some(&earlier) = joins[..n].iter().find(|&&e| meta_of(e).name == meta.name) {
            return Err(SelectionError::DuplicateFormat {
                first: files[earlier].file_name.to_string(),
                second: files[join].file_name.to_string(),
                report: meta.report_name.clone(),
            });
        }
    }

    Ok(IngestPlan { primary, joins })
}

#[cfg(test)]
#[path = "file_selection_tests.rs"]
mod tests;
