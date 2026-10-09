//! Concurrent-load measurement for the efficiency refactor's A4-A6 work
//! (`run_blocking` for CPU-heavy handlers, idle-gated pool pings, the
//! sealing-before-transaction change).
//!
//! The refactor's per-chunk benchmarks were all single-request. What they
//! could not show is whether the async workers stay responsive and the
//! pool stays healthy while several operators run heavy tools at once.
//! This drives the REAL router (real durable session stores, real vendor
//! registry, real RLS, real session cookies) over real HTTP against the
//! local `test-db`, from a server runtime with a deliberately small number
//! of worker threads, and measures:
//!
//! * a cheap **canary** (`GET /health/whoami`: session resolution + RLS, no
//!   CPU) every 20 ms while dedup checks run -- if a CPU-heavy handler
//!   were blocking the async workers, the canary's tail latency explodes;
//! * the heavy operation itself (`POST /dedup/check`, upload + parse +
//!   detect + match + view + encrypted tool-run insert + durable session
//!   save), per concurrency level;
//! * a facility detail read (`GET /clients/.../facilities/...`), the
//!   multi-query page operators keep open;
//! * the peak number of connections checked out of the pool.
//!
//! It only reports; there is no timing assertion (the machine decides the
//! numbers). It does assert the load produced no 5xx / transport errors.
//!
//! ```text
//! TEST_DATABASE_URL=postgres://app_service:app_service@127.0.0.1:5433/unitprep_test \
//!   cargo test --release --bin unitprep -- --ignored --nocapture concurrent_load_report
//! ```
//!
//! Knobs (environment): `LOAD_WORKERS` server worker threads (default 4),
//! `LOAD_SECONDS` per level (12), `LOAD_ROWS` rows per upload (2400),
//! `LOAD_LEVELS` concurrent dedup operators, comma separated (1,4,8,16).
//! Point `TEST_DATABASE_URL` through `dev-tools/latency_proxy.py` to repeat
//! the run with a remote-database round trip.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use unitprep_core::durable_session_store::DurableSessionStore;
use unitprep_core::vendor_format::ContentType;
use uuid::Uuid;

use super::clickup_db_tests::{create_user, superuser_pool};
use super::test_support::empty_state;
use super::AppState;
use crate::application::dedup_session_service::DedupSession;
use crate::client_ops;

const SESSION_COOKIE: &str = "unitprep_session";

fn knob<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct Server {
    addr: SocketAddr,
    pool: sqlx::PgPool,
}

/// The real app, on its own runtime with `worker_threads` async workers, so
/// the load generator below cannot steal its threads.
fn start_server(worker_threads: usize) -> Server {
    // Warnings and errors only, so a 5xx in the table comes with its cause.
    let _ = tracing_subscriber::fmt()
        .with_env_filter("unitprep=warn,sqlx=warn")
        .try_init();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(worker_threads)
            .enable_all()
            .build()
            .expect("server runtime");
        rt.block_on(async move {
            // The PRODUCTION pool policy (20 connections, idle-gated ping,
            // 10 s acquire timeout), not `connect_test()`'s 5-connection
            // pool: a smaller pool would make the numbers pessimistic.
            let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
            assert!(!url.contains("neon.tech"), "never against Neon");
            let options: sqlx::postgres::PgConnectOptions = url.parse().expect("url");
            let db = crate::db::pool_options(Duration::from_secs(30)).connect_lazy_with(options);
            let state = AppState {
                dedup_sessions: Arc::new(DurableSessionStore::<DedupSession>::with_timeout(
                    db.clone(),
                    "dedup_session",
                    Duration::from_secs(3600),
                )),
                unit_vendors: client_ops::vendor_format::initial_cache(&db, ContentType::Units)
                    .await,
                tenant_vendors: client_ops::vendor_format::initial_cache(&db, ContentType::Tenants)
                    .await,
                tenant_file_meta: client_ops::vendor_file_meta::initial_cache(
                    &db,
                    ContentType::Tenants,
                )
                .await,
                db: db.clone(),
                ..empty_state()
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            tx.send((listener.local_addr().expect("addr"), db))
                .expect("report address");
            axum::serve(
                listener,
                super::router(state).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("serve");
        });
    });
    let (addr, pool) = rx.recv().expect("server start");
    Server { addr, pool }
}

struct Fixture {
    cookie: String,
    company_id: Uuid,
    facility_id: Uuid,
}

