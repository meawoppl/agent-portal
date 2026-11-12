CREATE TABLE secret_access_request_groups (
    request_id UUID PRIMARY KEY REFERENCES secret_access_requests(id) ON DELETE CASCADE,
    group_id UUID NOT NULL,
    group_size INTEGER NOT NULL CHECK (group_size BETWEEN 1 AND 16),
    position INTEGER NOT NULL CHECK (position >= 0 AND position < 16),
    UNIQUE (group_id, position)
);
CREATE INDEX secret_access_request_groups_group_id_idx ON secret_access_request_groups(group_id);
