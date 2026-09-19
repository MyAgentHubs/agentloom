#![cfg(test)]

use super::*;

#[test]
fn startup_installs_a_unique_rustls_crypto_provider() {
    install_rustls_crypto_provider();

    assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    assert!(
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .is_err(),
        "进程级 CryptoProvider 只能安装一次"
    );
}
