# ecat-workflow-oozie

Apache Oozie 5.x REST API v2 adapter for e-cat; Oozie was retired in 2025-02 and moved to the Apache Attic in 2025-04. Uses HTTP Basic auth; Kerberos/SPNEGO is not supported. `definition` is the job configuration XML (or `{"xml": "..."}`); `params` objects are escaped and injected as `<property>` entries before `</configuration>`. `create` submits (PREP), `trigger` starts the same job id, non-null trigger params are `Unsupported`, and `schedule` is ignored.

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.
