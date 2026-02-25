//! Integration tests for BrowserType::connect_over_cdp()
//!
//! Tests cover:
//! - Chromium-only enforcement (Firefox/WebKit should fail)
//! - Real CDP connection to a Chrome instance with remote debugging

use playwright_rs::protocol::Playwright;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

mod common;

/// Test that connect_over_cdp fails for Firefox (Chromium-only)
#[tokio::test]
async fn test_connect_over_cdp_chromium_only() {
    common::init_tracing();

    let playwright = match Playwright::launch().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Skipping test: Failed to launch Playwright: {}", e);
            return;
        }
    };

    // Firefox should fail
    let result = playwright
        .firefox()
        .connect_over_cdp("http://localhost:9222", None)
        .await;
    assert!(
        result.is_err(),
        "Firefox should not support connect_over_cdp"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Chromium"),
        "Error should mention Chromium: {}",
        err
    );

    // WebKit should fail
    let result = playwright
        .webkit()
        .connect_over_cdp("http://localhost:9222", None)
        .await;
    assert!(
        result.is_err(),
        "WebKit should not support connect_over_cdp"
    );

    playwright.shutdown().await.ok();
}

/// Launch Chrome with --remote-debugging-port and return the CDP endpoint URL.
///
/// Uses Playwright Node.js to find the Chrome binary and launch it with
/// remote debugging enabled, then discovers the CDP endpoint via /json/version.
async fn start_chrome_with_cdp(
    package_path: &std::path::Path,
) -> Option<(tokio::process::Child, String)> {
    // Node.js script that:
    // 1. Gets Chrome executable path from Playwright
    // 2. Spawns Chrome with --remote-debugging-port=0
    // 3. Reads the DevTools URL from stderr
    // 4. Outputs the HTTP endpoint to stdout
    let script = format!(
        r#"
const {{ chromium }} = require('{}');
const {{ spawn }} = require('child_process');
const http = require('http');

const execPath = chromium.executablePath();

const child = spawn(execPath, [
    '--headless',
    '--remote-debugging-port=0',
    '--no-sandbox',
    '--disable-gpu',
    '--use-mock-keychain',
    '--no-first-run'
], {{ stdio: ['pipe', 'pipe', 'pipe'] }});

// Chrome outputs the DevTools URL to stderr
let stderr = '';
child.stderr.on('data', (data) => {{
    stderr += data.toString();
    // Look for the DevTools listening message
    const match = stderr.match(/DevTools listening on (ws:\/\/[^\s]+)/);
    if (match) {{
        // Extract port from ws://127.0.0.1:PORT/devtools/browser/...
        const portMatch = match[1].match(/:(\d+)\//);
        if (portMatch) {{
            console.log('http://127.0.0.1:' + portMatch[1]);
        }}
    }}
}});

// Keep running until stdin closes
process.stdin.resume();
process.stdin.on('close', () => {{
    child.kill();
    process.exit(0);
}});

// Also kill on timeout (safety)
setTimeout(() => {{
    child.kill();
    process.exit(1);
}}, 25000);
"#,
        package_path.display()
    );

    let mut child = Command::new("node")
        .arg("-e")
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let stdout = child.stdout.take()?;
    let mut reader = tokio::io::BufReader::new(stdout).lines();

    // Wait for the CDP endpoint with timeout
    let endpoint = tokio::time::timeout(Duration::from_secs(15), async {
        while let Ok(Some(line)) = reader.next_line().await {
            if line.starts_with("http://") || line.starts_with("ws://") {
                return Some(line);
            }
        }
        None
    })
    .await
    .ok()??;

    Some((child, endpoint))
}

/// Test connecting to a real Chrome via CDP
#[tokio::test]
async fn test_connect_over_cdp_real_chrome() {
    common::init_tracing();

    // Find the Playwright package path
    let drivers_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("drivers");

    let package_path = std::fs::read_dir(&drivers_dir)
        .ok()
        .and_then(|mut entries| entries.next())
        .and_then(|e| e.ok())
        .map(|e| e.path().join("package"));

    let package_path = match package_path {
        Some(p) if p.exists() => p,
        _ => {
            tracing::warn!("Skipping test: Playwright driver not found");
            return;
        }
    };

    // Start Chrome with CDP
    let (mut chrome_process, cdp_endpoint) = match start_chrome_with_cdp(&package_path).await {
        Some(result) => result,
        None => {
            tracing::warn!("Skipping test: Failed to start Chrome with CDP");
            return;
        }
    };

    tracing::info!("Chrome CDP endpoint: {}", cdp_endpoint);

    // Launch local Playwright
    let playwright = match Playwright::launch().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Skipping test: Failed to launch Playwright: {}", e);
            let _ = chrome_process.kill().await;
            return;
        }
    };

    // Connect over CDP
    let browser = match playwright
        .chromium()
        .connect_over_cdp(&cdp_endpoint, None)
        .await
    {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("Failed to connect over CDP: {}", e);
            let _ = playwright.shutdown().await;
            let _ = chrome_process.kill().await;
            panic!("connect_over_cdp failed: {:?}", e);
        }
    };

    tracing::info!("Connected via CDP! Browser version: {}", browser.version());

    // Verify browser works
    assert!(browser.is_connected());
    assert!(!browser.version().is_empty());

    // Create a page and navigate
    let page = browser.new_page().await.expect("Failed to create page");
    page.goto("data:text/html,<h1>CDP Connection Works!</h1>", None)
        .await
        .expect("Failed to navigate");

    let heading = page.locator("h1").await;
    let text = heading.text_content().await.expect("Failed to get text");
    assert_eq!(text, Some("CDP Connection Works!".to_string()));

    tracing::info!("CDP connection test passed!");

    // Cleanup
    browser.close().await.ok();
    playwright.shutdown().await.ok();
    let _ = chrome_process.kill().await;
}
