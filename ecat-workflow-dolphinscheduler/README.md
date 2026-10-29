# ecat-workflow-dolphinscheduler

Apache DolphinScheduler 3.4.3 REST API adapter for e-cat. `base_url` is `http://host:12345/dolphinscheduler` (no version segment); `api_style` defaults to `"workflow"` (3.3+: `workflow-definition`/`workflow-instances`) and accepts `"legacy"` for 3.2 and earlier. A `WorkflowId` is a definition code and a `RunId` an instance id; legacy triggers that return only a `triggerCode` are polled into an instance id. Auth uses the `token` header.

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.
