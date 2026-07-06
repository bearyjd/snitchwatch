import { test, expect } from "@playwright/test";
import { spawn, ChildProcess } from "child_process";

let bridge: ChildProcess | null = null;

test.beforeAll(async () => {
    bridge = spawn("cargo", ["run", "-p", "snitchwatch-bridge-cli", "--quiet"], {
        env: {
            ...process.env,
            SNITCHWATCH_WS_BIND: "127.0.0.1:3031",
            SNITCHWATCH_GRPC_BIND: "127.0.0.1:50051",
            RUST_LOG: "warn",
        },
        stdio: "inherit",
    });
    await new Promise((r) => setTimeout(r, 15_000));
});

test.afterAll(async () => {
    if (bridge) bridge.kill("SIGTERM");
});

test("ask_rule arrives in connections list", async ({ page }) => {
    test.skip(
        !process.env.SNITCHWATCH_MOCK_DAEMON_RUNNING,
        "needs mock_opensnitchd to be running on 127.0.0.1:50051"
    );

    await page.goto("/");
    // Dismiss any onboarding overlay (it shouldn't appear if mock is up).
    await page.evaluate(() => {
        const o = document.getElementById("onboarding-overlay");
        if (o) o.hidden = true;
    });

    // Fire AskRule via the M2 helper binary.
    const helper = spawn(
        "cargo",
        ["run", "--manifest-path", "tests/web_smoke/helpers/Cargo.toml", "--", "firefox", "github.com", "443"],
        { stdio: "inherit" }
    );

    // Row should appear in the connections panel within 5 seconds.
    await expect(page.getByText("firefox")).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText("github.com")).toBeVisible();

    helper.kill("SIGTERM");
});
