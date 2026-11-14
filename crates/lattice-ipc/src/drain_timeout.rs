// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `--drain-timeout` 时长值与解析函数；仅供命令行参数解析，不参与线上协议序列化。

use std::time::Duration;

/// 停机排空上限；只用于本地进程参数传递，不实现序列化。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrainTimeout(Duration);

impl DrainTimeout {
    pub const DEFAULT: Self = Self(Duration::from_secs(60));

    pub const fn as_duration(self) -> Duration {
        self.0
    }
}

impl Default for DrainTimeout {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl From<DrainTimeout> for Duration {
    fn from(value: DrainTimeout) -> Self {
        value.0
    }
}

impl std::str::FromStr for DrainTimeout {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        parse_drain_timeout(raw)
    }
}

/// 解析 `--drain-timeout` 参数：非负整数，单位支持 `ms`、`s`、`m`，无单位按秒计。
pub fn parse_drain_timeout(raw: &str) -> Result<DrainTimeout, String> {
    if raw.is_empty() {
        return Err("排空时长不能为空".into());
    }
    enum Unit {
        Millis,
        Secs,
        Mins,
    }
    let (digits, unit) = if let Some(digits) = raw.strip_suffix("ms") {
        (digits, Unit::Millis)
    } else if let Some(digits) = raw.strip_suffix('s') {
        (digits, Unit::Secs)
    } else if let Some(digits) = raw.strip_suffix('m') {
        (digits, Unit::Mins)
    } else {
        (raw, Unit::Secs)
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("排空时长必须是非负整数，单位只支持 ms、s 或 m".into());
    }
    let value = digits
        .parse::<u64>()
        .map_err(|_| "排空时长数值溢出".to_string())?;
    let duration = match unit {
        Unit::Millis => Duration::from_millis(value),
        Unit::Secs => Duration::from_secs(value),
        Unit::Mins => value
            .checked_mul(60)
            .map(Duration::from_secs)
            .ok_or_else(|| "排空时长数值溢出".to_string())?,
    };
    Ok(DrainTimeout(duration))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_timeout_accepts_supported_units_and_rejects_invalid_inputs() {
        assert_eq!(DrainTimeout::DEFAULT.as_duration(), Duration::from_secs(60));
        for (raw, expected) in [
            ("300ms", Duration::from_millis(300)),
            ("60s", Duration::from_secs(60)),
            ("2m", Duration::from_secs(120)),
            ("60", Duration::from_secs(60)),
            ("0", Duration::ZERO),
            ("0ms", Duration::ZERO),
            ("0s", Duration::ZERO),
            ("0m", Duration::ZERO),
        ] {
            let parsed = parse_drain_timeout(raw)
                .unwrap_or_else(|err| panic!("{raw} 应被接受，但失败：{err}"));
            assert_eq!(parsed.as_duration(), expected, "{raw} 解析结果不符");
            assert_eq!(Duration::from(parsed), expected);
            assert_eq!(raw.parse::<DrainTimeout>().unwrap(), parsed);
        }

        assert_eq!(parse_drain_timeout("").unwrap_err(), "排空时长不能为空");
        for invalid in [
            "-1", "1h", "1.5s", "+5", " 60s", "60s ", "60 s", "\t60", "ms", "s", "m",
        ] {
            assert_eq!(
                parse_drain_timeout(invalid).unwrap_err(),
                "排空时长必须是非负整数，单位只支持 ms、s 或 m",
                "{invalid:?} 错误信息不符"
            );
        }
        for overflow in ["18446744073709551616", "307445734561825861m"] {
            assert_eq!(
                parse_drain_timeout(overflow).unwrap_err(),
                "排空时长数值溢出",
                "{overflow:?} 应报数值溢出"
            );
        }
    }
}
