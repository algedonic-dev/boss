// A forge retry can turn a failed first attempt into a green suite.
// Read the real configuration in separate processes: importing it once
// would cache the CI value and leave the other execution lanes untested.
import { expect, test } from 'bun:test';

const configUrl = new URL('../playwright.mocked.config.ts', import.meta.url).href;
const readRetries = `import config from ${JSON.stringify(configUrl)};
console.log(JSON.stringify({ retries: config.retries }));`;

for (const ci of [undefined, 'true', 'false'] as const) {
  test(`the actual mocked configuration keeps first failures with CI=${ci ?? 'unset'}`, () => {
    const { CI: _inheritedCi, ...environment } = process.env;
    const result = Bun.spawnSync([process.execPath, '-e', readRetries], {
      env: ci === undefined ? environment : { ...environment, CI: ci },
      stdout: 'pipe',
      stderr: 'pipe',
      timeout: 10_000,
    });

    expect(result.exitCode).toBe(0);
    expect(result.stderr.toString()).toBe('');
    const configuration: unknown = JSON.parse(result.stdout.toString());
    expect(configuration).toEqual({ retries: 0 });
  });
}
