use super::*;
use unitprep_core::csv_document::CsvDocument;

#[test]
fn detects_duplicate_unit_numbers() {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
        ],
        rows: vec![
            vec![
                "A01".to_string(),
                "10x10 Inside Climate"
                    .to_string(),
            ],
            vec![
                "A01".to_string(),
                "10x10 Inside Climate"
                    .to_string(),
            ],
        ],
    };

    let issues =
        validate_document(
            &document,
            &HashSet::new(),
            &GroupCheckAcknowledgments::default(),
        )
        .unwrap();

    assert!(issues.iter().any(|i| {
        i.description
            == "Duplicate unit numbers"
            && i.severity
                == Severity::Error
    }));
}

#[test]
fn rare_group_is_a_warning_not_an_error(
) {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
        ],
        rows: vec![vec![
            "A01".to_string(),
            "10x17 Climate"
                .to_string(),
        ]],
    };

    let issues =
        validate_document(
            &document,
            &HashSet::new(),
            &GroupCheckAcknowledgments::default(),
        )
        .unwrap();

    let rare = issues
        .iter()
        .find(|i| {
            i.description
                == "Rare UnitGroup detected"
        })
        .expect(
            "expected a rare-group issue",
        );

    assert_eq!(
        rare.severity,
        Severity::Warning
    );
}

#[test]
fn blank_unitgroup_is_an_error_not_a_warning(
) {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
        ],
        rows: vec![vec![
            "A01".to_string(),
            "".to_string(),
        ]],
    };

    let issues =
        validate_document(
            &document,
            &HashSet::new(),
            &GroupCheckAcknowledgments::default(),
        )
        .unwrap();

    let blank = issues
        .iter()
        .find(|i| {
            i.description
                == "Blank UnitGroup values"
        })
        .expect(
            "expected a blank-UnitGroup issue",
        );

    assert_eq!(
        blank.severity,
        Severity::Error
    );
}

#[test]
fn detects_invalid_dimensions() {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "width".to_string(),
            "length".to_string(),
        ],
        rows: vec![vec![
            "A01".to_string(),
            "10x10 Inside Climate"
                .to_string(),
            "0".to_string(),
            "10".to_string(),
        ]],
    };

    let issues =
        validate_document(
            &document,
            &HashSet::new(),
            &GroupCheckAcknowledgments::default(),
        )
        .unwrap();

    let issue = issues
        .iter()
        .find(|i| {
            i.description
                == "Invalid dimensions"
        })
        .expect(
            "expected an invalid-dimensions issue",
        );

    // Warning, not Error: a unit with no dimensions is often a
    // legitimately non-standard group (office, apartment), not
    // necessarily bad data -- see the doc comment on `issues::build`'s
    // call site in mod.rs.
    assert_eq!(
        issue.severity,
        Severity::Warning
    );
}

#[test]
fn exempted_unit_is_not_flagged_for_invalid_dimensions(
) {
    // The group name must actually parse as a real dimension ("1200 sq
    // ft" would not) -- an odd/non-dimensioned name is now excluded from
    // this check entirely regardless of exemption (see mod.rs's
    // no-overlap-with-Odd rule), which would make this test pass for
    // the wrong reason.
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "width".to_string(),
            "length".to_string(),
        ],
        rows: vec![vec![
            "Office".to_string(),
            "10x10 Inside Climate"
                .to_string(),
            "".to_string(),
            "".to_string(),
        ]],
    };

    let mut exempt = HashSet::new();
    exempt.insert(
        "Office".to_string(),
    );

    let issues = validate_document(
        &document,
        &exempt,
        &GroupCheckAcknowledgments::default(),
    )
    .unwrap();

    assert!(!issues.iter().any(|i| {
        i.description
            == "Invalid dimensions"
    }));
}

