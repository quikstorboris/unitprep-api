use std::io::Cursor;

use calamine::{open_workbook_auto_from_rs, Data, Reader};
use chrono::NaiveTime;

use crate::csv_document::CsvDocument;
use crate::uploaded_file::UploadedFile;

/// Parses an Excel workbook (`.xlsx`/`.xls`) into a CsvDocument.
///
/// LIMITATION: only the first worksheet is read. If a real-world export
/// puts data on a sheet other than the first (e.g. a cover/readme sheet
/// precedes it), that data will not be found. Not yet needed by any known
/// export format, so left as a documented limitation rather than adding
/// sheet-selection UI/logic before there's a real use case for it — revisit
/// if a workbook with the data on a non-first sheet shows up.
pub fn parse_excel_document(file: &UploadedFile) -> anyhow::Result<CsvDocument> {
    // `Cursor<&[u8]>` satisfies calamine's `Read + Seek` requirement just
    // as well as an owned `Cursor<Vec<u8>>` — no need to clone the whole
    // file's bytes just to hand them to the workbook reader.
    let cursor = Cursor::new(&file.bytes);

    let mut workbook = open_workbook_auto_from_rs(cursor)?;

    let first_sheet =
        workbook.sheet_names().first().cloned().ok_or_else(|| {
            anyhow::anyhow!("Workbook '{}' contains no worksheets", file.file_name)
        })?;

    let range = workbook.worksheet_range(&first_sheet)?;

    let mut rows_iter = range.rows();

    let header_row = rows_iter
        .next()
        .ok_or_else(|| anyhow::anyhow!("Workbook '{}' contains no rows", file.file_name))?;

    let headers: Vec<String> = header_row
        .iter()
        .map(cell_to_string)
        .map(|v| v.trim().to_lowercase())
        .collect();

    let mut rows: Vec<Vec<String>> = Vec::new();

    for row in rows_iter {
        let values: Vec<String> = row.iter().map(cell_to_string).collect();

        let has_data = values.iter().any(|v| !v.trim().is_empty());

        if has_data {
            rows.push(values);
        }
    }

    Ok(CsvDocument {
        file_name: file.file_name.clone(),
        headers,
        rows,
        modified_at: file.modified_at,
    })
}

fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(v) => v.clone(),
        Data::Bool(v) => v.to_string(),
        Data::Int(v) => v.to_string(),

        Data::Float(v) => {
            // The `as i64` cast saturates (not UB, but silently wrong) for
            // any whole-number value outside i64's range -- e.g. 1e30
            // became the string "9223372036854775807" with no error,
            // indistinguishable from a real value that size. Only take the
            // integer-formatting fast path when the value actually fits;
            // an out-of-range whole number falls back to the same
            // to_string() the non-whole branch already uses, which prints
            // the real digits instead of a wrong sentinel.
            if v.fract() == 0.0 && *v >= i64::MIN as f64 && *v <= i64::MAX as f64 {
                (*v as i64).to_string()
            } else {
                v.to_string()
            }
        }

        Data::DateTime(v) => {
            // calamine's own doc comment says `as_datetime()` returns
            // `None` on overflow, but property testing found that isn't
            // the whole story: some out-of-range serial values (e.g. a
            // corrupted cell with a wildly large float) make it panic
            // instead, inside chrono's own TimeDelta construction, not
            // ours to fix. `catch_unwind` treats that the same as `None`
            // -- fall back to the raw serial number rather than a 500 for
            // what is, from the caller's perspective, just one bad cell
            // in an otherwise-readable file. Safe here: no `panic = "abort"`
            // profile is set, and `ExcelDateTime`'s own read is side-effect
            // free, so there's nothing left in a torn state to unwind past.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| v.as_datetime()));

            match result {
                // Cells with no time-of-day component (the common case for
                // a plain date column) render as a bare date, not
                // midnight-suffixed noise.
                Ok(Some(dt)) if dt.time() == NaiveTime::MIN => dt.format("%Y-%m-%d").to_string(),
                Ok(Some(dt)) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
                Ok(None) | Err(_) => v.to_string(),
            }
        }

        Data::DateTimeIso(v) => v.clone(),

        Data::DurationIso(v) => v.clone(),

        Data::Error(v) => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use calamine::{ExcelDateTime, ExcelDateTimeType};
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn date_only_cell_formats_as_plain_date() {
        // Excel serial 45292 = 2024-01-01 (no time-of-day component).
        let cell = Data::DateTime(ExcelDateTime::new(
            45292.0,
            ExcelDateTimeType::DateTime,
            false,
        ));

        assert_eq!(cell_to_string(&cell), "2024-01-01");
    }

    #[test]
    fn datetime_cell_with_time_formats_with_time() {
        // 45292.5 = 2024-01-01 12:00:00.
        let cell = Data::DateTime(ExcelDateTime::new(
            45292.5,
            ExcelDateTimeType::DateTime,
            false,
        ));

        assert_eq!(cell_to_string(&cell), "2024-01-01 12:00:00");
    }

    /// Regression test: a whole-number float outside i64's range used to
    /// silently saturate via `as i64` (1e30 became "9223372036854775807",
    /// -1e30 became i64::MIN's string) with no error -- indistinguishable
    /// from a real value that size. The real digits (or at least a value
    /// of the right magnitude) must come through instead.
    #[test]
    fn huge_whole_number_float_does_not_silently_saturate_to_i64_bounds() {
        let huge = 1e30_f64;
        let result = cell_to_string(&Data::Float(huge));

        assert_ne!(result, i64::MAX.to_string());

        let round_tripped: f64 = result.parse().expect("should still parse as a number");
        assert!((round_tripped - huge).abs() / huge < 1e-9);
    }

    #[test]
    fn negative_huge_whole_number_float_does_not_silently_saturate_to_i64_bounds() {
        let huge = -1e30_f64;
        let result = cell_to_string(&Data::Float(huge));

        assert_ne!(result, i64::MIN.to_string());

        let round_tripped: f64 = result.parse().expect("should still parse as a number");
        assert!((round_tripped - huge).abs() / huge.abs() < 1e-9);
    }

    #[test]
    fn non_date_cell_types_are_unaffected() {
        assert_eq!(cell_to_string(&Data::Int(42)), "42");
        assert_eq!(cell_to_string(&Data::Float(10.0)), "10");
        assert_eq!(cell_to_string(&Data::String("hello".to_string())), "hello");
    }

    proptest! {
        /// This is the exact function that once silently stringified a
        /// date-typed cell to its raw serial-number float instead of a
        /// real date (see the "Parse Excel date/datetime cells" fix).
        /// Any f64 a workbook could conceivably contain -- including the
        /// NaN/infinite/negative/huge values `as_datetime()` is documented
        /// to reject via `None` -- must produce *some* string without
        /// panicking, whether that's a formatted date or the raw-number
        /// fallback.
        #[test]
        fn datetime_cell_never_panics_for_any_serial_value(serial in proptest::num::f64::ANY) {
            let cell = Data::DateTime(ExcelDateTime::new(
                serial,
                ExcelDateTimeType::DateTime,
                false,
            ));

            let _ = cell_to_string(&cell);
        }

        /// Same robustness property for Float cells (the other numeric
        /// variant with a fract()-based branch that could misbehave on
        /// NaN/infinity).
        #[test]
        fn float_cell_never_panics_for_any_value(value in proptest::num::f64::ANY) {
            let _ = cell_to_string(&Data::Float(value));
        }
    }
}
