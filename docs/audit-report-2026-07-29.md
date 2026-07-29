# e-cat 代码审查与测试报告

**日期**: 2026-07-29  
**分支**: main  
**项目**: e-cat (Rust workspace)

---

## 审查范围

审查了工作区中全部 5 个 crate 的所有源代码：

| Crate | 文件 |
|-------|------|
| `ecat-protos` | `build.rs`, `src/lib.rs`, `proto/errors.proto`, `proto/metadata.proto` |
| `ecat-errors` | `src/lib.rs`, `src/codes.rs` |
| `ecat-metadata` | `src/lib.rs` |
| `ecat-encoding` | `src/lib.rs`, `src/json.rs`, `src/proto.rs` |
| `ecat-logging` | `src/lib.rs` |

---

## 发现的问题及修复

### 1. [Bug] `codec_from_content_type` 对未知类型静默回退到 JSON

- **文件**: `ecat-encoding/src/lib.rs:59`
- **严重程度**: 中等
- **问题**: 当传入不支持的 Content-Type（如 `"application/msgpack"`）时，函数静默返回 JSON codec，调用方无从得知使用了错误的编码
- **修复**: 将返回类型从 `CodecBox` 改为 `Result<CodecBox, CodecError>`，未知类型返回 `Err`

```rust
// 修复前
pub fn codec_from_content_type(ct: &str) -> CodecBox {
    match ct {
        "application/json" => CodecBox::Json(...),
        "application/protobuf" | "application/x-protobuf" => CodecBox::Proto(...),
        _ => CodecBox::Json(...),  // 静默回退
    }
}

// 修复后
pub fn codec_from_content_type(ct: &str) -> Result<CodecBox, CodecError> {
    match ct {
        "application/json" => Ok(CodecBox::Json(...)),
        "application/protobuf" | "application/x-protobuf" => Ok(CodecBox::Proto(...)),
        other => Err(CodecError::Decode(format!("unsupported content type: {other}"))),
    }
}
```

### 2. [Minor] `ecat-logging::init` 未使用参数

- **文件**: `ecat-logging/src/lib.rs:4`
- **严重程度**: 低
- **问题**: `_service_name` 参数被接受但完全未使用（通过下划线前缀抑制警告）
- **修复**: 移除未使用的参数，函数签名从 `pub fn init(_service_name: &str)` 改为 `pub fn init()`

### 3. [Missing] 缺少 Debug 派生

- **文件**: `ecat-encoding/src/json.rs`, `ecat-encoding/src/proto.rs`, `ecat-encoding/src/lib.rs`
- **严重程度**: 低
- **问题**: `JsonCodec`、`ProtoCodec` 和 `CodecBox` 缺少 `Debug` 实现，影响错误处理和测试
- **修复**: 为三个类型添加 `#[derive(Debug)]`

---

## 测试覆盖

### 修复前

| Crate | 测试数 |
|-------|--------|
| `ecat-encoding` | 0 |
| `ecat-errors` | 4 |
| `ecat-logging` | 0 |
| `ecat-metadata` | 0 |
| `ecat-protos` | 0 |
| **合计** | **4** |

### 修复后

| Crate | 测试数 | 新增 |
|-------|--------|------|
| `ecat-encoding` | 15 | JsonCodec 编解码往返、非法数据解码、content_type；CodecBox JSON/Proto 编解码分发；codec_for 枚举映射；codec_from_content_type 正常及错误路径；Encoding 变体相等性 |
| `ecat-errors` | 4 | （原有测试） |
| `ecat-logging` | 1 | init 冒烟测试 |
| `ecat-metadata` | 9 | new/get/set、覆盖写入、trace_id；From\<HeaderMap\>（含非UTF8值跳过）；From\<MetadataMap\>（ASCII 及二进制跳过）；IntoIterator 完整遍历 |
| `ecat-protos` | 0 | （仅 protobuf 代码生成，无逻辑需测试） |
| **合计** | **29** | **+25** |

---

## 验证结果

```
cargo test   → 29 passed, 0 failed
cargo check  → 0 errors, 0 warnings
cargo clippy → 0 warnings
```

---

## 修改文件清单

| 文件 | 变更 |
|------|------|
| `ecat-encoding/src/lib.rs` | `codec_from_content_type` 返回 Result + `#[derive(Debug)]` for CodecBox + 15 个测试 |
| `ecat-encoding/src/json.rs` | `#[derive(Debug)]` for JsonCodec |
| `ecat-encoding/src/proto.rs` | `#[derive(Debug)]` for ProtoCodec |
| `ecat-logging/src/lib.rs` | 移除未使用的 `_service_name` 参数 + 1 个测试 |
| `ecat-metadata/src/lib.rs` | 9 个测试 |
