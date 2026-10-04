-- Sync protocol 2 (dino-sync PROTOCOL = 2). Every record carries a token, deletes too, sealed to
-- its stamp and whether it's a delete; a server without the account key can't forge, restamp or
-- replay one. Version 1 records can't be opened under version 2's sealing, and nothing launched
-- holds them, so they go: devices send their settings again.
DELETE FROM sync_records;
UPDATE sync_heads SET bytes = 0, updated_at = now();
ALTER TABLE sync_records ALTER COLUMN value SET NOT NULL;
ALTER TABLE sync_records ADD COLUMN deleted boolean NOT NULL DEFAULT false;

-- Approval is commit, then reveal (dino_sync::approval): four messages, each kept as the device
-- sent it. The server reads none of them beyond their shape, and can't open the grant.
--   pending   the new device committed (commitment)
--   responded a signed-in device answered (response: its public key and nonce)
--   revealed  the new device revealed what it committed to (reveal), to that answer only
--   granted   the signed-in device sealed the account key to it (grant_body)
--   denied
DELETE FROM sync_approvals;
ALTER TABLE sync_approvals DROP COLUMN public_key;
ALTER TABLE sync_approvals DROP COLUMN approver_key;
ALTER TABLE sync_approvals ADD COLUMN commitment jsonb NOT NULL;
ALTER TABLE sync_approvals ADD COLUMN response jsonb;
ALTER TABLE sync_approvals ADD COLUMN reveal jsonb;
