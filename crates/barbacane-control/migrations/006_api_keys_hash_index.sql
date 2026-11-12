-- A registration looks a key up by its hash; the hash is unique and indexed.
CREATE UNIQUE INDEX idx_api_keys_hash ON api_keys(key_hash);
