# ecat-graphql

GraphQL integration for e-cat.

Part of the [e-cat](https://github.com/erik/e-cat) ecosystem.

## 限制 (Limitations)

当前实现为手写的最小解析器，**仅支持顶层单字段查询**（如 `{ hello }`）：

- 不支持嵌套字段、别名、参数 (arguments)、变量 (variables) 与 fragment；
- mutation 同样仅支持单字段；
- 请勿在生产服务中将其暴露为通用 GraphQL 端点，如需完整功能请接入成熟 GraphQL 引擎（如 async-graphql / juniper）。

The resolver map is keyed by top-level field name; anything beyond a single
top-level field (no nesting, aliases, arguments, or fragments) is rejected.
