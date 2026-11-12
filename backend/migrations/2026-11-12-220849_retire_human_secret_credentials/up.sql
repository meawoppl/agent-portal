-- Human vault access is OAuth-browser-only. Keep historical records for audit,
-- but revoke capabilities issued by the retired CLI enrollment flow.
UPDATE secret_credentials
SET revoked_at = COALESCE(revoked_at, NOW());

UPDATE secret_enrollments
SET state = 'denied', expires_at = LEAST(expires_at, NOW())
WHERE state IN ('pending', 'approved');
