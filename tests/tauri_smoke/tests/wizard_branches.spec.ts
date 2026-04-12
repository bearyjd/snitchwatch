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
    // Wait for the bridge to bind.
    await new Promise((r) => setTimeout(r, 15_000));
});

test.afterAll(async () => {
    if (bridge) bridge.kill("SIGTERM");
});

test("connected branch hides the overlay", async ({ page }) => {
    test.skip(
        !process.env.SNITCHWATCH_MOCK_DAEMON_RUNNING,
        "needs mock_opensnitchd to be running on 127.0.0.1:50051"
    );
    await page.goto("/");
    const overlay = page.locator("#onboarding-overlay");
    await expect(overlay).toBeHidden();
});

test("unit_missing branch shows Install button", async ({ page }) => {
    await page.goto("/");
    const overlay = page.locator("#onboarding-overlay");
    await expect(overlay).toBeVisible();
    await expect(page.getByRole("button", { name: "Install" })).toBeVisible();
    await expect(page.getByText("Welcome to Snitchwatch")).toBeVisible();
});
