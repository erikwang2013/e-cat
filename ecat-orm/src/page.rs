// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 分页。`paginate` 发两条查询（COUNT + 取页），`paginate_without_count`
//! 只发取页那条 —— 大表深层翻页时 `COUNT(*)` 的全表扫描比取一页还贵。

use ecat_data::SqlExecutor;

use crate::entity::Entity;
use crate::error::OrmError;
use crate::query::Query;

/// 一页数据。
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// `paginate_without_count` 时为 `None`（省掉一次 COUNT 全表扫描）。
    pub total: Option<u64>,
    /// 页码，**从 1 开始**。
    pub page: u64,
    pub per_page: u64,
}

impl<T> Page<T> {
    /// 总页数。`total` 为 `None` 或 `per_page == 0` 时返回 `None`
    /// —— 算不出来就如实说算不出来，不猜。
    pub fn total_pages(&self) -> Option<u64> {
        let total = self.total?;
        if self.per_page == 0 {
            return None;
        }
        // 向上取整：42 条 / 每页 20 = 3 页（整除会丢掉最后 2 条）。
        Some(total.div_ceil(self.per_page))
    }

    /// 还有下一页吗。**没有总数时返回 `false`** —— 不知道就不说有。
    pub fn has_next(&self) -> bool {
        self.total_pages().is_some_and(|t| self.page < t)
    }
}

impl<E: Entity, S> Query<E, S> {
    /// 取一页，并额外发一条 COUNT 得到 `total`（共 2 次查询）。
    ///
    /// COUNT 复用同一个 WHERE / JOIN，但**去掉 ORDER BY / LIMIT / OFFSET**
    /// （见 [`crate::query::sql::build_select`]）：带上它们数的就不是全量了。
    pub async fn paginate<X>(&self, db: &X, page: u64, per_page: u64) -> Result<Page<E>, OrmError>
    where
        X: SqlExecutor + ?Sized,
    {
        let count = crate::query::sql::build_select(self, db.dialect(), true);
        let rows = db
            .query_with(&count.sql, &count.params)
            .await
            .map_err(OrmError::Rdbms)?;
        // COUNT 没有别名时 PG 把列名小写化、SQL Server 干脆不给名字 ——
        // 所以 `build_select` 显式挂了 `AS "ecat_count"`，这里按名字取。
        let total = rows
            .first()
            .map(|r| crate::value::from_row_col::<i64>(r, crate::query::sql::COUNT_ALIAS))
            .transpose()?
            .unwrap_or(0)
            .max(0) as u64;

        let items = self.page_items(db, page, per_page).await?;
        Ok(Page {
            items,
            total: Some(total),
            page,
            per_page,
        })
    }

    /// 取一页但**不数总数**（1 次查询）。`total` 为 `None`，
    /// 因此 [`Page::total_pages`] / [`Page::has_next`] 也给不出答案。
    pub async fn paginate_without_count<X>(
        &self,
        db: &X,
        page: u64,
        per_page: u64,
    ) -> Result<Page<E>, OrmError>
    where
        X: SqlExecutor + ?Sized,
    {
        let items = self.page_items(db, page, per_page).await?;
        Ok(Page {
            items,
            total: None,
            page,
            per_page,
        })
    }

