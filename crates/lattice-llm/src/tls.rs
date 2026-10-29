// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::LlmError;

/// 按启动环境载入可用根；个别证书失败只记警告，返回的 DER 供 provider 与 Tool Host 共用。
pub fn load_tls_roots_der() -> Result<Vec<Vec<u8>>, LlmError> {
    let roots = rustls_native_certs::load_native_certs();
    for error in roots.errors {
        tracing::warn!(%error, "failed to load a TLS root certificate");
    }
    let mut store = rustls::RootCertStore::empty();
    let roots: Vec<_> = roots
        .certs
        .into_iter()
        .filter_map(|cert| match store.add(cert.clone()) {
            Ok(()) => Some(cert.as_ref().to_vec()),
            Err(error) => {
                tracing::warn!(%error, "failed to load a TLS root certificate");
                None
            }
        })
        .collect();
    if roots.is_empty() {
        return Err(LlmError::Config {
            message: "no usable TLS root certificates".into(),
        });
    }
    Ok(roots)
}
