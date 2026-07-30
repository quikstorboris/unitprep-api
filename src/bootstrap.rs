//! First-admin bootstrap (Phase 2 task 8).
//!
//! Solves the chicken-and-egg at the root of the invite model: an invite
//! is created by an admin, and until one exists nobody can create one.
//! This is the one-time way in.
//!
//! ## Why a CLI subcommand and not an endpoint
//!
//! An HTTP endpoint that can mint an administrator is a hole whose only
//! defence is an environment variable being right, everywhere, forever.
//! A subcommand has no remote surface at all: the guard is structural
//! rather than configured. (`AUTH_BOOTSTRAP_ENABLED` on the registration
//! path is exactly the shape being retired -- once this and invite
//! acceptance exist, that variable should never be set again.)
//!
//! It is a subcommand of the main binary rather than a `src/bin/` target
//! because this crate has no library target, so a separate binary could
//! not `use` the token generation and hashing in `auth::session_token` and
//! would have to reimplement them. Duplicating a security primitive to
//! keep a file tidy is the wrong trade.
//!
//! ## Why it connects as the owner
//!
//! `BOOTSTRAP_DATABASE_URL` must be the owner/direct connection, not the
//! application's. The owner bypasses RLS, which is what lets this insert a
//! user with no established identity. The alternative -- teaching
//! `app_service` to create users -- would hand the running application a
//! capability it must never have, permanently, to serve a one-time need.
//!
//! ## What the "no existing users" guard is and is not
//!
//! It prevents an **operator mistake** -- running this twice, or against
//! the wrong branch, quietly creating a second administrator. It is *not*
//! a security control: anyone holding the owner credential can already do
//! anything this tool does, and more. Stated plainly so nobody mistakes it
//! for a boundary and builds on it.
//!
//! ## The account is created `invited`, not `active`
//!
//! Deliberately, so the first admin walks the same path as everyone after
//! them: accept the invite, which flips `invited` -> `active` via
//! `consume_invite`, then enrol a passkey. One enrolment path, exercised
//! from the very first account, rather than a special case that only ever
//! runs once and is therefore never really tested.

use std::collections::HashMap;

use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::auth::generate_token;

/// How long the printed setup link stays usable.
///
/// 24 hours, chosen deliberately over a more generous window. The real risk
/// with a setup link is not that someone brute-forces it -- it is 256 bits
/// of random -- but that it *lingers*: sitting in a chat log, an email, a
/// terminal's scrollback, long after the person who needed it has enrolled.
/// A short window shrinks that exposure far more cheaply than any process
/// control, and `--reissue-invite` makes an expired link a non-event.
const DEFAULT_INVITE_HOURS: i64 = 24;

/// The `company` enum's accepted values, mirroring the `user_company`
/// Postgres type. Validated here so a typo fails before any connection is
/// opened, with a message naming the valid options -- rather than
/// surfacing as a Postgres enum cast error.
const VALID_COMPANIES: [&str; 3] = ["trojan", "cobre", "quikstor"];

pub const USAGE: &str = "\
Create the first administrator and print a one-time setup link.

USAGE:
    unitprep bootstrap-admin --email <EMAIL> --first-name <NAME> \
--last-name <NAME> --company <COMPANY> [--job-title <TITLE>]

    unitprep bootstrap-admin --reissue-invite --email <EMAIL>

OPTIONS:
    --email           required
    --first-name      required (creating only)
    --last-name       required (creating only)
    --company         required (creating only); one of: trojan, cobre, quikstor
    --job-title       optional
    --reissue-invite  mint a fresh setup link for an administrator who
                      already exists but has not enrolled yet -- for when the
                      original link was lost or expired

ENVIRONMENT:
    BOOTSTRAP_DATABASE_URL   required. The OWNER/direct connection string,
                             not the application's. See the module docs for
                             why this deliberately does not reuse
                             DATABASE_URL.
    BOOTSTRAP_INVITE_HOURS   optional; defaults to 24.