    async fn page_items<X>(&self, db: &X, page: u64, per_page: u64) -> Result<Vec<E>, OrmError>
    where
        X: SqlExecutor + ?Sized,
    {
        // 页码从 1 开始；`page == 0` 当作第 1 页而不是让 `page - 1` 在 u64 上下溢 panic。
        // `page` 按调用方给的原样回显（不悄悄改成 1）—— 回显的是它要的那页。
        let offset = page.saturating_sub(1).saturating_mul(per_page);
        self.clone_with_limit(per_page, offset).fetch(db).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crud::fixtures::*;
    use crate::entity::Entity;
    use crate::query::Op;
    use ecat_data::{Dialect, Row, SqlExecutor};
    use serde_json::json;

    // ---- Page 本身 ----

    #[test]
    fn page_carries_items_and_totals() {
        let p = Page {
            items: vec!["a", "b"],
            total: Some(2),
            page: 1,
            per_page: 10,
        };
        assert_eq!(p.items.len(), 2);
        assert_eq!(p.total, Some(2));
        assert_eq!(p.page, 1);
        assert_eq!(p.per_page, 10);
    }

    /// 向上取整：42 条 / 每页 20 = 3 页（整除会丢最后 2 条）。
    #[test]
    fn total_pages_rounds_up() {
        let p: Page<u8> = Page {
            items: vec![],
            total: Some(42),
            page: 1,
            per_page: 20,
        };
        assert_eq!(p.total_pages(), Some(3));
    }

    #[test]
    fn total_pages_is_exact_on_a_multiple() {
        let p: Page<u8> = Page {
            items: vec![],
            total: Some(40),
            page: 1,
            per_page: 20,
        };
        assert_eq!(p.total_pages(), Some(2));
    }

    #[test]
    fn zero_rows_yields_zero_pages() {
        let p: Page<u8> = Page {
            items: vec![],
            total: Some(0),
            page: 1,
            per_page: 20,
        };
        assert_eq!(p.total_pages(), Some(0));
    }

    /// `paginate_without_count` 没有总数 —— 算不出来就如实说算不出来，不猜。
    #[test]
    fn total_pages_is_none_without_total() {
        let p: Page<i32> = Page {
            items: vec![],
            total: None,
            page: 1,
            per_page: 20,
        };
        assert_eq!(p.total_pages(), None);
        assert!(!p.has_next(), "没有总数就没有下一页 —— 不能凭空说还有");
    }

    #[test]
    fn zero_per_page_does_not_panic() {
        let p: Page<u8> = Page {
            items: vec![],
            total: Some(10),
            page: 1,
            per_page: 0,
        };
        assert_eq!(p.total_pages(), None);
    }

    #[test]
    fn has_next_is_true_before_the_last_page() {
        let p: Page<u8> = Page {
            items: vec![],
            total: Some(30),
            page: 1,
            per_page: 20,
        };
        assert!(p.has_next());
        assert!(!Page { page: 2, ..p }.has_next(), "最后一页没有下一页");
    }

    // ---- Query::paginate ----

    /// 两行结果：既能当 COUNT 的结果（读 `ecat_count`），也能当数据行。
    fn paging_spy() -> Spy {
        let s = spy(Dialect::Sqlite, 0);
        *s.rows.lock().unwrap() = vec![Row::new(
            vec!["id".into(), "name".into(), "ecat_count".into()],
            vec![json!(1), json!("x"), json!(7)],
        )];
        s
    }

    /// 先 COUNT 再取页，`total` 从 `ecat_count` 读回。
    /// `page = 2, per_page = 10` → `OFFSET 10`（页码**从 1 开始**）。
    #[tokio::test]
    async fn paginate_issues_a_count_then_a_limited_select() {
        let s = paging_spy();
        let p = U::query()
            .filter("name", Op::Eq, "x")
            .unwrap()
            .paginate(&s, 2, 10)
            .await
            .unwrap();

        let calls = s.calls();
        assert_eq!(calls.len(), 2, "COUNT + 取页 = 两条查询");
        assert_eq!(
            calls[0].0, r#"SELECT COUNT(*) AS "ecat_count" FROM "users" WHERE "name" = ?"#,
            "COUNT 必须走同一条 WHERE，且列名要能被 Row 按名字取到"
        );
        assert_eq!(
            calls[1].0,
            r#"SELECT "id", "name" FROM "users" WHERE "name" = ? LIMIT 10 OFFSET 10"#
        );
        assert_eq!(calls[1].1, vec![json!("x")], "分页参数不得混进绑定值");

        assert_eq!(p.total, Some(7));
        assert_eq!(p.page, 2);
        assert_eq!(p.per_page, 10);
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.total_pages(), Some(1));
    }

