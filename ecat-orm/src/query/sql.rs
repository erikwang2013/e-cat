// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! SQL 文本与绑定参数**同时生成**。
//!
//! 为什么不分两步：`$1` / `@P1` 这类编号占位符的编号依赖参数**顺序**，
//! 而顺序又由 WHERE 子句的结构决定。先生成 SQL、再按同样顺序收集参数的写法，
//! 一旦某处漏收或多收（比如软删除条件被算进参数位），编号就会整体错位 ——
//! 在编号占位符方言上是**静默取错值**，不是报错。

use ecat_data::Dialect;
use serde_json::Value;

use super::Expr;
use super::Query;
use crate::dialect::DialectSpec;
use crate::dialect::Limit;
use crate::dialect::lookup;
use crate::entity::Entity;
use crate::entity::EntityMeta;

/// 生成好的语句与它的参数。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Built {
    pub sql: String,
    pub params: Vec<Value>,
}

/// COUNT 查询的列别名。`Row` 只能按列名取值，所以这个名字是
/// `build_select` 与 `page::paginate` 之间的契约 —— 两边共用同一个常量。
pub(crate) const COUNT_ALIAS: &str = "ecat_count";

/// 顺次发号，避免手写计数器。
pub(crate) struct Placeholders<'a> {
    spec: &'a dyn DialectSpec,
    next: usize,
}

impl<'a> Placeholders<'a> {
    pub(crate) fn new(spec: &'a dyn DialectSpec) -> Self {
        Self { spec, next: 0 }
    }

    /// 取下一个占位符并**推进编号**。
    pub(crate) fn take(&mut self) -> String {
        self.next += 1;
        self.spec.placeholder(self.next)
    }

    /// 已发出的个数。与 `params.len()` 对账用（见 `build_select` 末尾的哨兵）。
    pub(crate) fn used(&self) -> usize {
        self.next
    }
}

/// 渲染 WHERE 子句，**同时**产出参数。两者必须同源同序（见本模块文件头）。
///
/// 返回 `None` 表示没有任何条件（调用方据此决定要不要写 `WHERE`）。
/// **软删除闸门也在这里面**（按 `with_trashed` 决定加不加 `<sd> IS NULL`），
/// 这样删除路径与查询路径的软删除行为**不可能不一致**。
///
/// `ph` / `params` 以 `&mut` 传入而不是函数内新建：调用方在此之前可能已占用参数位，
/// 函数内新建会让编号从头开始、与调用方已生成的部分**撞号**。
pub(crate) fn render_where(
    meta: &'static EntityMeta,
    filters: &[Expr],
    with_trashed: bool,
    spec: &dyn DialectSpec,
    ph: &mut Placeholders<'_>,
    params: &mut Vec<Value>,
) -> Option<String> {
    let mut conds: Vec<String> = Vec::new();

    // 软删除闸门。**注意：它不分配占位符** —— 是字面量 IS NULL。
    // 分配了会让用户参数整体错位一位（`$1` 绑到 NULL 上，查询静默返回空集）。
    if let Some(sd) = meta.flags.soft_delete
        && !with_trashed
    {
        conds.push(format!("{} IS NULL", spec.quote(sd)));
    }

    for f in filters {
        match f {
            Expr::Cmp { column, op, value } => {
                let p = ph.take();
                conds.push(format!("{} {} {p}", spec.quote(column), op.as_sql()));
                params.push(value.clone());
            }
            Expr::In {
                column,
                values,
                negated,
            } => {
                let q_col = spec.quote(column);
                if values.is_empty() {
                    // `IN ()` 是语法错误；空集合语义上恒假。
                    conds.push(if *negated {
                        "1 = 1".into()
                    } else {
                        "1 = 0".into()
                    });
                } else {
                    let list = values
                        .iter()
                        .map(|v| {
                            let p = ph.take();
                            params.push(v.clone());
                            p
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let not = if *negated { "NOT " } else { "" };
                    conds.push(format!("{q_col} {not}IN ({list})"));
                }
            }
            Expr::Null { column, negated } => {
                let q_col = spec.quote(column);
                conds.push(if *negated {
                    format!("{q_col} IS NOT NULL")
                } else {
                    format!("{q_col} IS NULL")
                });
            }
            Expr::Raw(expr) => conds.push(expr.clone()),
        }
    }

    if conds.is_empty() {
        None
    } else {
        Some(conds.join(" AND "))
    }
}

/// `for_count` 为真时生成 COUNT 查询：**去掉 ORDER BY / LIMIT / OFFSET**。
///
/// COUNT 列**显式取别名 `ecat_count`**：不取名字时 PostgreSQL 会把 `COUNT(*)`
/// 小写化成 `count`、SQL Server 的未命名聚合列干脆没有名字，而 `Row` 只能按
/// 名字取值 —— 那样 `paginate` 在真库上读不到 total。
pub(crate) fn build_select<E: Entity, S>(q: &Query<E, S>, d: Dialect, for_count: bool) -> Built {
    let spec = lookup(d);
    let meta = E::META;
    let mut ph = Placeholders::new(spec);
    let mut params: Vec<Value> = Vec::new();

    let cols = if for_count {
        format!("COUNT(*) AS {}", spec.quote(COUNT_ALIAS))
    } else {
        meta.columns
            .iter()
            .map(|c| spec.quote(c.name))
            .collect::<Vec<_>>()
            .join(", ")
    };

    // 分页片段：COUNT 不带（见「裁决 C」——它是 prefix + suffix）。
    // 「没设 limit」以 `None` 传下去，由方言决定省略 `LIMIT` / `FETCH` ——
    // **不能在校验口填 `u64::MAX`**：那会产出 `LIMIT 18446744073709551615`
    // （MSSQL 是 `FETCH NEXT … ROWS ONLY`），超出 BIGINT，真库拒收。
    // 两个都没设置时也走同一条路：`limit_clause(None, 0, _)` 返回空片段。
    let limit = if for_count {
        Limit::none()
    } else {
        spec.limit_clause(q.limit, q.offset.unwrap_or(0), !q.orders.is_empty())
    };

    let mut sql = format!(
        "SELECT {}{cols} FROM {}",
        limit.prefix,
        spec.quote(meta.table)
    );

    for (kind, table, on) in &q.joins {
        // 表名与 ON 条件都是调用方给的字符串 —— 信任边界在调用方（见 `Query::join`）。
        sql.push_str(&format!(" {} {} ON {on}", kind.as_sql(), spec.quote(table)));
    }

    let where_sql = render_where(meta, &q.filters, q.with_trashed, spec, &mut ph, &mut params);
    if let Some(w) = where_sql {
        sql.push_str(" WHERE ");
        sql.push_str(&w);
    }

    // ---- ORDER BY（COUNT 不带）----
    if !for_count && !q.orders.is_empty() {
        let parts = q
            .orders
            .iter()
            .map(|o| format!("{} {}", spec.quote(&o.column), o.dir.as_sql()))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" ORDER BY {parts}"));
    }

    sql.push_str(&limit.suffix);

    debug_assert_eq!(
        ph.used(),
        params.len(),
        "占位符数与参数数必须相等 —— 不等就是某处漏收/多收了参数"
    );

    Built { sql, params }
}

// 测试见 `query/sql_tests.rs` —— 放在本文件里会把它顶过 500 行硬上限。
