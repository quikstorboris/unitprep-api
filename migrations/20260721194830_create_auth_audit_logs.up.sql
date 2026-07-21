CREATE TABLE auth_audit_logs (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_type TEXT NOT NULL,
    actor_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    target_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    metadata JSONB NOT NULL DEFAULT '{}',
    before_state JSONB,
    after_state JSONB,
    ip_address INET,
    user_agent TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX auth_audit_logs_actor_user_id_idx ON auth_audit_logs (actor_user_id);
CREATE INDEX auth_audit_logs_target_user_id_idx ON auth_audit_logs (target_user_id);
CREATE INDEX auth_audit_logs_created_at_idx ON auth_audit_logs (created_at);

CREATE FUNCTION prevent_audit_log_mutation()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'auth_audit_logs is append-only: % not permitted', TG_OP;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER auth_audit_logs_no_update
    BEFORE UPDATE ON auth_audit_logs
    FOR EACH ROW
    EXECUTE FUNCTION prevent_audit_log_mutation();

CREATE TRIGGER auth_audit_logs_no_delete
    BEFORE DELETE ON auth_audit_logs
    FOR EACH ROW
    EXECUTE FUNCTION prevent_audit_log_mutation();