    /// `paginate_without_count` 只发取页那一条（大表深层翻页时 `COUNT(*)`
    /// 的全表扫描比取一页还贵）。
    #[tokio::test]
    async fn paginate_without_count_skips_the_count_query() {
        let s = paging_spy();
        let p = U::query()
            .filter("name", Op::Eq, "x")
            .unwrap()
            .paginate_without_count(&s, 1, 10)
            .await
            .unwrap();
        let calls = s.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0,
            // 第 1 页的 offset 是 0，方言层把它整个省掉（`LIMIT 10`）。
            r#"SELECT "id", "name" FROM "users" WHERE "name" = ? LIMIT 10"#
        );
        assert_eq!(p.total, None);
        assert!(!p.has_next(), "没有总数时不猜下一页");
    }

    /// `page = 0` 当作第 1 页（`page - 1` 在 u64 上会下溢 panic）。
    #[tokio::test]
    async fn page_zero_is_the_first_page() {
        let s = spy(Dialect::Sqlite, 0);
        let p = U::query()
            .filter("id", Op::Eq, 1)
            .unwrap()
            .paginate_without_count(&s, 0, 10)
            .await
            .unwrap();
        assert_eq!(
            last(&s).0,
            r#"SELECT "id", "name" FROM "users" WHERE "id" = ? LIMIT 10"#
        );
        assert_eq!(p.page, 0, "回显调用方给的页码 —— 不悄悄改成 1");
    }

    /// 分页不得改写调用方已有的 ORDER BY / LIMIT —— `limit` 由分页决定，
    /// 排序保持（否则每页的顺序不同，翻页会重复/漏行）。
    #[tokio::test]
    async fn paginate_keeps_the_order_by() {
        let s = spy(Dialect::Sqlite, 0);
        U::query()
            .filter("id", Op::Eq, 1)
            .unwrap()
            .order_by("id", crate::query::Order::Desc)
            .unwrap()
            .limit(3)
            .paginate_without_count(&s, 3, 5)
            .await
            .unwrap();
        assert_eq!(
            last(&s).0,
            r#"SELECT "id", "name" FROM "users" WHERE "id" = ? ORDER BY "id" DESC LIMIT 5 OFFSET 10"#
        );
    }

    /// 真库上跑完一轮分页：条数、每页内容、`has_next` 都对得上。
    #[tokio::test]
    async fn pagination_round_trips_on_a_real_database() {
        let db = mem_sqlite("pagination").await;
        db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
            .await
            .unwrap();
        for i in 1..=5 {
            U::insert(
                &db,
                &U {
                    id: 0,
                    name: format!("u{i}"),
                },
            )
            .await
            .unwrap();
        }

        let q = || U::query().order_by("id", crate::query::Order::Asc).unwrap();
        let first = q().paginate(&db, 1, 2).await.unwrap();
        assert_eq!(first.total, Some(5));
        assert_eq!(first.total_pages(), Some(3));
        assert!(first.has_next());
        let names: Vec<_> = first.items.iter().map(|u| u.name.clone()).collect();
        assert_eq!(names, vec!["u1", "u2"], "第 1 页");

        let last_page = q().paginate(&db, 3, 2).await.unwrap();
        let names: Vec<_> = last_page.items.iter().map(|u| u.name.clone()).collect();
        assert_eq!(names, vec!["u5"], "第 3 页只剩一行");
        assert!(!last_page.has_next());

        // 越界页不是错误，是空页 —— 但 total 仍然给得出。
        let beyond = q().paginate(&db, 9, 2).await.unwrap();
        assert!(beyond.items.is_empty());
        assert_eq!(beyond.total, Some(5));

        let no_count = q().paginate_without_count(&db, 2, 2).await.unwrap();
        let names: Vec<_> = no_count.items.iter().map(|u| u.name.clone()).collect();
        assert_eq!(names, vec!["u3", "u4"], "第 2 页");
        assert_eq!(no_count.total, None);
    }
}
