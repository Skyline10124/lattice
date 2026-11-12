// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Local IPC 协议线上形状基线测试入口。
//!
//! - `gate`：确定性生成完整快照字节、逐字节比对、版本升级门禁及往返检查；
//! - `fixtures`：按领域组织的 enum 穷尽片段、结构体 `full`/`minimal` 片段与内部一致场景帧。

use std::collections::BTreeMap;

use serde::{Serialize, de::DeserializeOwned};

/// 把 enum 的变体模式与取样值绑定在同一处：
/// 1. 生成无通配符的穷尽 `match`，新增变体时直接编译失败，重复分支由编译器报不可达模式；
/// 2. 运行时断言每条样例确实落入它声明的变体分支；
/// 3. 每条样例经往返检查后作为独立片段 `fragment/<类型名>/<变体名>` 写入快照。
macro_rules! enum_fragments {
    (
        $book:expr,
        $type_slug:literal,
        $enum_ty:ty {
            $( ($variant_slug:literal, $pat:pat_param => $sample:expr) ),+ $(,)?
        }
    ) => {{
        let classify = |__value: &$enum_ty| -> &'static str {
            match __value {
                $(
                    $pat => $variant_slug,
                )+
            }
        };
        $(
            {
                let __sample: $enum_ty = $sample;
                let __matched = classify(&__sample);
                assert_eq!(
                    __matched,
                    $variant_slug,
                    "enum 片段 fragment/{}/{} 的样例未匹配声明的变体模式（实际落入分支 {:?}）",
                    $type_slug,
                    $variant_slug,
                    __matched,
                );
                $book.insert(
                    format!("fragment/{}/{}", $type_slug, $variant_slug),
                    &__sample,
                );
            }
        )+
    }};
}

mod fixtures;
mod gate;

#[derive(Default)]
pub(super) struct CaseBook {
    cases: BTreeMap<String, String>,
}

impl CaseBook {
    pub(super) fn insert<T>(&mut self, name: impl Into<String>, value: &T)
    where
        T: Serialize + DeserializeOwned,
    {
        let name = name.into();
        let encoded = gate::round_trip_json(&name, value);
        let previous = self.cases.insert(name.clone(), encoded);
        assert!(
            previous.is_none(),
            "快照样例名称重复: {name}（请确保每个片段名与场景帧名唯一）"
        );
    }

    pub(super) fn insert_struct_pair<T>(&mut self, slug: &str, full: &T, minimal: &T)
    where
        T: Serialize + DeserializeOwned,
    {
        self.insert(format!("fragment/{slug}/full"), full);
        self.insert(format!("fragment/{slug}/minimal"), minimal);
    }

    fn into_cases(self) -> BTreeMap<String, String> {
        self.cases
    }
}

#[test]
fn local_ipc_wire_shape_matches_v63_baseline() {
    let mut book = CaseBook::default();
    fixtures::register_all(&mut book);
    gate::check_or_update_snapshot(&book.into_cases());
}
