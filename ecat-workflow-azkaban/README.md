# ecat-workflow-azkaban

Azkaban 3.x legacy REST adapter for e-cat; the repository is archived (last commit 2023-08-29, last release 3.90.0, no 4.x). Logs in with `POST /?action=login`, caches `session.id` (body or `azkaban.browser.session.id` cookie) and re-logs in once when it expires. `definition` is a base64 zip or `{"project", "flow", "zip_base64"}`; `WorkflowId` is `project/flow` and `RunId` is the exec id. `list_runs` is `Unsupported` (no officially verified ajax endpoint); `schedule` is ignored.

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.
