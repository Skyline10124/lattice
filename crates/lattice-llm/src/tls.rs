// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

/// 按启动环境载入平台根证书的原始 DER，供 provider 与 Tool Host 共用。
///
/// 这里不解析、不过滤，也不因为集合为空而失败：大小与有效性的筛选和警告只在
/// [`crate::RequestGateway::new`] 的选根步骤里做一次，“没有可用的根证书”也只在那里判定。
pub fn load_tls_roots_der() -> Vec<Vec<u8>> {
    native_roots_der(rustls_native_certs::load_native_certs())
}

/// 按与网关相同的规则筛出可用的根证书；不经过网关的调用方（如 xtask）用它在调用处判断是否可用。
pub fn usable_tls_roots_der(roots_der: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    crate::client::load_bounded_roots(roots_der).0
}

/// 载入结果到 DER 集合的纯函数：记录载入错误，证书原样透传，可能为空。
fn native_roots_der(result: rustls_native_certs::CertificateResult) -> Vec<Vec<u8>> {
    for error in &result.errors {
        tracing::warn!(%error, "failed to load a TLS root certificate");
    }
    result
        .certs
        .into_iter()
        .map(|cert| cert.as_ref().to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use rustls::pki_types::CertificateDer;
    use rustls_native_certs::{CertificateResult, Error, ErrorKind};

    use super::*;
    use crate::test_tls::TLS;

    #[test]
    fn empty_load_result_is_an_empty_set_not_an_error() {
        let roots = native_roots_der(CertificateResult::default());
        assert!(roots.is_empty(), "empty load result must give an empty set");
    }

    #[test]
    fn load_result_entries_pass_through_unfiltered() {
        let oversized = vec![0x30; 70_000];
        let invalid = vec![1, 2, 3];
        let mut result = CertificateResult::default();
        result.certs = vec![
            CertificateDer::from(TLS.ca_der.clone()),
            CertificateDer::from(invalid.clone()),
            CertificateDer::from(oversized.clone()),
        ];
        result.errors.push(Error {
            context: "test",
            kind: ErrorKind::Os(Box::new(std::io::Error::other("unreadable store"))),
        });
        let roots = native_roots_der(result);
        assert_eq!(
            roots,
            vec![TLS.ca_der.clone(), invalid, oversized],
            "loader must not parse, filter or reorder roots; selection belongs to the gateway"
        );
    }
}