/// The core no-overlap rule: a group with no dimension attempt at all
/// (an office, an apartment) is Odd only, even though its actual
/// Width/Length columns are blank -- that's expected for a
/// non-dimensioned group, not a second, separate "Invalid dimensions"
/// problem.
#[test]
fn a_group_with_no_dimension_attempt_is_odd_only_not_also_invalid(
) {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "width".to_string(),
            "length".to_string(),
        ],
        rows: vec![vec![
            "Office".to_string(),
            "Hertz Office Space"
                .to_string(),
            "".to_string(),
            "".to_string(),
        ]],
    };

    let issues = validate_document(
        &document,
        &HashSet::new(),
        &GroupCheckAcknowledgments::default(),
    )
    .unwrap();

    assert!(issues.iter().any(|i| {
        i.description
            == "Odd UnitGroup values"
    }));

    assert!(!issues.iter().any(|i| {
        i.description
            == "Invalid dimensions"
    }));
}

/// The other half of the no-overlap rule: a group name that *attempts* a
/// dimension but botches it ("10x", missing its second number) is
/// Invalid Dimensions, regardless of what the row's own actual
/// Width/Length columns say -- and is never also Odd.
#[test]
fn a_malformed_dimension_attempt_is_invalid_dimensions_only_not_odd(
) {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "width".to_string(),
            "length".to_string(),
        ],
        rows: vec![vec![
            "A01".to_string(),
            "10x".to_string(),
            "10".to_string(),
            "10".to_string(),
        ]],
    };

    let issues = validate_document(
        &document,
        &HashSet::new(),
        &GroupCheckAcknowledgments::default(),
    )
    .unwrap();

    assert!(issues.iter().any(|i| {
        i.description
            == "Invalid dimensions"
    }));

    assert!(!issues.iter().any(|i| {
        i.description
            == "Odd UnitGroup values"
    }));
}

#[test]
fn detects_climate_mismatch() {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "climatecontrolled"
                .to_string(),
        ],
        rows: vec![vec![
            "A01".to_string(),
            "10x10 Inside Climate"
                .to_string(),
            "No".to_string(),
        ]],
    };

    let issues =
        validate_document(
            &document,
            &HashSet::new(),
            &GroupCheckAcknowledgments::default(),
        )
        .unwrap();

    let issue = issues
        .iter()
        .find(|i| {
            i.description
                == "Climate status does not match UnitGroup"
        })
        .expect(
            "expected a climate-mismatch issue",
        );

    assert_eq!(
        issue.severity,
        Severity::Warning
    );
}

#[test]
fn detects_locality_mismatch() {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "locality".to_string(),
        ],
        rows: vec![vec![
            "A01".to_string(),
            "10x10 Outside Non-Climate"
                .to_string(),
            "Inside".to_string(),
        ]],
    };

    let issues =
        validate_document(
            &document,
            &HashSet::new(),
            &GroupCheckAcknowledgments::default(),
        )
        .unwrap();

    let issue = issues
        .iter()
        .find(|i| {
            i.description
                == "Locality does not match UnitGroup"
        })
        .expect(
            "expected a locality-mismatch issue",
        );

    assert_eq!(
        issue.severity,
        Severity::Warning
    );
}

#[test]
fn detects_unitgroup_dimension_mismatch(
) {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "width".to_string(),
            "length".to_string(),
        ],
        rows: vec![vec![
            "A01".to_string(),
            "10x20 Inside Climate"
                .to_string(),
            "10".to_string(),
            "15".to_string(),
        ]],
    };

    let issues =
        validate_document(
            &document,
            &HashSet::new(),
            &GroupCheckAcknowledgments::default(),
        )
        .unwrap();

    let issue = issues
        .iter()
        .find(|i| {
            i.description
                == "UnitGroup dimensions do not match Width/Length"
        })
        .expect(
            "expected a dimension-mismatch issue",
        );

    assert_eq!(
        issue.severity,
        Severity::Warning
    );
}

#[test]
fn detects_rare_group() {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
        ],
        rows: vec![vec![
            "A01".to_string(),
            "10x17 Climate"
                .to_string(),
        ]],
    };

    let issues =
        validate_document(
            &document,
            &HashSet::new(),
            &GroupCheckAcknowledgments::default(),
        )
        .unwrap();

    assert!(issues.iter().any(|i| {
        i.description
            == "Rare UnitGroup detected"
    }));
}

