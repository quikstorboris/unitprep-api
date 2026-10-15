# Longview Self Storage / T.C.'s RV rental agreement

- **What it is:** a blank rental agreement template, converted from a PDF to .docx
  (`longview_self_storage_99987_rental_agreement-3-1.docx`). No personal data, so
  `input.docx` is the original, byte for byte.
- **Original:** `QS Fileserver/Shared/QMS Onboarding/Longview Self Storage/` on Dropbox.
  SHA-256 `cb5fe60a32ff61b54320a23ba983f74de76c400c1298adca8c1e387a7a515284`, 49,265 bytes.
- **Why it is in the corpus:** every kind of blank a PDF conversion produces is in this one
  file, and the tagger found 2 hits (both wrong) before they were handled:
  underlined-tab blanks, floating text-box labels over drawn field bars, empty table cells,
  initials squares inside group shapes, and a `mc:Fallback` duplicate of every shape.
- **`patterns.json`:** the 42 label patterns in force when this case was blessed (the dev
  database's `client_ops.tag_pattern`), so the test needs no database.
- **`expected.json`:** 28 candidates: 27 Auto (applied) and 1 Needs Review (the lienholder
  "City:", which has no QMS tag).
- **Deliberately not tagged:** Employer, Active Military, the whole lienholder block, the
  TC'S unit blank (this is the Longview copy), card number / expires / name on card, signature
  lines, and 4 underscore text boxes whose "initials" label lives in a different (grouped)
  shape.
- **Full record:** the vault note `Real Document Record - Longview Self Storage Rental Agreement`.
- **Re-bless after an intended change:**
  `UPDATE_CORPUS=1 cargo test -p unitprep-tagger-pipeline --test corpus`, then review the diff.
