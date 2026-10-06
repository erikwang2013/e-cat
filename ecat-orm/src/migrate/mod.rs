// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 迁移系统。**执行位置在用户代码**（spec §7）—— `ecat-cli` 不链接用户代码，
//! 看不到实体定义，无从知道建什么表。
//!
//! ```ignore
//! let db = SqlxClient::connect(&url).await?;
//! let m = Migrator::new(&db)
//!     .add("001_users", create_table::<User>())
//!     .add("002_posts", create_table::<Post>());
//! m.status().await?;
//! m.run().await?;
//! ```
//!
//! 迁移是**一条 SQL**：方言在 `run()` / `down()` 时才由连接的 `dialect()` 决定
//! （见 [`MigrationBuilder`]），所以同一份迁移列表换库不用重写。

use ecat_data::{Dialect, SqlExecutor};

use crate::error::OrmError;

mod ddl;
mod version;

use ddl::parse_version;
pub use ddl::{create_table, create_table_sql, drop_table, drop_table_sql};

#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod tests;

/// 一条**已按某个方言解析好**的迁移。
#[derive(Debug, Clone)]
pub struct Migration {
    pub version: i64,
    pub name: String,
    pub sql: String,
    /// 反向 SQL。`None` = 不可逆（`down` 会报
    /// [`OrmError::MigrationIrreversible`]），**不是**「什么都不用做」。
    pub reverse_sql: Option<String>,
}

impl Migration {
    /// 造一条不可逆的迁移。要可逆用 [`MigrationBuilder::with_reverse`]。
    pub fn new(version: i64, name: String, sql: String) -> Self {
        Self {
            version,
            name,
            sql,
            reverse_sql: None,
        }
    }

    /// 反向 SQL。未提供时返回 [`OrmError::MigrationIrreversible`] ——
    /// 静默跳过会让「回滚」看起来成功了，而数据库状态一点没变。
    pub fn reverse(&self) -> Result<&str, OrmError> {
        self.reverse_sql
            .as_deref()
            .ok_or_else(|| OrmError::MigrationIrreversible(self.name.clone()))
    }
}

/// 一条**尚未绑定方言**的迁移。
///
/// 存的是「怎么生成 SQL」而不是 SQL 本身：建表语句按方言不同（引号、自增拼法、
/// 有没有 `IF NOT EXISTS`），而迁移列表在用户代码里只写一次、连接却是运行期
/// 才给的。绑死方言的话，换库就要重写整份迁移列表。
pub struct MigrationBuilder {
    forward: Box<dyn Fn(Dialect) -> String + Send + Sync>,
    reverse: Option<Box<dyn Fn(Dialect) -> String + Send + Sync>>,
}

impl MigrationBuilder {
    /// 自定义迁移（ALTER / 数据回填等）。`create_table` / `drop_table` 是它的两个
    /// 实体工厂版。
    pub fn new(forward: impl Fn(Dialect) -> String + Send + Sync + 'static) -> Self {
        Self {
            forward: Box::new(forward),
            reverse: None,
        }
    }

    /// 附上反向 SQL。**没有它 [`Migrator::down`] 就报错**，因为
    /// 「DROP TABLE 再 CREATE 回来」会丢数据 —— 不能替调用方猜。
    pub fn with_reverse(
        mut self,
        reverse: impl Fn(Dialect) -> String + Send + Sync + 'static,
    ) -> Self {
        self.reverse = Some(Box::new(reverse));
        self
    }

    fn build(&self, version: i64, name: String, dialect: Dialect) -> Migration {
        Migration {
            version,
            name,
            sql: (self.forward)(dialect),
            reverse_sql: self.reverse.as_ref().map(|f| f(dialect)),
        }
    }
}

/// 版本表与已注册迁移的对比结果。三组都按版本号升序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationStatus {
    /// 两边都有 —— 已应用。
    pub applied: Vec<i64>,
    /// 迁移列表里有、版本表里没有 —— 待应用。
    pub pending: Vec<i64>,
    /// 版本表里有、迁移列表里没有 —— 数据库与代码不一致，**必须报出来**。
    pub unknown: Vec<i64>,
}

/// 对比「版本表里的」与「本次注册的」版本号。
///
/// 三组都**按版本号排序**而不是按输入顺序：声明顺序在多次编辑后很容易与实际
/// 版本号不符，而执行顺序由版本号决定。
pub fn classify(applied: &[i64], known: &[i64]) -> MigrationStatus {
    let mut in_both: Vec<i64> = applied
        .iter()
        .copied()
        .filter(|v| known.contains(v))
        .collect();
    let mut pending: Vec<i64> = known
        .iter()
        .copied()
        .filter(|v| !applied.contains(v))
        .collect();
    let mut unknown: Vec<i64> = applied
        .iter()
        .copied()
        .filter(|v| !known.contains(v))
        .collect();
    in_both.sort_unstable();
    pending.sort_unstable();
    unknown.sort_unstable();
    MigrationStatus {
        applied: in_both,
        pending,
        unknown,
    }
}

