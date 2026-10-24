-- Sharding migration checksum (read-only, MySQL 8.4/9).
-- Template: verify.sh substitutes {{TABLE}} with an identifier it validated against
-- ^[A-Za-z_][A-Za-z0-9_]*$ (safe to backtick-interpolate: no backticks/semicolons).
-- Ranges are contiguous ranges of CRC32(<primary key>) in [0, 2^32):
-- bucket = CRC32(pk) DIV 268435456, so bucket k covers [k*2^28, (k+1)*2^28).
-- Sections: ROW_COUNTS, PK_RANGES, CHECKSUMS, ORPHANS.
SET SESSION group_concat_max_len = 1048576;
-- Columns are compared via CAST(... AS CHAR); pin UTC so the session time zone of the
-- source and target connections cannot make identical TIMESTAMP values hash differently.
SET SESSION time_zone = '+00:00';

-- SECTION: ROW_COUNTS
SELECT 'ROW_COUNTS' AS section, '{{TABLE}}' AS tbl, COUNT(*) AS row_count
FROM `{{TABLE}}`;

-- SECTION: PK_RANGES
-- Primary key columns in key order, plus the 16 CRC32 range boundaries both sides share.
SET @pk_cols = (
  SELECT GROUP_CONCAT(CONCAT('`', REPLACE(COLUMN_NAME, '`', '``'), '`')
                      ORDER BY SEQ_IN_INDEX SEPARATOR ', ')
  FROM information_schema.STATISTICS
  WHERE TABLE_SCHEMA = DATABASE()
    AND TABLE_NAME = '{{TABLE}}'
    AND INDEX_NAME = 'PRIMARY'
);
-- A table without a PRIMARY KEY must fail loudly instead of comparing empty ranges;
-- the placeholder makes the prepared statement below fail with "Unknown column".
SET @pk_cols = IFNULL(@pk_cols, '`__missing_primary_key__`');
WITH RECURSIVE buckets (bucket) AS (
  SELECT 0 UNION ALL SELECT bucket + 1 FROM buckets WHERE bucket < 15
)
SELECT 'PK_RANGES' AS section, '{{TABLE}}' AS tbl, @pk_cols AS pk_columns,
       bucket, bucket * 268435456 AS crc32_low, (bucket + 1) * 268435456 - 1 AS crc32_high
FROM buckets
ORDER BY bucket;

-- SECTION: CHECKSUMS
-- Per bucket: COUNT(*) and SUM(CRC32(CONCAT_WS('|', <all columns>))) over the rows in
-- that bucket. Each field is tagged N (NULL) or V (value), length-prefixed and joined
-- with '|', so NULL vs CHAR(0), or a value containing '|', cannot alias another row.
SET @cols = (
  SELECT GROUP_CONCAT(CONCAT(
           'CONCAT(IF(`', REPLACE(COLUMN_NAME, '`', '``'), '` IS NULL, ''N'', ''V''), ',
           'LENGTH(COALESCE(CAST(`', REPLACE(COLUMN_NAME, '`', '``'), '` AS CHAR), CHAR(0))), ',
           'CHAR(58), COALESCE(CAST(`', REPLACE(COLUMN_NAME, '`', '``'), '` AS CHAR), CHAR(0)))')
         ORDER BY ORDINAL_POSITION SEPARATOR ', ')
  FROM information_schema.COLUMNS
  WHERE TABLE_SCHEMA = DATABASE()
    AND TABLE_NAME = '{{TABLE}}'
);
SET @cols = IFNULL(@cols, '`__missing_table__`');
SET @sql = CONCAT(
  'SELECT ''CHECKSUMS'' AS section, bucket, COUNT(*) AS row_count, SUM(row_crc) AS checksum ',
  'FROM (SELECT CRC32(CONCAT_WS(''|'', ', @pk_cols, ')) DIV 268435456 AS bucket, ',
  'CRC32(CONCAT_WS(''|'', ', @cols, ')) AS row_crc ',
  'FROM `{{TABLE}}`) AS b GROUP BY bucket ORDER BY bucket'
);
PREPARE checksum_stmt FROM @sql;
EXECUTE checksum_stmt;
DEALLOCATE PREPARE checksum_stmt;

