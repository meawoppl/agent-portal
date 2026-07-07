DROP TABLE forward_subdomains;

ALTER TABLE session_forwards DROP CONSTRAINT session_forwards_session_id_key;
ALTER TABLE session_forwards ADD CONSTRAINT session_forwards_session_id_port_key UNIQUE (session_id, port);
