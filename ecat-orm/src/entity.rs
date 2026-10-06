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

    /// 开始一个查询。`User::query()` 比 `Query::<User, Unfiltered>::new()` 好读。
    fn query() -> crate::query::Query<Self, crate::query::Unfiltered>
    where
        Self: Sized,
    {
        crate::query::Query::new()
    }

    /// 关联预加载的写回口。由派生宏按关联名分派到具体字段 ——
    /// `Entity` 泛型地访问不到 `posts` 字段，只有宏知道每个关联对应哪个字段、
    /// 目标类型是什么。
    ///
    /// **即使 `rows` 为空也必须被调用**（`set_relation(name, vec![])`）——
    /// 否则前一次加载残留在字段里的旧数据不会被清掉（静默给过时数据）。
    ///
    /// 单值关联（HasOne / BelongsTo）只取第一行，其余忽略。
    fn set_relation(&mut self, name: &str, rows: Vec<Row>) -> Result<(), OrmError>;
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

    /// INSERT 时自增主键由数据库生成，**不能**出现在列清单里。
    /// 补测理由：本方法此前零覆盖，而它决定「哪些列会进 INSERT」——
    /// 写错不会报错，只会写出多余或缺失的列。
    #[test]
    fn insertable_columns_skip_auto_increment_pk() {
        let names: Vec<_> = META.insertable_columns().map(|c| c.name).collect();
        assert_eq!(names, vec!["name"], "自增主键 id 不该出现在 INSERT 里");
    }

    /// **非自增主键必须出现在 INSERT 里** —— 手工分配的整数主键、UUID 字符串
    /// 都由调用方给值，漏掉就插不进去。
    ///
    /// 判据是 `!(pk && auto_increment)` 而不是 `!pk`：这一条把两者的区别钉死。
    #[test]
    fn insertable_columns_keep_a_non_auto_increment_pk() {
        static COLS: [ColumnMeta; 2] = [
            ColumnMeta {
                name: "id",
                ty: ColType::Text,
                nullable: false,
                pk: true,
                auto_increment: false,
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
            table: "k",
            pk: "id",
            columns: &COLS,
            relations: &[],
            flags: EntityFlags::NONE,
        };
        let names: Vec<_> = META.insertable_columns().map(|c| c.name).collect();
        assert_eq!(names, vec!["id", "name"], "非自增主键必须保留");
    }

    /// 顺序必须与 `columns` 声明顺序一致 —— 派生宏按字段顺序生成，
    /// crud 按同一顺序收集参数；顺序不一致会让参数与列错位（静默错值）。
    #[test]
    fn column_iterators_preserve_declaration_order() {
        static COLS: [ColumnMeta; 3] = [
            ColumnMeta {
                name: "a",
                ty: ColType::I64,
                nullable: false,
                pk: true,
                auto_increment: true,
            },
            ColumnMeta {
                name: "b",
                ty: ColType::I64,
                nullable: false,
                pk: false,
                auto_increment: false,
            },
            ColumnMeta {
                name: "c",
                ty: ColType::I64,
                nullable: false,
                pk: false,
                auto_increment: false,
            },
        ];
        static META: EntityMeta = EntityMeta {
            table: "t",
            pk: "a",
            columns: &COLS,
            relations: &[],
            flags: EntityFlags::NONE,
        };
        let ins: Vec<_> = META.insertable_columns().map(|c| c.name).collect();
        let upd: Vec<_> = META.updatable_columns().map(|c| c.name).collect();
        assert_eq!(ins, vec!["b", "c"], "自增主键 a 不进 INSERT，其余按声明序");
        assert_eq!(upd, vec!["b", "c"], "主键 a 不进 UPDATE，其余按声明序");
    }

    /// UPDATE 永远跳过主键 —— 包括非自增主键（主键是定位条件，不是被更新的列）。
    #[test]
    fn updatable_columns_always_skip_the_pk() {
        let names: Vec<_> = META.updatable_columns().map(|c| c.name).collect();
        assert_eq!(names, vec!["name"]);
    }

    /// 关联查找的命中与未命中。
    #[test]
    fn relation_lookup_hits_and_misses() {
        static RELS: [RelationMeta; 1] = [RelationMeta {
            name: "posts",
            kind: RelationKind::HasMany,
            target_table: "posts",
            foreign_key: "user_id",
            local_key: "id",
        }];
        static META: EntityMeta = EntityMeta {
            table: "users",
            pk: "id",
            columns: &[],
            relations: &RELS,
            flags: EntityFlags::NONE,
        };
        let r = META
            .relation("posts")
            .expect("declared relation must be found");
        assert_eq!(r.kind, RelationKind::HasMany);
        assert_eq!(r.foreign_key, "user_id");
        assert!(META.relation("nope").is_none());
    }
}