Creating refuses to run if any user already exists -- it is a one-time
setup step, not a way to add users. For that, use the invite flow.";

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    /// Create the first administrator outright.
    Create {
        first_name: String,
        last_name: String,
        company: String,
        job_title: Option<String>,
    },
    /// Mint a replacement setup link for an administrator who exists but
    /// has not enrolled.
    ///
    /// This exists because the alternative recovery is impossible, not
    /// merely inconvenient. A lost setup token cannot be worked around by
    /// deleting the account and starting over: `auth_audit_logs` references
    /// users with `ON DELETE SET NULL`, and its append-only trigger forbids
    /// the UPDATE that would perform, so once the account has *any* audit
    /// history -- one failed sign-in attempt is enough -- it can never be
    /// hard-deleted, by anyone, including the owner role. Without this mode
    /// a mislaid link would leave a database with an administrator nobody
    /// can ever authenticate as and no supported way forward.
    ReissueInvite,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BootstrapArgs {
    pub email: String,
    pub mode: Mode,
}

/// Parses `--key value` pairs. Hand-rolled rather than pulling in a
/// argument-parsing dependency for one internal one-shot subcommand.
///
/// Rejects an unknown flag instead of ignoring it: a mistyped `--frist-name`
/// would otherwise silently create an account with a missing name, and this
/// tool's whole point is that it runs once.
pub fn parse_args(argv: &[String]) -> Result<BootstrapArgs, String> {
    const VALUE_OPTIONS: [&str; 5] = ["email", "first-name", "last-name", "company", "job-title"];

    let mut values: HashMap<String, String> = HashMap::new();
    let mut reissue = false;
    let mut i = 0;

    while i < argv.len() {
        let key = &argv[i];
        if !key.starts_with("--") {
            return Err(format!(
                "unexpected argument {key:?} (expected --key value)"
            ));
        }
        let name = key.trim_start_matches('-').to_string();

        // The one flag that takes no value. Handled before the
        // value-consuming branch so `--reissue-invite --email x` does not
        // swallow the next option as its argument.
        if name == "reissue-invite" {
            if reissue {
                return Err(format!("{key} given more than once"));
            }
            reissue = true;
            i += 1;
            continue;
        }

        if !VALUE_OPTIONS.contains(&name.as_str()) {
            return Err(format!("unknown option {key:?}"));
        }

        let value = argv
            .get(i + 1)
            .ok_or_else(|| format!("{key} requires a value"))?;
        if value.starts_with("--") {
            return Err(format!("{key} requires a value, found {value:?}"));
        }
        if values.insert(name, value.clone()).is_some() {
            return Err(format!("{key} given more than once"));
        }
        i += 2;
    }

    let get = |name: &str| -> Option<String> {
        values
            .get(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let required = |name: &str| -> Result<String, String> {
        get(name).ok_or_else(|| format!("--{name} is required"))
    };

    let email = required("email")?;

    // Deliberately not validated beyond containing '@'. The database holds
    // email as citext with a unique constraint, which is the check that
    // matters; a hand-rolled format rule would only ever be wrong in one
    // direction or the other.
    if !email.contains('@') {
        return Err(format!("--email does not look like an address: {email:?}"));
    }

    if reissue {
        // Reject the creating options rather than ignoring them: someone
        // passing a name alongside --reissue-invite plainly expects it to be
        // applied, and silently dropping it would be the wrong kind of
        // surprise on a recovery path.
        for unexpected in ["first-name", "last-name", "company", "job-title"] {
            if values.contains_key(unexpected) {
                return Err(format!(
                    "--{unexpected} cannot be combined with --reissue-invite, which only \
                     mints a new setup link for an account that already exists"
                ));
            }
        }
        return Ok(BootstrapArgs {
            email,
            mode: Mode::ReissueInvite,
        });
    }

    let company = required("company")?.to_lowercase();
    if !VALID_COMPANIES.contains(&company.as_str()) {
        return Err(format!(
            "--company must be one of {}, got {company:?}",
            VALID_COMPANIES.join(", ")
        ));
    }

    Ok(BootstrapArgs {
        email,
        mode: Mode::Create {
            first_name: required("first-name")?,
            last_name: required("last-name")?,
            company,
            job_title: get("job-title"),
        },
    })
}

fn invite_hours() -> i64 {
    std::env::var("BOOTSTRAP_INVITE_HOURS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|d| *d > 0)
        .unwrap_or(DEFAULT_INVITE_HOURS)
}

/// Runs the bootstrap. Returns a message to print on success, or an error
/// message to print to stderr before exiting non-zero.
pub async fn run(args: BootstrapArgs) -> Result<String, String> {
    let database_url = std::env::var("BOOTSTRAP_DATABASE_URL").map_err(|_| {
        "BOOTSTRAP_DATABASE_URL is not set. It must be the OWNER/direct connection \
         string (the one used for migrations), not the application's DATABASE_URL -- \
         creating a user requires bypassing row-level security, which the \
         application role deliberately cannot do."
            .to_string()
    })?;

    // Eager connect, not lazy: for a one-shot command a bad URL should fail
    // immediately with a connection error, rather than surfacing later as a
    // confusing failure partway through.
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .map_err(|err| format!("could not connect using BOOTSTRAP_DATABASE_URL: {err}"))?;

    // Everything below runs in one transaction: a partial bootstrap -- a
    // user with no invite -- would leave an account nobody can enrol into
    // and that the creating path would then refuse to fix, because a user
    // exists.
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| format!("could not begin transaction: {err}"))?;

    let user_id = match &args.mode {
        Mode::Create {
            first_name,
            last_name,
            company,
            job_title,
        } => {
            let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM auth.users")
                .fetch_one(&mut *tx)
                .await
                .map_err(|err| format!("could not count existing users: {err}"))?;

            if existing > 0 {
                return Err(format!(
                    "refusing to run: {existing} user(s) already exist. This is a one-time \
                     setup step for an empty database. If the administrator exists but never \
                     enrolled and the setup link is gone, use --reissue-invite. To add a \
                     further user, have an existing administrator issue an invite."
                ));
            }

            sqlx::query_scalar(
                "INSERT INTO auth.users
                     (email, first_name, last_name, job_title, company, role, status)
                 VALUES ($1::citext, $2, $3, $4, $5::auth.user_company,
                         'admin'::auth.auth_role, 'invited'::auth.user_status)
                 RETURNING id",
            )
            .bind(&args.email)
            .bind(first_name)
            .bind(last_name)
            .bind(job_title.as_deref())
            .bind(company)
            .fetch_one(&mut *tx)
            .await
            .map_err(|err| format!("could not create the administrator: {err}"))?
        }

        Mode::ReissueInvite => {
            // Narrow on purpose. This is a recovery path, so it must not
            // become a general "mint a link for anyone" tool: it requires
            // the named account to still be un-enrolled, which is the only
            // situation where no other route in exists.
            let found: Option<(Uuid, String, i64)> = sqlx::query_as(
                "SELECT u.id,
                        u.status::text,
                        (SELECT count(*) FROM auth.webauthn_credentials c
                          WHERE c.user_id = u.id)
                 FROM auth.users u
                 WHERE u.email = $1::citext AND u.deleted_at IS NULL",
            )
            .bind(&args.email)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|err| format!("could not look up that account: {err}"))?;

            let Some((id, status, credential_count)) = found else {
                return Err(format!(
                    "no active account with email {}. Nothing to reissue an invite for.",
                    args.email
                ));
            };

            if credential_count > 0 {
                return Err(format!(
                    "refusing to reissue: {} already has {credential_count} passkey(s) \
                     enrolled and can sign in normally. A setup link is only for an \
                     account that has never enrolled.",
                    args.email
                ));
            }

            if status != "invited" {
                return Err(format!(
                    "refusing to reissue: {} has status {status:?}, not \"invited\". \
                     Reissuing is only for an account still awaiting its first enrolment.",
                    args.email
                ));
            }

            // Any outstanding invite is retired first, so exactly one live
            // link exists per account. Leaving the old one usable would mean
            // a lost token stayed valid until natural expiry -- the opposite
            // of what someone reaching for this command wants.
            let retired = sqlx::query(
                "UPDATE auth.user_invites SET used_at = now()
                  WHERE user_id = $1 AND used_at IS NULL",
            )
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|err| format!("could not retire the previous invite: {err}"))?
            .rows_affected();

            if retired > 0 {
                eprintln!("note: retired {retired} outstanding invite(s) for this account");
            }

            id
        }
    };

    // Same generation and hashing as a session token -- imported rather
    // than reimplemented, so there is exactly one definition of how a
    // bearer secret is produced and stored in this codebase.
    let (raw_token, token_hash) = generate_token();

    let hours = invite_hours();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(hours);

    // created_by is left to its column default, which resolves to NULL with
    // no identity context set -- the case the schema made that column
    // nullable for.
    sqlx::query(
        "INSERT INTO auth.user_invites (user_id, token_hash, expires_at)
         VALUES ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(&token_hash)
    .bind(expires_at)
    .execute(&mut *tx)
    .await
    .map_err(|err| format!("could not create the setup invite: {err}"))?;

    tx.commit()
        .await
        .map_err(|err| format!("could not commit: {err}"))?;

    let headline = match args.mode {
        Mode::Create { .. } => "Administrator created.",
        Mode::ReissueInvite => "New setup link issued; any previous one is now void.",
    };

    Ok(format!(
        "\n{headline}\n\
         \n  user id : {user_id}\
         \n  email   : {}\
         \n  status  : invited (becomes active when the invite is accepted)\
         \n\n\
         Setup token, valid for {hours} hour(s):\n\n  {raw_token}\n\n\
         Shown ONCE -- only its hash is stored, so it cannot be recovered. If it \
         is lost, run again with --reissue-invite; do NOT try to delete the \
         account and start over, which stops being possible as soon as the \
         account has any audit history.\n\n\
         Accepting the invite is what activates the account and enrols the \
         first passkey (invite acceptance endpoint, not yet built as of this \
         writing -- until then the account exists but cannot sign in).\n",
        args.email
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn valid() -> Vec<String> {
        argv(&[
            "--email",
            "a@b.com",
            "--first-name",
            "A",
            "--last-name",
            "B",
            "--company",
            "quikstor",
        ])
    }

    /// Unwraps the creating mode, failing the test rather than panicking
    /// obscurely if a case somehow parsed as reissue.
    fn as_create(args: &BootstrapArgs) -> (&str, &str, &str, Option<&str>) {
        match &args.mode {
            Mode::Create {
                first_name,
                last_name,
                company,
                job_title,
            } => (
                first_name.as_str(),
                last_name.as_str(),
                company.as_str(),
                job_title.as_deref(),
            ),
            Mode::ReissueInvite => panic!("expected Create mode"),
        }
    }

    #[test]
    fn parses_a_complete_invocation() {
        let parsed = parse_args(&valid()).expect("should parse");
        assert_eq!(parsed.email, "a@b.com");
        let (first, last, company, job) = as_create(&parsed);
        assert_eq!((first, last, company, job), ("A", "B", "quikstor", None));
    }

    #[test]
    fn job_title_is_optional_and_captured_when_given() {
        let mut args = valid();
        args.push("--job-title".to_string());
        args.push("Implementation Manager".to_string());
        let parsed = parse_args(&args).expect("should parse");
        assert_eq!(as_create(&parsed).3, Some("Implementation Manager"));
    }

    #[test]
    fn reissue_needs_only_an_email() {
        let parsed =
            parse_args(&argv(&["--reissue-invite", "--email", "a@b.com"])).expect("should parse");
        assert_eq!(parsed.mode, Mode::ReissueInvite);
        assert_eq!(parsed.email, "a@b.com");
    }

    /// A bare flag must not consume the following option as its value --
    /// the ordering bug that would make `--reissue-invite --email x` parse
    /// as `reissue-invite="--email"` and then report email as missing.
    #[test]
    fn reissue_flag_does_not_swallow_the_next_option() {
        let both_orders = [
            argv(&["--reissue-invite", "--email", "a@b.com"]),
            argv(&["--email", "a@b.com", "--reissue-invite"]),
        ];
        for args in both_orders {
            let parsed = parse_args(&args).unwrap_or_else(|e| panic!("{args:?} failed: {e}"));
            assert_eq!(parsed.mode, Mode::ReissueInvite);
            assert_eq!(parsed.email, "a@b.com");
        }
    }

    /// Silently ignoring a name passed alongside --reissue-invite would be
    /// the wrong surprise on a recovery path -- the caller plainly expects
    /// it applied.
    #[test]
    fn reissue_rejects_the_creating_options_rather_than_ignoring_them() {
        for (flag, value) in [
            ("--first-name", "A"),
            ("--last-name", "B"),
            ("--company", "quikstor"),
            ("--job-title", "X"),
        ] {
            let args = argv(&["--reissue-invite", "--email", "a@b.com", flag, value]);
            let err = parse_args(&args).expect_err("should reject");
            assert!(
                err.contains("cannot be combined"),
                "{flag} should be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn reissue_still_requires_an_email() {
        let err = parse_args(&argv(&["--reissue-invite"])).expect_err("should reject");
        assert!(err.contains("email"), "got {err:?}");
    }

    #[test]
    fn every_required_option_is_actually_required() {
        for missing in ["--email", "--first-name", "--last-name", "--company"] {
            let kept: Vec<String> = valid()
                .chunks(2)
                .filter(|pair| pair[0] != missing)
                .flat_map(|pair| pair.to_vec())
                .collect();
            let err = parse_args(&kept).expect_err("should reject");
            assert!(
                err.contains(missing.trim_start_matches("--")),
                "error for missing {missing} should name it, got {err:?}"
            );
        }
    }

    /// The reason unknown flags are rejected rather than ignored: this tool
    /// runs once, and a typo that silently dropped a name would produce an
    /// account it then refuses to let you fix.
    #[test]
    fn a_mistyped_option_is_rejected_rather_than_ignored() {
        let mut args = valid();
        args.push("--frist-name".to_string());
        args.push("Typo".to_string());
        let err = parse_args(&args).expect_err("should reject");
        assert!(err.contains("unknown option"), "got {err:?}");
    }

    #[test]
    fn company_must_be_one_of_the_enum_values() {
        let mut args = valid();
        // Replace the company value.
        let idx = args.iter().position(|a| a == "--company").unwrap();
        args[idx + 1] = "acme".to_string();
        let err = parse_args(&args).expect_err("should reject");
        assert!(
            err.contains("trojan"),
            "error should list valid ones: {err:?}"
        );
    }

    #[test]
    fn company_is_case_insensitive() {
        let mut args = valid();
        let idx = args.iter().position(|a| a == "--company").unwrap();
        args[idx + 1] = "QuikStor".to_string();
        let parsed = parse_args(&args).expect("should parse");
        assert_eq!(as_create(&parsed).2, "quikstor");
    }

    #[test]
    fn a_duplicated_option_is_rejected_rather_than_last_one_winning() {
        let mut args = valid();
        args.push("--email".to_string());
        args.push("second@b.com".to_string());
        let err = parse_args(&args).expect_err("should reject");
        assert!(err.contains("more than once"), "got {err:?}");
    }

    #[test]
    fn an_option_consuming_the_next_option_as_its_value_is_rejected() {
        let args = argv(&["--email", "--first-name", "A"]);
        let err = parse_args(&args).expect_err("should reject");
        assert!(err.contains("requires a value"), "got {err:?}");
    }

    #[test]
    fn whitespace_only_values_do_not_count_as_provided() {
        let mut args = valid();
        let idx = args.iter().position(|a| a == "--first-name").unwrap();
        args[idx + 1] = "   ".to_string();
        let err = parse_args(&args).expect_err("should reject");
        assert!(err.contains("first-name"), "got {err:?}");
    }

    #[test]
    fn an_address_without_an_at_sign_is_rejected() {
        let mut args = valid();
        let idx = args.iter().position(|a| a == "--email").unwrap();
        args[idx + 1] = "not-an-address".to_string();
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn a_bare_positional_argument_is_rejected() {
        let mut args = valid();
        args.push("stray".to_string());
        let err = parse_args(&args).expect_err("should reject");
        assert!(err.contains("unexpected argument"), "got {err:?}");
    }
}
