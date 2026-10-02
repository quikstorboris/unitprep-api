-- A run's output may now live in BOTH places: the file saved to Dropbox and
-- a copy of the same file kept in the database. Boris (2026-10-02): the
-- results workbook should always be recoverable from OO itself, because a
-- Dropbox file can be moved, renamed or deleted afterwards (the same reason
-- the source file is stored). The earlier constraint made the two mutually
-- exclusive ("whichever export a user triggers last wins").
ALTER TABLE client_ops.tool_runs
    DROP CONSTRAINT IF EXISTS tool_runs_output_mutually_exclusive;
