# Warp Session Sharing Relay

Self-hosted in-memory relay for no-cloud Warp session sharing.

## Endpoints

- `GET /health`
- WebSocket `GET /sessions/create`
- WebSocket `GET /sessions/join/{session_id}?pwd={session_secret}`
- WebSocket `GET /sessions/{session_id}/resume`

The relay uses Warp's pinned `session-sharing-protocol` crate. It keeps v1 state in memory only:
session id, link secret, reconnect token, scrollback, active prompt, window size, participant list,
roles, ordered terminal events, selections, input state, and link access role.

Cloud identity features are intentionally unsupported here. Team access, email guests, pending
guests, and account-backed ACL operations return protocol error responses.

## Build

```sh
docker build -t warp-session-sharing-relay crates/session-sharing-relay
```

## Run

```sh
docker run --rm -p 8788:8788 warp-session-sharing-relay
```

Then start Warp with:

```sh
WARP_NO_CLOUD=1 WARP_SESSION_SHARING_SERVER_URL=ws://127.0.0.1:8788
```

The existing local multi-agent service remains separate on port `8787`.

## Notes

- The relay is trusted infrastructure operated by the user or team.
- TLS is expected to be terminated by a reverse proxy for internet deployments.
- Sessions do not survive relay restarts.
