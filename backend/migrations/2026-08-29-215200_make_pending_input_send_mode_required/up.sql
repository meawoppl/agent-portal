UPDATE pending_inputs
SET send_mode = 'normal'
WHERE send_mode IS NULL;

ALTER TABLE pending_inputs
ALTER COLUMN send_mode SET DEFAULT 'normal',
ALTER COLUMN send_mode SET NOT NULL;
