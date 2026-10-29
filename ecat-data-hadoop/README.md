# ecat-data-hadoop

Hadoop WebHDFS storage client for e-cat (`StorageClient`).

The `bucket` argument is treated as a top-level HDFS directory. Kerberos/SPNEGO
is not supported; use `user.name` or a delegation token.

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.