async fn make_fixture() -> Fixture {
    let superuser = superuser_pool();
    let user = create_user(&superuser, "loadtest").await;
    sqlx::query(
        "INSERT INTO auth.user_roles (user_id, role_id, granted_by)
         SELECT $1, id, $1 FROM auth.roles WHERE key = 'onboarding_manager'",
    )
    .bind(user)
    .execute(&superuser)
    .await
    .expect("grant role");

    let (raw, hash) = crate::auth::generate_token();
    sqlx::query(
        "INSERT INTO auth.sessions (user_id, token_hash, expires_at)
         VALUES ($1, $2, now() + interval '2 hours')",
    )
    .bind(user)
    .bind(&hash)
    .execute(&superuser)
    .await
    .expect("session");

    let company_id: Uuid = sqlx::query_scalar(
        "INSERT INTO clients.companies (legal_name, source) VALUES ('Load Test Co', 'manual') RETURNING id",
    )
    .fetch_one(&superuser)
    .await
    .expect("company");
    let facility_id: Uuid = sqlx::query_scalar(
        "INSERT INTO clients.facilities (company_id, name, source)
         VALUES ($1, 'Load Test Facility', 'manual') RETURNING id",
    )
    .bind(company_id)
    .fetch_one(&superuser)
    .await
    .expect("facility");

    Fixture {
        cookie: format!("{SESSION_COOKIE}={raw}"),
        company_id,
        facility_id,
    }
}

fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// A QSX/QMS-shaped tenant export built from the dedup crate's synthetic
/// facility, so it goes through the same registry detection a real file does.
fn tenant_csv(rows: usize) -> Vec<u8> {
    let mut out = String::from(
        "CustNumb,TenantId,UnitNumber,FirtLast,FirstName,LastName,PhoneNumber,Email,\
         AddressStreet1,AddressCity,AddressState,AddressPostalCode\n",
    );
    for r in unitprep_dedup::synthetic::synthetic_facility(rows) {
        let fields = [
            &r.cust_numb,
            &r.tenant_id,
            &r.unit_number,
            &r.first_last,
            &r.first_name,
            &r.last_name,
            &r.phone_number,
            &r.email,
            &r.address_street1,
            &r.address_city,
            &r.address_state,
            &r.address_postal_code,
        ];
        let line: Vec<String> = fields.iter().map(|f| csv_escape(f)).collect();
        out.push_str(&line.join(","));
        out.push('\n');
    }
    out.into_bytes()
}

fn multipart_body(file_name: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let boundary = "----loadtestboundary7MA4YWxkTrZu0gW";
    let mut body = Vec::with_capacity(bytes.len() + 256);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{file_name}\"; \
             filename=\"{file_name}\"\r\nContent-Type: text/csv\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

#[derive(Clone, Copy)]
struct Sample {
    ms: f64,
    status: u16, // 0 = transport error
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

fn summarize(label: &str, samples: &[Sample]) -> usize {
    let mut ok: Vec<f64> = samples
        .iter()
        .filter(|s| (200..300).contains(&s.status))
        .map(|s| s.ms)
        .collect();
    ok.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let bad_5xx = samples
        .iter()
        .filter(|s| s.status == 0 || s.status >= 500)
        .count();
    let other = samples.len() - ok.len() - bad_5xx;
    println!(
        "    {label:<18} n={:<5} p50={:>8.1}  p95={:>8.1}  p99={:>8.1}  max={:>8.1} ms   non-2xx={other} 5xx/transport={bad_5xx}",
        samples.len(),
        percentile(&ok, 0.50),
        percentile(&ok, 0.95),
        percentile(&ok, 0.99),
        ok.last().copied().unwrap_or(f64::NAN),
    );
    bad_5xx
}

async fn get_sample(client: &reqwest::Client, url: &str, cookie: &str) -> Sample {
    let started = Instant::now();
    let status = match client.get(url).header("Cookie", cookie).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let body = response.bytes().await.unwrap_or_default();
            if status >= 500 {
                eprintln!(
                    "    {status} on GET {url}: {}",
                    String::from_utf8_lossy(&body)
                );
            }
            status
        }
        Err(err) => {
            eprintln!("    transport error on GET {url}: {err:?}");
            0
        }
    };
    Sample {
        ms: started.elapsed().as_secs_f64() * 1000.0,
        status,
    }
}