-- SECTION: ORPHANS
-- Foreign-key orphans: rows of {{TABLE}} where every FK column is non-NULL (MySQL only
-- enforces the constraint then) but no parent row matches in the same schema.
-- Cross-schema and differently-sharded parents are out of scope; each skipped FK still
-- emits a visible "(skipped: cross-schema)" row, so run this per shard knowing that.
SET @orphans_same = (
  SELECT GROUP_CONCAT(CONCAT(
           'SELECT ''ORPHANS'' AS section, ''', REPLACE(fk.constraint_name, '''', ''''''),
           ''' AS constraint_name, ''', REPLACE(fk.ref_table, '''', ''''''),
           ''' AS parent_table, COUNT(*) AS orphan_rows ',
           'FROM `{{TABLE}}` AS c LEFT JOIN `', REPLACE(fk.ref_table, '`', '``'),
           '` AS p ON ', fk.on_clause,
           ' WHERE ', fk.nn_clause, ' AND p.`', fk.first_ref_col, '` IS NULL')
         ORDER BY fk.constraint_name SEPARATOR ' UNION ALL ')
  FROM (
    SELECT k.CONSTRAINT_NAME AS constraint_name,
           k.REFERENCED_TABLE_NAME AS ref_table,
           GROUP_CONCAT(CONCAT('c.`', REPLACE(k.COLUMN_NAME, '`', '``'),
                               '` = p.`', REPLACE(k.REFERENCED_COLUMN_NAME, '`', '``'), '`')
                        ORDER BY k.ORDINAL_POSITION SEPARATOR ' AND ') AS on_clause,
           GROUP_CONCAT(CONCAT('c.`', REPLACE(k.COLUMN_NAME, '`', '``'), '` IS NOT NULL')
                        ORDER BY k.ORDINAL_POSITION SEPARATOR ' AND ') AS nn_clause,
           MAX(CASE WHEN k.ORDINAL_POSITION = 1
                    THEN REPLACE(k.REFERENCED_COLUMN_NAME, '`', '``') END) AS first_ref_col
    FROM information_schema.KEY_COLUMN_USAGE AS k
    WHERE k.TABLE_SCHEMA = DATABASE()
      AND k.TABLE_NAME = '{{TABLE}}'
      AND k.REFERENCED_TABLE_SCHEMA = DATABASE()
      AND k.REFERENCED_TABLE_NAME IS NOT NULL
    GROUP BY k.CONSTRAINT_NAME, k.REFERENCED_TABLE_NAME
  ) AS fk
);
-- Same counting contract for skipped FKs would require reading another schema, so emit a
-- constant row instead: the skip is visible in both sides' output and in any diff.
SET @orphans_skipped = (
  SELECT GROUP_CONCAT(CONCAT(
           'SELECT ''ORPHANS'' AS section, ''', REPLACE(k.CONSTRAINT_NAME, '''', ''''''),
           ''' AS constraint_name, ''',
           REPLACE(CONCAT(k.REFERENCED_TABLE_SCHEMA, '.', k.REFERENCED_TABLE_NAME), '''', ''''''),
           ''' AS parent_table, ''(skipped: cross-schema)'' AS orphan_rows')
         ORDER BY k.CONSTRAINT_NAME SEPARATOR ' UNION ALL ')
  FROM (
    SELECT DISTINCT CONSTRAINT_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME
    FROM information_schema.KEY_COLUMN_USAGE
    WHERE TABLE_SCHEMA = DATABASE()
      AND TABLE_NAME = '{{TABLE}}'
      AND REFERENCED_TABLE_NAME IS NOT NULL
      AND (REFERENCED_TABLE_SCHEMA IS NULL OR REFERENCED_TABLE_SCHEMA <> DATABASE())
  ) AS k
);
SET @orphan_sql = CONCAT_WS(' UNION ALL ', NULLIF(@orphans_same, ''), NULLIF(@orphans_skipped, ''));
SET @orphan_sql = IF(@orphan_sql IS NULL OR @orphan_sql = '',
  'SELECT ''ORPHANS'' AS section, ''(none)'' AS constraint_name, ''(none)'' AS parent_table, 0 AS orphan_rows',
  CONCAT('SELECT * FROM (', @orphan_sql, ') AS o ORDER BY 2, 3'));
PREPARE orphan_stmt FROM @orphan_sql;
EXECUTE orphan_stmt;
DEALLOCATE PREPARE orphan_stmt;
