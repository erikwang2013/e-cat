# ecat-data-hbase

Apache HBase REST (JSON CellSet) wide-column client for e-cat
(`WideColumnClient`).

Basic auth is supported; Kerberos/SPNEGO is not. Values are stored as UTF-8 for
strings and JSON text for other types; see the crate documentation for the exact
encoding contract.

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.
