-- Delivery-tracking id for pending inputs (#1236). Carried from the web
-- client's outbox so (a) replay after a proxy reconnect preserves delivery
-- tracking instead of degrading to content reconciliation, and (b) the
-- backend can deduplicate an input the client resends after a reconnect.
ALTER TABLE pending_inputs ADD COLUMN client_msg_id UUID;
