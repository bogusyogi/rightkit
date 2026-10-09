import test from 'node:test';
import assert from 'node:assert/strict';
import { desktopIntrusionViolation } from './desktop-intrusion-policy.mjs';
import { claudeHookDecision } from './index.mjs';

const mac = (c) => desktopIntrusionViolation(c, { platform: 'darwin' });
const win = (c) => desktopIntrusionViolation(c, { platform: 'win32' });

for (const c of [
  'screencapture -x /tmp/a.png', '/usr/sbin/screencapture /tmp/a.png', 'sudo screencapture x', 'FOO=1 screencapture x',
  'ls && screencapture x', 'ls; screencapture x', 'ls | screencapture x', "bash -c 'screencapture x'", 'zsh -c "echo hi; screencapture x"',
  'notifyutil -p dev.orthic.pulse.hub.show.main', 'notifyutil --post foo', 'echo $(screencapture x)', 'env FOO=1 /usr/bin/notifyutil -p x',
  "osascript -e 'tell application \"Finder\" to activate'", 'osascript run.scpt', 'open -a Pulse', 'open -b dev.x', 'open Foo.app', 'open -n /Applications/X.app',
  'open https://example.com', 'open .', 'cliclick c:10,10', 'xdotool key a', 'ydotool click 1', 'sudo -u adrian open -a X',
]) test(`denies ${c}`, () => assert.ok(mac(c)));

for (const c of [
  'openssl x509 -in a.pem', 'opensnoop', 'reopen foo', 'cargo test', 'git status', 'git log --oneline', 'echo notifyutil', 'grep -r open src',
  'rightkit-qa hidden run', 'right-ctl screenshot app', '/x/agent-bin/right-ctl open-window', 'rightkit-control click --hidden', 'rightkit cargo -- test',
  'notifyutil -g some.state', 'bash -c "ls -la"', 'ls open',
]) test(`allows ${c}`, () => assert.equal(mac(c), null));

for (const c of ['nircmd.exe win activate', 'powershell -Command "[void][W]::SetForegroundWindow($h)"', 'pwsh -c "[System.Windows.Forms.SendKeys]::SendWait(\'a\')"',
  'powershell -c "$g.CopyFromScreen(0,0,0,0,$s)"', 'cmd /c nircmd savescreenshot x.png', 'xdotool key a'])
  test(`win32 denies ${c}`, () => assert.ok(win(c)));
for (const c of ['powershell -c Get-Date', 'Start-Process notepad.exe', 'cargo test', 'right-ctl x']) test(`win32 allows ${c}`, () => assert.equal(win(c), null));
test('heredoc bodies are data, but commands after them are checked', () => {
  assert.equal(mac("python3 - <<'EOF'\nopen(p).read()\nEOF"), null);
  assert.ok(mac("cat <<EOF\nhi\nEOF\nscreencapture x"));
});
test('win32 does not apply mac rules', () => assert.equal(win('screencapture x'), null));

test('hook output shape and reason', () => {
  const d = claudeHookDecision({ tool_name: 'Bash', tool_input: { command: 'screencapture -x /tmp/a.png' } });
  if (process.platform === 'darwin') {
    assert.equal(d.hookSpecificOutput.permissionDecision, 'deny');
    assert.match(d.hookSpecificOutput.permissionDecisionReason, /rightkit-qa/);
    assert.match(d.hookSpecificOutput.permissionDecisionReason, /right-ctl/);
  }
  assert.equal(claudeHookDecision({ tool_name: 'Bash', tool_input: { command: 'git status' } }).decision, 'allow');
});
