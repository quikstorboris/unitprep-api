-- Read-only: how much space stored tool runs take, by tool and by age.
-- Run against a database you are allowed to read (the dev branch, or prod
-- through scripts/prod_db_status.sh's connection), never as a migration:
--   psql "$DATABASE_URL" -f scripts/tool_runs_storage.sql
-- See RUNBOOK.md ("Stored tool runs (retention)") for what to do with it.
-- Needs a role that can read the table through its row-level security (an
-- admin/developer session or the owner role); a restricted role sees fewer
-- rows and so under-reports.

SELECT tool,
       count(*)                                        AS runs,
       pg_size_pretty(sum(pg_column_size(t.*))::bigint) AS stored_size,
       pg_size_pretty((avg(pg_column_size(t.*)))::bigint) AS avg_per_run,
       min(created_at)::date                           AS oldest,
       max(created_at)::date                           AS newest
  FROM client_ops.tool_runs t
 GROUP BY tool
 ORDER BY sum(pg_column_size(t.*)) DESC;

SELECT CASE
         WHEN created_at >= now() - interval '30 days'  THEN '0-30 days'
         WHEN created_at >= now() - interval '90 days'  THEN '30-90 days'
         WHEN created_at >= now() - interval '180 days' THEN '90-180 days'
         ELSE 'older than 180 days'
       END                                              AS age,
       count(*)                                         AS runs,
       pg_size_pretty(sum(pg_column_size(t.*))::bigint) AS stored_size
  FROM client_ops.tool_runs t
 GROUP BY 1
 ORDER BY min(created_at) DESC;

SELECT pg_size_pretty(pg_total_relation_size('client_ops.tool_runs')) AS table_total_with_toast_and_indexes;
