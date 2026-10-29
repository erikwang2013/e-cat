# ecat-data-delta-sharing

Delta Sharing read-only protocol client for e-cat.

Scope: **discovery and metadata only**. This crate lists shares, schemas and
tables, reads table metadata/version, and queries a table for its file list.
It does **not** download, decode, or read the Parquet files referenced by that
list — the caller gets file URLs and metadata for an execution engine.

- Endpoints follow the [Delta Sharing protocol](https://github.com/delta-io/delta-sharing/blob/main/PROTOCOL.md):
  `GET /shares`, `GET /shares/{share}/schemas`, `GET .../tables`,
  `GET .../metadata`, `GET .../version`, `POST .../query`.
- Listing endpoints follow `nextPageToken` and aggregate `items`.
  `query_table` aggregates `files` across pages and removes `nextPageToken`.
- `query_table` supports both query response protocols, chosen from the
  `Content-Type`: the current NDJSON stream (`application/x-ndjson` /
  `application/json-seq`, one `protocol` / `metaData` / `file` /
  `endStreamAction` action per line) and the legacy
  `{ "files": [...], "nextPageToken": ... }` JSON envelope. Both the wrapped
  `{"endStreamAction": {...}}` and the bare form in the spec are accepted.
  The request sends `delta-sharing-capabilities: includeEndStreamAction=true`;
  `nextPageToken` continues with a `pageToken` request field. The result also
  carries `refreshToken` / `minUrlExpirationTimestamp` (last non-null value)
  from `endStreamAction`, for refreshing presigned URLs.
- Bearer token is required. TLS via `ecat-tls`.
- Per-client timeout, circuit breaker and concurrency cap. No metrics feature:
  this crate is not an `ecat-data` SQL/graph/etc. backend, so nothing is
  registered in the shared outbound-metrics registry.

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.
