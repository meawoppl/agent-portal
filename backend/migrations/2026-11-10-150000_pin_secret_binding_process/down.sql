ALTER TABLE secret_bindings DROP CONSTRAINT IF EXISTS secret_binding_process_required;
ALTER TABLE secret_bindings DROP COLUMN proxy_token_id;