#[test]
fn rare_group_flags_group_names_not_unit_numbers(
) {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
        ],
        rows: vec![vec![
            "A01".to_string(),
            "10x17 Climate"
                .to_string(),
        ]],
    };

    let issues =
        validate_document(
            &document,
            &HashSet::new(),
            &GroupCheckAcknowledgments::default(),
        )
        .unwrap();

    let rare = issues
        .iter()
        .find(|i| {
            i.description
                == "Rare UnitGroup detected"
        })
        .expect(
            "expected a rare-group issue",
        );

    assert!(
        rare.flagged_are_group_names
    );
    assert_eq!(
        rare.flagged_values,
        vec!["10x17 Climate".to_string()]
    );
}

#[test]
fn invalid_dimensions_flags_unit_numbers_not_group_names(
) {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "width".to_string(),
            "length".to_string(),
        ],
        rows: vec![vec![
            "A01".to_string(),
            "10x10 Inside Climate"
                .to_string(),
            "0".to_string(),
            "10".to_string(),
        ]],
    };

    let issues =
        validate_document(
            &document,
            &HashSet::new(),
            &GroupCheckAcknowledgments::default(),
        )
        .unwrap();

    let invalid = issues
        .iter()
        .find(|i| {
            i.description
                == "Invalid dimensions"
        })
        .expect(
            "expected an invalid-dimensions issue",
        );

    assert!(
        !invalid
            .flagged_are_group_names
    );
    assert_eq!(
        invalid.flagged_values,
        vec!["A01".to_string()]
    );
}

#[test]
fn detects_casing_mismatch() {
    let document = CsvDocument {
            modified_at: None,
        file_name: "test.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
        ],
        rows: vec![
            vec![
                "K10".to_string(),
                "10x10 Climate"
                    .to_string(),
            ],
            vec![
                "k10".to_string(),
                "10x10 Climate"
                    .to_string(),
            ],
        ],
    };

    let issues =
        validate_document(
            &document,
            &HashSet::new(),
            &GroupCheckAcknowledgments::default(),
        )
        .unwrap();

    assert!(issues.iter().any(|i| {
        i.description
            == "Inconsistent unit-number casing"
    }));
}

/// A document missing the required UnitGroup/Number columns must
/// fail loudly (`Err`), not silently report zero issues — see the
/// comment on `ColumnIndices::discover`'s `None` branch above. This
/// path should be unreachable in practice (discovery already
/// requires these columns before a file is passed here), but if
/// discovery and validation's column lookups ever disagree again,
/// this must surface as an error, not a false "all clean" result.
#[test]
fn validate_document_errors_loudly_when_a_supposed_unit_file_has_no_matching_columns(
) {
    let document = CsvDocument {
            modified_at: None,
        file_name: "units.csv"
            .to_string(),
        headers: vec![
            "some_other_column"
                .to_string(),
        ],
        rows: vec![vec![
            "value".to_string(),
        ]],
    };

    let err = validate_document(
        &document,
        &HashSet::new(),
        &GroupCheckAcknowledgments::default(),
    )
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("units.csv")
    );
}

/// The regression this whole fix exists for: a real unit file whose
/// UnitGroup header uses an underscore (as discovery already
/// tolerates — see
/// `api::discover::tests::discover_classifies_unit_file_with_underscored_headers`)
/// must still be validated normally, not silently skipped.
#[test]
fn validates_a_unit_file_with_an_underscored_unitgroup_header(
) {
    let document = CsvDocument {
            modified_at: None,
        file_name: "units.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unit_group".to_string(),
        ],
        rows: vec![
            vec![
                "A01".to_string(),
                "".to_string(),
            ],
        ],
    };

    let issues =
        validate_document(
            &document,
            &HashSet::new(),
            &GroupCheckAcknowledgments::default(),
        )
        .unwrap();

    assert!(issues.iter().any(|i| {
        i.description
            == issues::BLANK_UNITGROUP
    }));
}
