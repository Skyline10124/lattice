// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 场景用的 TLS 身份：临时 CA 与它签发的服务端证书（`localhost`、`127.0.0.1`）。
//!
//! 产品只信任提交端 `SSL_CERT_FILE` 指向的根，所以每个场景把 CA 写进自己的目录再指过去；
//! 本地进程没有任何系统级信任变化。证书由 `openssl` 命令生成，用完的私钥文件随临时目录删除。

use std::io;
use std::process::Command;
use std::sync::{Arc, OnceLock};

use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// 一个 CA 与它签发的服务端证书、私钥。
#[derive(Clone)]
pub struct TlsIdentity {
    ca_pem: Vec<u8>,
    ca_der: Vec<u8>,
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
}

impl TlsIdentity {
    /// 生成一套新的 CA 与服务端证书，有效期两天。
    pub fn generate() -> io::Result<Self> {
        let dir = tempfile::tempdir()?;
        std::fs::write(
            dir.path().join("leaf.ext"),
            "basicConstraints=critical,CA:FALSE\n\
             keyUsage=critical,digitalSignature\n\
             extendedKeyUsage=serverAuth\n\
             subjectAltName=DNS:localhost,IP:127.0.0.1\n",
        )?;
        let ec = [
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
        ];
        let ca: Vec<&str> = ["req", "-x509"]
            .into_iter()
            .chain(ec)
            .chain([
                "-keyout",
                "ca-key.pem",
                "-out",
                "ca.pem",
                "-days",
                "2",
                "-config",
                "/dev/null",
                "-subj",
                "/CN=LATTICE harness CA",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-addext",
                "keyUsage=critical,keyCertSign,cRLSign",
            ])
            .collect();
        let leaf: Vec<&str> = ["req", "-new"]
            .into_iter()
            .chain(ec)
            .chain([
                "-keyout",
                "key.pem",
                "-out",
                "leaf.csr",
                "-config",
                "/dev/null",
                "-subj",
                "/CN=localhost",
            ])
            .collect();
        for args in [
            ca,
            leaf,
            vec![
                "x509",
                "-req",
                "-in",
                "leaf.csr",
                "-CA",
                "ca.pem",
                "-CAkey",
                "ca-key.pem",
                "-CAcreateserial",
                "-out",
                "cert.pem",
                "-days",
                "2",
                "-extfile",
                "leaf.ext",
            ],
            vec![
                "x509", "-in", "cert.pem", "-outform", "DER", "-out", "cert.der",
            ],
            vec!["x509", "-in", "ca.pem", "-outform", "DER", "-out", "ca.der"],
            vec![
                "pkcs8", "-topk8", "-nocrypt", "-in", "key.pem", "-outform", "DER", "-out",
                "key.der",
            ],
        ] {
            run_openssl(&args, dir.path())?;
        }
        Ok(Self {
            ca_pem: std::fs::read(dir.path().join("ca.pem"))?,
            ca_der: std::fs::read(dir.path().join("ca.der"))?,
            cert_der: std::fs::read(dir.path().join("cert.der"))?,
            key_der: std::fs::read(dir.path().join("key.der"))?,
        })
    }

    /// 本进程共用的一套身份；`ScenarioWorld` 默认信任它，fake provider 默认用它。
    pub fn shared() -> io::Result<&'static Self> {
        static SHARED: OnceLock<Result<TlsIdentity, String>> = OnceLock::new();
        SHARED
            .get_or_init(|| Self::generate().map_err(|error| error.to_string()))
            .as_ref()
            .map_err(|error| io::Error::other(error.clone()))
    }

    /// CA 证书（PEM），供 `SSL_CERT_FILE` 使用。
    pub fn ca_pem(&self) -> &[u8] {
        &self.ca_pem
    }

    /// CA 证书（DER），供进程内客户端使用。
    pub fn ca_der(&self) -> &[u8] {
        &self.ca_der
    }

    /// 服务端配置；`alpn` 为空表示不协商应用协议。
    pub(crate) fn server_config(&self, alpn: &[&[u8]]) -> io::Result<Arc<rustls::ServerConfig>> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(self.cert_der.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.clone())),
            )
            .map_err(io::Error::other)?;
        config.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
        Ok(Arc::new(config))
    }
}

/// 有界执行一次 `openssl`：超过 30 秒就杀掉并报错，避免证书生成拖住任意场景的启动。
fn run_openssl(args: &[&str], dir: &std::path::Path) -> io::Result<()> {
    const LIMIT: std::time::Duration = std::time::Duration::from_secs(30);
    let mut child = Command::new("openssl")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let started = std::time::Instant::now();
    while child.try_wait()?.is_none() {
        if started.elapsed() >= LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other(format!(
                "openssl {} did not finish within {LIMIT:?}",
                args.join(" ")
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let out = child.wait_with_output()?;
    if out.status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "openssl {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    )))
}
