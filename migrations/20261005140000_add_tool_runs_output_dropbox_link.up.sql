-- The Dropbox share link for a run's saved summary file, captured in the
-- background when the file is written so later consumers (the ClickUp
-- duplicate-check comment) do not have to ask Dropbox for it again.
-- NULL until the link exists, and reset whenever a new file is saved for
-- the run. Nullable on purpose: a link is a convenience, never required
-- (Dropbox may refuse one, and the comment falls back to the file's path).
ALTER TABLE client_ops.tool_runs ADD COLUMN output_dropbox_link TEXT;
