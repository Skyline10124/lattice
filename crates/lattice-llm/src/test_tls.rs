// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{
    io,
    process::{Command, Stdio},
    sync::{Arc, LazyLock},
    time::Duration,
};

pub(crate) struct TestTls {
    pub(crate) ca_der: Vec<u8>,
    pub(crate) server: Arc<rustls::ServerConfig>,
}

pub(crate) static TLS: LazyLock<TestTls> =
    LazyLock::new(|| generate_tls().expect("generate temporary test certificates"));

/// 只在本进程生成一次两天期证书；载入后立即删除临时目录中的私钥文件。
fn generate_tls() -> io::Result<TestTls> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join("leaf.ext"),
        "basicConstraints=critical,CA:FALSE\n\
         keyUsage=critical,digitalSignature\n\
         extendedKeyUsage=serverAuth\n\
         subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1\n",
    )?;
    let ec = [
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:P-256",
        "-nodes",
    ];
    let ca = ["req", "-x509"]
        .into_iter()
        .chain(ec)
        .chain([
            "-config",
            "/dev/null",
            "-keyout",
            "ca-key.pem",
            "-out",
            "ca.pem",
            "-days",
            "2",
            "-subj",
            "/CN=LATTICE test CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ])
        .collect::<Vec<_>>();
    let leaf = ["req", "-new"]
        .into_iter()
        .chain(ec)
        .chain([
            "-config",
            "/dev/null",
            "-keyout",
            "key.pem",
            "-out",
            "leaf.csr",
            "-subj",
            "/CN=localhost",
        ])
        .collect::<Vec<_>>();
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
    ] {
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TLS certificate generation exceeded 30 seconds",
            ));
        }
        let mut child = Command::new("openssl")
            .args(&args)
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(result.err().unwrap_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::TimedOut,
                            "TLS certificate generation exceeded 30 seconds",
                        )
                    }));
                }
            }
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "openssl {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
    }
    let ca_der = CertificateDer::from_pem_file(dir.path().join("ca.pem"))
        .map_err(io::Error::other)?
        .as_ref()
        .to_vec();
    let cert =
        CertificateDer::from_pem_file(dir.path().join("cert.pem")).map_err(io::Error::other)?;
    let key = PrivateKeyDer::from_pem_file(dir.path().join("key.pem")).map_err(io::Error::other)?;
    let server = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(io::Error::other)?
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .map_err(io::Error::other)?;
    Ok(TestTls {
        ca_der,
        server: Arc::new(server),
    })
}