async fn dedup_sample(
    client: &reqwest::Client,
    url: &str,
    cookie: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Sample {
    let started = Instant::now();
    let status = match client
        .post(url)
        .header("Cookie", cookie)
        .header("Content-Type", content_type)
        .body(body)
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status().as_u16();
            let body = response.bytes().await.unwrap_or_default();
            if status >= 500 {
                eprintln!(
                    "    {status} on dedup check: {}",
                    String::from_utf8_lossy(&body)
                );
            }
            status
        }
        Err(err) => {
            eprintln!("    transport error on dedup check: {err:?}");
            0
        }
    };
    Sample {
        ms: started.elapsed().as_secs_f64() * 1000.0,
        status,
    }
}

/// Reports what the level stored, then deletes it and vacuums so the next
/// level starts from an empty table. The test-db's data directory is a
/// RAM-backed tmpfs: left alone, a few hundred 2 MB runs fill it, and a full
/// disk makes Postgres PANIC (it happened on the first full run).
async fn reclaim_level_data(fixture: &Fixture, since: chrono::DateTime<chrono::Utc>) {
    let superuser = superuser_pool();
    let (runs, stored): (i64, Option<i64>) = sqlx::query_as(
        "SELECT count(*), sum(coalesce(pg_column_size(source_bytes), 0)
                            + coalesce(pg_column_size(records_encrypted), 0))::bigint
           FROM client_ops.tool_runs WHERE facility_id = $1",
    )
    .bind(fixture.facility_id)
    .fetch_one(&superuser)
    .await
    .expect("measure stored runs");
    if runs > 0 {
        println!(
            "    stored per dedup run: {:.0} KB (source + records, encrypted) x {runs} runs",
            stored.unwrap_or(0) as f64 / runs as f64 / 1024.0
        );
    }
    sqlx::query("DELETE FROM client_ops.tool_runs WHERE facility_id = $1")
        .bind(fixture.facility_id)
        .execute(&superuser)
        .await
        .expect("delete level runs");
    sqlx::query(
        "DELETE FROM auth.durable_sessions WHERE kind = 'dedup_session' AND created_at >= $1",
    )
    .bind(since)
    .execute(&superuser)
    .await
    .expect("delete level sessions");
    for table in ["client_ops.tool_runs", "auth.durable_sessions"] {
        sqlx::query(&format!("VACUUM {table}"))
            .execute(&superuser)
            .await
            .expect("vacuum");
    }
}

