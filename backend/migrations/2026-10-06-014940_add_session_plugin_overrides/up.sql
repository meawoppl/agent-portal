CREATE TABLE session_plugin_overrides (
    session_id UUID PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    overrides JSONB NOT NULL DEFAULT '[]'::jsonb,
    CONSTRAINT plugin_overrides_array CHECK (jsonb_typeof(overrides) = 'array')
);
