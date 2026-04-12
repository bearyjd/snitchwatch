import { test, expect } from '@playwright/test';
import { spawn, ChildProcess } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';

let bridge: ChildProcess;
let grpcAddr = '';

test.beforeAll(async () => {
  bridge = spawn('cargo', ['run', '-q', '-p', 'snitchwatch-bridge-cli'], {
    cwd: '../..',
    env: {
      ...process.env,
      SNITCHWATCH_WS_BIND: '127.0.0.1:3031',
      SNITCHWATCH_GRPC_BIND: '127.0.0.1:50321',
      RUST_LOG: 'warn',
    },
    stdio: 'inherit',
  });
  await delay(15_000);
  grpcAddr = '127.0.0.1:50321';
});

test.afterAll(async () => {
  if (bridge && !bridge.killed) bridge.kill('SIGTERM');
});

test('AskRule from mock daemon shows up in the Connections list', async ({ page }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');

  // Spawn the helper binary in the background — it will block on the AskRule
  // until the user clicks Allow/Deny in the UI.
  const helper = spawn(
    'cargo',
    [
      'run',
      '-q',
      '--manifest-path',
      'tests/web_smoke/helpers/Cargo.toml',
      '--',
      '--grpc',
      grpcAddr,
      '--process',
      '/usr/bin/curl',
      '--host',
      'example.com',
      '--port',
      '443',
    ],
    { cwd: '../..', stdio: 'pipe' },
  );

  // Wait for the row to show up in the Connections list.
  await expect(page.getByText('example.com')).toBeVisible({ timeout: 10_000 });
  await expect(page.getByText('curl')).toBeVisible();

  // Click Allow in the inspector pane. (The exact selector depends on the
  // vendored UI; see web/js/connections.js for the inspector button id.)
  await page.getByRole('button', { name: /allow/i }).first().click();

  // The helper exits when the bridge responds with the synthesized Rule.
  const code: number = await new Promise(res => helper.on('exit', c => res(c ?? -1)));
  expect(code).toBe(0);
});
