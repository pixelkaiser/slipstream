CREATE TABLE IF NOT EXISTS file_based_mcp_server_activations (
    installation_uuid TEXT NOT NULL PRIMARY KEY,
    last_modified_at TIMESTAMP NOT NULL
);