async fn remove_fixture(fixture: &Fixture) {
    let superuser = superuser_pool();
    sqlx::query("DELETE FROM clients.facilities WHERE id = $1")
        .bind(fixture.facility_id)
        .execute(&superuser)
        .await
        .expect("delete facility");
    sqlx::query("DELETE FROM clients.companies WHERE id = $1")
        .bind(fixture.company_id)
        .execute(&superuser)
        .await
        .expect("delete company");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the local test-db and takes minutes -- see module doc"]
async fn concurrent_load_report() {
    let _ = dotenvy::from_filename(".env.local");
    std::env::set_var(
        "CLIENT_PII_ENCRYPTION_KEY",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );

    let workers: usize = knob("LOAD_WORKERS", 4);
    let seconds: u64 = knob("LOAD_SECONDS", 12);
    let rows: usize = knob("LOAD_ROWS", 2400);
    let max_runs: usize = knob("LOAD_MAX_RUNS", 400);
    let test_started = chrono::Utc::now();
    let levels: Vec<usize> = std::env::var("LOAD_LEVELS")
        .unwrap_or_else(|_| "1,4,8,16".to_string())
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();

    let server = start_server(workers);
    let fixture = make_fixture().await;
    let base = format!("http://{}", server.addr);
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(64)
        .build()
        .expect("client");
    let whoami = format!("{base}/health/whoami");
    let detail = format!(
        "{base}/clients/{}/facilities/{}",
        fixture.company_id, fixture.facility_id
    );
    let dedup_url = format!("{base}/dedup/check?facility_id={}", fixture.facility_id);

    let csv = tenant_csv(rows);
    let (content_type, body) = multipart_body("tenants.csv", &csv);
    println!(
        "\n=== concurrent load: {workers} server worker threads, {rows}-row uploads ({} KB), {seconds}s per level ===",
        csv.len() / 1024
    );

    // Smoke: every operation must succeed once before it is measured.
    let smoke = [
        (
            "whoami",
            get_sample(&client, &whoami, &fixture.cookie).await,
        ),
        (
            "facility detail",
            get_sample(&client, &detail, &fixture.cookie).await,
        ),
        (
            "dedup check",
            dedup_sample(
                &client,
                &dedup_url,
                &fixture.cookie,
                &content_type,
                body.clone(),
            )
            .await,
        ),
    ];
    for (name, s) in smoke {
        println!("  smoke {name:<16} status {} in {:.1} ms", s.status, s.ms);
        assert!(
            (200..300).contains(&s.status),
            "{name} did not succeed in the smoke run (status {})",
            s.status
        );
    }

    let mut total_bad = 0usize;
    for &operators in &levels {
        println!("\n  --- {operators} concurrent dedup operator(s) ---");

        // Idle canary first, for comparison.
        let mut idle = Vec::new();
        for _ in 0..200 {
            idle.push(get_sample(&client, &whoami, &fixture.cookie).await);
        }
        total_bad += summarize("canary (idle)", &idle);

        let stop = Arc::new(AtomicBool::new(false));
        let peak_checked_out = Arc::new(AtomicUsize::new(0));
        let level_started = Instant::now();
        let deadline = level_started + Duration::from_secs(seconds);
        let completed = Arc::new(AtomicUsize::new(0));

        let sampler = {
            let pool = server.pool.clone();
            let stop = stop.clone();
            let peak = peak_checked_out.clone();
            tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    let in_use = (pool.size() as usize).saturating_sub(pool.num_idle());
                    peak.fetch_max(in_use, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
        };

        let canary = {
            let (client, url, cookie, stop) = (
                client.clone(),
                whoami.clone(),
                fixture.cookie.clone(),
                stop.clone(),
            );
            tokio::spawn(async move {
                let mut samples = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    samples.push(get_sample(&client, &url, &cookie).await);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                samples
            })
        };

        let detail_readers: Vec<_> = (0..2)
            .map(|_| {
                let (client, url, cookie, stop) = (
                    client.clone(),
                    detail.clone(),
                    fixture.cookie.clone(),
                    stop.clone(),
                );
                tokio::spawn(async move {
                    let mut samples = Vec::new();
                    while !stop.load(Ordering::Relaxed) {
                        samples.push(get_sample(&client, &url, &cookie).await);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    samples
                })
            })
            .collect();

        let checkers: Vec<_> = (0..operators)
            .map(|_| {
                let (client, url, cookie, ct, body) = (
                    client.clone(),
                    dedup_url.clone(),
                    fixture.cookie.clone(),
                    content_type.clone(),
                    body.clone(),
                );
                let completed = completed.clone();
                tokio::spawn(async move {
                    let mut samples = Vec::new();
                    // Bounded by time AND by run count: every check stores
                    // about 2 MB, and the test-db lives in RAM (tmpfs).
                    while Instant::now() < deadline && completed.load(Ordering::Relaxed) < max_runs
                    {
                        samples.push(dedup_sample(&client, &url, &cookie, &ct, body.clone()).await);
                        completed.fetch_add(1, Ordering::Relaxed);
                    }
                    samples
                })
            })
            .collect();

        let mut dedup_samples = Vec::new();
        for handle in checkers {
            dedup_samples.extend(handle.await.expect("checker"));
        }
        stop.store(true, Ordering::Relaxed);
        let canary_samples = canary.await.expect("canary");
        let mut detail_samples = Vec::new();
        for handle in detail_readers {
            detail_samples.extend(handle.await.expect("reader"));
        }
        sampler.await.expect("sampler");

        total_bad += summarize("canary (loaded)", &canary_samples);
        total_bad += summarize("facility detail", &detail_samples);
        total_bad += summarize("dedup check", &dedup_samples);
        let per_sec = dedup_samples.len() as f64 / level_started.elapsed().as_secs_f64();
        println!(
            "    dedup throughput {per_sec:.1} checks/s; peak pool connections checked out: {} of {}",
            peak_checked_out.load(Ordering::Relaxed),
            server.pool.options().get_max_connections()
        );
        reclaim_level_data(&fixture, test_started).await;
    }

    remove_fixture(&fixture).await;
    assert_eq!(
        total_bad, 0,
        "the load produced 5xx or transport errors -- see the table above"
    );
}
