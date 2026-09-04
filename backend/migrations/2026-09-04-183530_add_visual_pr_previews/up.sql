-- Durable storage for admin visual-PR preview SVGs (generated on a launcher
-- host from a throwaway shallow clone; the portal is the long-term home).
CREATE TABLE visual_pr_previews (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo VARCHAR(255) NOT NULL,
    pr_number BIGINT NOT NULL,
    svg TEXT NOT NULL,
    model VARCHAR(100),
    generated_on VARCHAR(255),
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    UNIQUE (repo, pr_number)
);
