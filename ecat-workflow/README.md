# ecat-workflow

Unified workflow orchestration contract for e-cat.

`WorkflowClient` covers create / trigger / status / cancel / list across
Airflow, DolphinScheduler, Azkaban, and Oozie. Adapters declare their real
capabilities; unsupported operations return `WorkflowError::Unsupported`.
`WorkflowHttp` provides the shared outbound shell: TLS, Basic/Bearer auth,
custom headers, timeout, circuit breaker, and concurrency limit.

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.
