import { test, expect } from '@playwright/test';
import { spawn, ChildProcess } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';

let bridge: ChildProcess;

test.beforeAll(async () => {
  bridge = spawn('cargo', ['run', '-q', '-p', 'snitchwatch-bridge-cli'], {
    cwd: '../..',
    env: {
      ...process.env,
      SNITCHWATCH_WS_BIND: '127.0.0.1:3031',
      SNITCHWATCH_GRPC_BIND: '127.0.0.1:0',
      RUST_LOG: 'warn',
    },
    stdio: 'inherit',
  });
  // Give the bridge a few seconds to compile + bind. The first run may take
  // longer because of cargo compilation; subsequent runs reuse the artifact.
  await delay(15_000);
});

test.afterAll(async () => {
  if (bridge && !bridge.killed) bridge.kill('SIGTERM');
});

test('loads the Snitchwatch index page', async ({ page }) => {
  await page.goto('/');
  await expect(page).toHaveTitle(/Snitchwatch/i);
  await expect(page.locator('body')).not.toContainText(/Little Snitch/i);
});

test('loads the app.js asset', async ({ page }) => {
  const response = await page.goto('/assets/js/app.js');
  expect(response?.status()).toBe(200);
  const body = await response?.text();
  expect(body?.length ?? 0).toBeGreaterThan(0);
});

test('exposes the WebSocket endpoint', async ({ page }) => {
  // The /stream endpoint upgrades to WebSocket on a real client; with HTTP
  // GET it returns a 426 / 400. We just want to assert the route exists and
  // is not the SPA fallback.
  const response = await page.goto('/stream');
  // Either is acceptable — both prove the WS handler matched, not the fallback.
  expect([400, 426, 101]).toContain(response?.status() ?? 0);
});
