// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use ecat_data::Row;

use crate::error::OrmError;

/// 列的存储类型。方言层据此映射 DDL 类型名（见 `dialect::DialectSpec::col_type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColType {
    I64,
    I32,
    F64,
    Bool,
    /// 变长文本。DDL 上 SQL Server 用 `NVARCHAR(MAX)`，其余 `TEXT`。
    Text,
    /// 二进制，`Row` 内以 base64 字符串呈现。
    Bytes,
    /// 时间戳，`Row` 内以 **RFC3339 UTC** 字符串呈现（spec §6 时间类型策略）。
    Timestamp,
    /// 纯日期，`Row` 内以 **`YYYY-MM-DD`** 呈现。**不补 UTC 午夜** —— 源数据里
    /// 没有时刻、没有时区，补出来就是凭空断言（spec:638-642）。
    Date,
    /// JSON 文本，按文本处理（spec §10 非目标：不做类型化映射）。
    Json,
}

/// 单列元数据。`Copy` + 全 `&'static`，因此可放进 `static` 数组。
#[derive(Debug, Clone, Copy)]
pub struct ColumnMeta {
    pub name: &'static str,
    pub ty: ColType,
    pub nullable: bool,
    pub pk: bool,
    pub auto_increment: bool,
}

/// 实体的自动行为标志位。值为**列名**（不是布尔）—— 标记「哪一列承担这个职责」。
#[derive(Debug, Clone, Copy)]
pub struct EntityFlags {
    pub created_at: Option<&'static str>,
    pub updated_at: Option<&'static str>,
    pub soft_delete: Option<&'static str>,
    pub version: Option<&'static str>,
}

impl EntityFlags {
    /// 无任何自动行为的空标志位。
    pub const NONE: Self = Self {
        created_at: None,
        updated_at: None,
        soft_delete: None,
        version: None,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    HasMany,
    HasOne,
    BelongsTo,
}

/// 一条关联的元数据。
///
/// - `HasMany` / `HasOne`：本表 `local_key` → 目标表 `foreign_key`
/// - `BelongsTo`：本表 `foreign_key` → 目标表 `local_key`
#[derive(Debug, Clone, Copy)]
pub struct RelationMeta {
    /// 关系名，与 derive 生成的 `XxxRelation` 枚举变体一一对应（如 `"posts"`）。
    pub name: &'static str,
    pub kind: RelationKind,
    pub target_table: &'static str,
    pub foreign_key: &'static str,
    pub local_key: &'static str,
}

/// 实体元数据。**必须 const 可构造** —— 因此列/关联是 `&'static [..]`，不是 `Vec`。
#[derive(Debug)]
pub struct EntityMeta {
    pub table: &'static str,
    pub pk: &'static str,
    pub columns: &'static [ColumnMeta],
    pub relations: &'static [RelationMeta],
    pub flags: EntityFlags,
}

impl EntityMeta {
    /// 按列名查元数据。查询构建器的**标识符白名单**就是靠它实现的（spec §5.5a）。
    pub fn column(&self, name: &str) -> Option<&'static ColumnMeta> {
        // 返回 &'static：columns 本身是 'static 切片，元素可安全提升生命周期。
        self.columns.iter().find(|c| c.name == name)
    }

    /// 非主键、且非自增的列 —— INSERT 时要写的列。
    pub fn insertable_columns(&self) -> impl Iterator<Item = &'static ColumnMeta> + '_ {
        // 不加 `.copied()`：`ColumnMeta: Copy`，但 `.copied()` 会把 `Item` 变成
        // `ColumnMeta` 而非 `&'static ColumnMeta`，与返回类型不符。
        self.columns.iter().filter(|c| !(c.pk && c.auto_increment))
    }

    /// 非主键列 —— UPDATE 时要写的列。
    pub fn updatable_columns(&self) -> impl Iterator<Item = &'static ColumnMeta> + '_ {
        self.columns.iter().filter(|c| !c.pk)
    }

    /// 按关系名查关联元数据。
    pub fn relation(&self, name: &str) -> Option<&'static RelationMeta> {
        self.relations.iter().find(|r| r.name == name)
    }
}

/// 一个可持久化的实体。
///
/// 由 `#[derive(Entity)]` 生成实现；手写实现也被支持（元数据用 `const` 静态量）。
pub trait Entity: Sized + Send + Sync {
    const TABLE: &'static str;
    const PK: &'static str;
    const META: &'static EntityMeta;

    /// 从一行构造。列缺失与 NULL 是**两种不同的错误/行为** ——
    /// `Option<T>` 字段接受 NULL，非 `Option` 字段遇到 NULL 必须报
    /// [`OrmError::UnexpectedNull`]，不得静默取默认值。
    fn from_row(row: &Row) -> Result<Self, OrmError>;

    /// 全部列（含主键）的「列名 → 值」。**按 `META.columns` 顺序**。
    /// insert 会自行剔除自增主键，update 会自行剔除主键 —— 本方法不做过滤，
    /// 这样调用方与测试都能看到完整快照。
    fn to_values(&self) -> Vec<(&'static str, serde_json::Value)>;

    /// 主键值。
    fn pk_value(&self) -> serde_json::Value;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 元数据必须是 const 可构造的 —— 这是 `EntityMeta` 用 `&'static [..]`
    /// 而非 `Vec` 的**唯一理由**。这个测试在编译期就把它钉死：
    /// 一旦有人把 columns 改回 Vec，本测试无法编译。
    static COLS: [ColumnMeta; 2] = [
        ColumnMeta {
            name: "id",
            ty: ColType::I64,
            nullable: false,
            pk: true,
            auto_increment: true,
        },
        ColumnMeta {
            name: "name",
            ty: ColType::Text,
            nullable: false,
            pk: false,
            auto_increment: false,
        },
    ];
    static META: EntityMeta = EntityMeta {
        table: "users",
        pk: "id",
        columns: &COLS,
        relations: &[],
        flags: EntityFlags {
            created_at: None,
            updated_at: None,
            soft_delete: None,
            version: None,
        },
    };

    #[test]
    fn meta_is_const_constructible() {
        assert_eq!(META.table, "users");
        assert_eq!(META.columns.len(), 2);
    }

    #[test]
    fn column_lookup_hits_and_misses() {
        assert_eq!(META.column("name").map(|c| c.ty), Some(ColType::Text));
        assert!(META.column("nope").is_none());
    }

    #[test]
    fn pk_column_is_marked() {
        let pk = META.column(META.pk).expect("pk column must exist");
        assert!(pk.pk);
    }
}