/// 迁移器。
///
/// `add` 只登记「名字 + 生成器」：版本号解析与 SQL 生成推迟到 `status` / `run` /
/// `down`（那三个方法要访问数据库、要等连接的方言），因此链式 `add` 不需要 `?`
/// —— 名字写错会在第一次调那三个方法时报出来。
pub struct Migrator<'a, X: SqlExecutor + ?Sized> {
    db: &'a X,
    declared: Vec<(String, MigrationBuilder)>,
}

impl<'a, X: SqlExecutor + ?Sized> Migrator<'a, X> {
    pub fn new(db: &'a X) -> Self {
        Self {
            db,
            declared: Vec::new(),
        }
    }

    /// 登记一条迁移。名字形如 `"001_users"`：版本号取**首个下划线之前的数字前缀**，
    /// 解析失败会报 [`OrmError::InvalidMigrationName`]（见 [`self::ddl`]）。
    pub fn add(mut self, name: &str, builder: MigrationBuilder) -> Self {
        self.declared.push((name.to_string(), builder));
        self
    }

    /// 已注册迁移的版本号（含重复、含乱序 —— 对比交给 [`classify`]）。
    fn known_versions(&self) -> Result<Vec<i64>, OrmError> {
        self.declared
            .iter()
            .map(|(name, _)| parse_version(name))
            .collect()
    }

    /// 已注册迁移 + 按当前连接的方言解析出的 SQL。
    fn resolve(&self) -> Result<Vec<Migration>, OrmError> {
        let dialect = self.db.dialect();
        self.declared
            .iter()
            .map(|(name, b)| Ok(b.build(parse_version(name)?, name.clone(), dialect)))
            .collect()
    }

    /// 对比版本表与已注册的迁移。
    ///
    /// 版本表不存在时**先建**：否则全新数据库上第一次 `status()` 报的是
    /// 「表不存在」，而那正是最需要看到「全部 pending」的场景。
    pub async fn status(&self) -> Result<MigrationStatus, OrmError> {
        version::ensure_version_table(self.db).await?;
        let applied = version::read_applied(self.db).await?;
        Ok(classify(&applied, &self.known_versions()?))
    }

    /// 按版本号升序执行未应用的迁移。
    ///
    /// **单个失败即中止**：不吞错、也不继续后面的 —— 半应用的迁移集比一条都没
    /// 应用更难排查，继续执行只会把状态搅得更乱。返回的是**第一个**失败项，
    /// 已成功的前面几条保持已应用（迁移不是原子的，版本表就是账本）。
    pub async fn run(&self) -> Result<(), OrmError> {
        version::ensure_version_table(self.db).await?;
        let applied = version::read_applied(self.db).await?;
        let mut migrations = self.resolve()?;
        migrations.sort_by_key(|m| m.version);
        for m in migrations {
            if applied.contains(&m.version) {
                continue;
            }
            self.apply(&m, &m.sql).await?;
            version::record(self.db, m.version, &m.name).await?;
        }
        Ok(())
    }

    /// 回滚一个已应用的版本：执行它的反向 SQL，然后从版本表删掉那一行
    /// （与 [`Migrator::run`] 的「记录版本」互逆）。
    ///
    /// 未应用**或**未注册的版本都返回 [`OrmError::MigrationNotApplied`]；
    /// 没带反向 SQL 的返回 [`OrmError::MigrationIrreversible`]。
    pub async fn down(&self, version: i64) -> Result<(), OrmError> {
        version::ensure_version_table(self.db).await?;
        let applied = version::read_applied(self.db).await?;
        if !applied.contains(&version) {
            return Err(OrmError::MigrationNotApplied(version));
        }
        let migrations = self.resolve()?;
        let m = migrations
            .iter()
            .find(|m| m.version == version)
            .ok_or(OrmError::MigrationNotApplied(version))?;
        let reverse = m.reverse()?;
        self.apply(m, reverse).await?;
        version::remove(self.db, version).await
    }

    /// 执行一条迁移 SQL，失败时**包上版本号与名字** —— 只报原始 SQL 错误的话，
    /// 在几十条迁移里定位不到是哪一条炸的。
    async fn apply(&self, m: &Migration, sql: &str) -> Result<(), OrmError> {
        self.db
            .execute(sql)
            .await
            .map_err(|e| OrmError::Migration {
                version: m.version,
                name: m.name.clone(),
                source: Box::new(OrmError::Rdbms(e)),
            })?;
        Ok(())
    }
}
