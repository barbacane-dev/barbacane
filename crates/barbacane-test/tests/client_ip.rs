//! The client address a request is attributed to: the TCP peer, or behind
//! `--trusted-proxies` the address the proxies report, as plugins, the WAF and
//! its audit records see it.

use std::time::Duration;

use barbacane_test::TestGateway;

fn fixture(name: &str) -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .join("tests/fixtures")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// The client address the mock dispatcher received, for a request carrying
/// `headers`.
async fn client_ip(gateway: &TestGateway, headers: &[(&str, &str)]) -> String {
    let mut request = gateway.request_builder(reqwest::Method::GET, "/client-ip");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.expect("request");
    assert_eq!(response.status(), 200);
    response.text().await.expect("body")
}

async fn status(gateway: &TestGateway, path: &str, headers: &[(&str, &str)]) -> u16 {
    let mut request = gateway.request_builder(reqwest::Method::GET, path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.send().await.expect("request").status().as_u16()
}

const SPOOFED: (&str, &str) = ("x-forwarded-for", "198.51.100.7");

#[tokio::test]
async fn without_trusted_proxies_the_peer_is_the_client() {
    let gateway = TestGateway::from_spec(&fixture("client-ip.yaml"))
        .await
        .expect("gateway");
    assert_eq!(client_ip(&gateway, &[SPOOFED]).await, "127.0.0.1");
    assert_eq!(
        client_ip(&gateway, &[("forwarded", "for=198.51.100.7")]).await,
        "127.0.0.1"
    );
    // A plugin's decision follows the same address.
    assert_eq!(status(&gateway, "/denied", &[SPOOFED]).await, 200);
}

#[tokio::test]
async fn an_untrusted_peer_cannot_report_a_client() {
    let gateway = TestGateway::from_spec_with_args(
        &fixture("client-ip.yaml"),
        &["--trusted-proxies", "10.0.0.0/8"],
    )
    .await
    .expect("gateway");
    assert_eq!(client_ip(&gateway, &[SPOOFED]).await, "127.0.0.1");
    assert_eq!(status(&gateway, "/denied", &[SPOOFED]).await, 200);
}

#[tokio::test]
async fn behind_a_trusted_proxy_the_reported_client_is_used() {
    let gateway = TestGateway::from_spec_with_args(
        &fixture("client-ip.yaml"),
        &["--trusted-proxies", "127.0.0.1, 10.0.0.0/8"],
    )
    .await
    .expect("gateway");

    assert!(
        gateway.log().wait_for(
            "client address read from x-forwarded-for behind 127.0.0.1/32, 10.0.0.0/8",
            Duration::from_secs(10)
        ),
        "the setting is logged at start:\n{}",
        gateway.log().text()
    );

    assert_eq!(client_ip(&gateway, &[SPOOFED]).await, "198.51.100.7");
    // Right to left past the trusted hops; a client-written hop is not believed.
    assert_eq!(
        client_ip(
            &gateway,
            &[("x-forwarded-for", "1.1.1.1, 198.51.100.7, 10.2.3.4")]
        )
        .await,
        "198.51.100.7"
    );
    // No header, or a hop that is not an address: the trusted peer.
    assert_eq!(client_ip(&gateway, &[]).await, "127.0.0.1");
    assert_eq!(
        client_ip(&gateway, &[("x-forwarded-for", "unknown")]).await,
        "127.0.0.1"
    );
    // Forwarded is not read when X-Forwarded-For is the configured header.
    assert_eq!(
        client_ip(&gateway, &[("forwarded", "for=198.51.100.7")]).await,
        "127.0.0.1"
    );
    // The plugin sees the reported client.
    assert_eq!(status(&gateway, "/denied", &[SPOOFED]).await, 403);
    assert_eq!(status(&gateway, "/denied", &[]).await, 200);
}

#[tokio::test]
async fn the_forwarded_header_is_read_when_configured() {
    let gateway = TestGateway::from_spec_with_args(
        &fixture("client-ip.yaml"),
        &[
            "--trusted-proxies",
            "127.0.0.1",
            "--client-ip-header",
            "forwarded",
        ],
    )
    .await
    .expect("gateway");
    assert_eq!(
        client_ip(
            &gateway,
            &[("forwarded", r#"for="[2001:db8:cafe::17]:4711";proto=https"#)]
        )
        .await,
        "2001:db8:cafe::17"
    );
    assert_eq!(client_ip(&gateway, &[SPOOFED]).await, "127.0.0.1");
}

/// The WAF's `REMOTE_ADDR` and its audit record carry the reported client.
#[tokio::test]
async fn the_waf_audit_record_names_the_reported_client() {
    let gateway =
        TestGateway::from_spec_with_args(&fixture("waf.yaml"), &["--trusted-proxies", "127.0.0.1"])
            .await
            .expect("gateway");
    assert_eq!(
        status(&gateway, "/waf/search?q=1'%20or%20'1'%3D'1", &[SPOOFED]).await,
        403
    );
    let record = gateway
        .log()
        .wait_for_line("WAF audit record", Duration::from_secs(10))
        .unwrap_or_else(|| panic!("no audit record:\n{}", gateway.log().text()));
    assert!(
        record.contains("198.51.100.7"),
        "the audit record names the reported client: {record}"
    );
}

#[test]
fn an_invalid_trusted_proxy_list_is_refused_at_start() {
    let binary = barbacane_test::gateway::find_barbacane_binary().expect("barbacane binary");
    for (args, envs) in [
        (vec!["--trusted-proxies", "10.0.0.0/33"], vec![]),
        (vec!["--trusted-proxies", "proxy.internal"], vec![]),
        (vec!["--client-ip-header", "x-real-ip"], vec![]),
        (
            vec![],
            vec![("BARBACANE_TRUSTED_PROXIES", "10.0.0.0/8,nonsense")],
        ),
    ] {
        let output = std::process::Command::new(&binary)
            .args(["serve", "--artifact", "unused.bca"])
            .args(&args)
            .env_remove("BARBACANE_TRUSTED_PROXIES")
            .env_remove("BARBACANE_CLIENT_IP_HEADER")
            .envs(envs.iter().copied())
            .output()
            .expect("run the gateway");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{args:?} {envs:?}: {stderr}");
        assert!(
            stderr.contains("--trusted-proxies") || stderr.contains("--client-ip-header"),
            "{args:?} {envs:?}: names the flag: {stderr}"
        );
    }
}

/// The plugins' own `trusted_proxies`, which `--trusted-proxies` replaces, is
/// refused at compile time with the plugin's other unknown keys.
#[tokio::test]
async fn a_plugin_configured_with_trusted_proxies_does_not_compile() {
    let plugins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    for (plugin, config) in [
        (
            "ip-restriction",
            r#"{ allow: ["10.0.0.0/8"], trusted_proxies: ["127.0.0.1"] }"#,
        ),
        (
            "rate-limit",
            r#"{ quota: 10, window: 60, trusted_proxies: ["127.0.0.1"] }"#,
        ),
    ] {
        let dir = tempfile::TempDir::new().expect("temp dir");
        std::fs::write(
            dir.path().join("barbacane.yaml"),
            format!(
                "plugins:\n  {plugin}:\n    path: {}\n  mock:\n    path: {}\n",
                plugins.join(format!("{plugin}/{plugin}.wasm")).display(),
                plugins.join("mock/mock.wasm").display(),
            ),
        )
        .expect("manifest");
        let spec = dir.path().join("api.yaml");
        std::fs::write(
            &spec,
            format!(
                r#"openapi: "3.1.0"
info: {{ title: Legacy, version: "1.0.0" }}
paths:
  /legacy:
    get:
      operationId: legacy
      x-barbacane-middlewares:
        - name: {plugin}
          config: {config}
      x-barbacane-dispatch: {{ name: mock, config: {{ status: 200 }} }}
      responses: {{ "200": {{ description: ok }} }}
"#
            ),
        )
        .expect("spec");
        let error = match TestGateway::from_spec(spec.to_str().expect("utf-8 path")).await {
            Ok(_) => panic!("{plugin} compiled with trusted_proxies"),
            Err(e) => e.to_string(),
        };
        assert!(
            error.contains("E1023") && error.contains("trusted_proxies"),
            "{plugin}: {error}"
        );
    }
}
