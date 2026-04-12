import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
    testDir: "./tests",
    timeout: 60_000,
    fullyParallel: false,
    workers: 1,
    reporter: "list",
    use: {
        baseURL: process.env.SNITCHWATCH_TAURI_BASE ?? "http://127.0.0.1:3031",
        trace: "on-first-retry",
    },
    projects: [
        {
            name: "firefox",
            use: { ...devices["Desktop Firefox"] },
        },
    ],
});
