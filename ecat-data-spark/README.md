# ecat-data-spark

Apache Spark SQL over the Apache Livy REST API client for e-cat.

One `SparkClient` serializes statements on its cached Livy SQL session; see the
crate documentation for the concurrency contract.

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.
