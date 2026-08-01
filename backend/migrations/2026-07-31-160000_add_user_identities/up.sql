-- Multi-provider login identities (#1535).
--
-- Identity used to live on `users.google_id` (VARCHAR UNIQUE NOT NULL), which
-- hard-coded exactly one provider: a GitHub-only user could not satisfy the NOT
-- NULL, and two providers could in principle collide on a subject string. Move
-- identity into its own table keyed by (provider, subject) so a user can hold
-- several, and back-fill the existing Google identities before dropping the
-- column.

CREATE TABLE user_identities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- Stable provider key ("google", "github", ...). Not an enum: adding a
    -- provider must not require a migration.
    provider VARCHAR(32) NOT NULL,
    -- The provider's immutable subject for this user (OIDC `sub`, GitHub's
    -- numeric id as text). Never the email — emails change hands.
    subject VARCHAR(255) NOT NULL,
    -- Email as this provider asserted it, for auditing which identity supplied
    -- the address on `users`. Nullable: not every provider returns one.
    email VARCHAR(255),
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    -- One account per (provider, subject); a subject may only ever map to one
    -- user, so a second login through the same provider can never fork a user.
    UNIQUE (provider, subject)
);

CREATE INDEX idx_user_identities_user_id ON user_identities(user_id);

-- Back-fill every existing user's Google identity before the column goes away.
INSERT INTO user_identities (user_id, provider, subject, email)
SELECT id, 'google', google_id, email FROM users;

ALTER TABLE users DROP COLUMN google_id;

-- Emails now identify a human across providers (the account-linking rule keys
-- on a verified email), so they must be unique. Case-insensitive because
-- providers differ on casing and the allow-list check already lowercases.
--
-- Safe on existing data: logins were Google-only and keyed on a unique
-- google_id, and two Google accounts cannot share a primary email — so
-- duplicates are not reachable. If this index fails to build, the deploy has
-- duplicate users that must be merged by hand rather than silently tolerated.
CREATE UNIQUE INDEX idx_users_email_lower ON users (lower(email));
