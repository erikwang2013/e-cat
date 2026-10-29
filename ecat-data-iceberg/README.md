# ecat-data-iceberg

Apache Iceberg REST Catalog client for e-cat.

Scope: **catalog metadata only**. This crate lists namespaces and tables and can
create/drop namespaces and tables through the Iceberg REST API. It does **not**
read or decode Iceberg data files (Parquet, Avro, ORC), manifests, or snapshots —
callers receive raw metadata JSON and use an execution engine for the data plane.

- Multi-level namespaces are joined with the unit separator and URL-encoded, e.g.
  `["prod", "analytics"]` -> `prod%1Fanalytics`.
- Bearer token (preferred) or basic auth; TLS via `ecat-tls`.
- Per-client timeout, circuit breaker and concurrency cap. No metrics feature:
  this crate is not an `ecat-data` SQL/graph/etc. backend, so nothing is
  registered in the shared outbound-metrics registry.
- Requires a catalog mounted under `/v1`; a `prefix` override from `/v1/config`
  is not applied automatically (include it in `base_url`).

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.
