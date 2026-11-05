// Native process test on Linux, Windows and macOS. No shell or SSH server.
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const cwd = fileURLToPath(new URL('../', import.meta.url));
const cargo = process.platform === 'win32' ? 'cargo.exe' : 'cargo';
const run = (args, options = {}) => execFileSync(cargo, args, {
  cwd, stdio: 'inherit', ...options,
});
run(['build', '-p', 'sync-core', '--example', 'askpass_fixture']);
const metadata = JSON.parse(run(['metadata', '--format-version', '1', '--no-deps'], {
  stdio: ['ignore', 'pipe', 'inherit'], encoding: 'utf8',
}));
const fixture = path.join(metadata.target_directory, 'debug', 'examples',
  process.platform === 'win32' ? 'askpass_fixture.exe' : 'askpass_fixture');
run(['test', '-p', 'sync-core', '--test', 'askpass_process', '--', '--ignored'], {
  env: { ...process.env, PI_SYNC_TEST_ASKPASS: fixture },
});
