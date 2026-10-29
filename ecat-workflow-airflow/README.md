# ecat-workflow-airflow

Apache Airflow 3.x stable REST API adapter for e-cat (v2 default, v1 optional). `base_url` is the host root (`http://host:8080`); the adapter strips a trailing `/api/v1` or `/api/v2` and appends `/api/{version}/`. v2 triggers require `logical_date` (null unless given in params), use JWT via `bearer_token` (obtained from `POST /auth/token`), and cancel by PATCHing `state=failed`; v1 omits `logical_date`.

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.
