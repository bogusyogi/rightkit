import { validateInstalledArtifact, fixtureEntry } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const recipeId = 'legal-ui-server-markup-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const noop = () => {};

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/legal-ui');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/legal-ui', rootResolvedUrl: root, entries: { '@rightkit/legal-ui': root, '@rightkit/legal-ui/legal-ui.css': import.meta.resolve('@rightkit/legal-ui/legal-ui.css') } });
  result.artifact = provenance.artifact;
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));
  const React = await import(fixtureEntry(import.meta.url, import.meta.resolve('react')));
  const { renderToStaticMarkup } = await import(fixtureEntry(import.meta.url, import.meta.resolve('react-dom/server')));
  const ui = await import(provenance.entries['@rightkit/legal-ui']);
  const css = readFileSync(fileURLToPath(provenance.entries['@rightkit/legal-ui/legal-ui.css']), 'utf8');
  const h = React.createElement;
  const render = (component, props) => renderToStaticMarkup(h(component, props));
  const body = (content, format, extra = {}) => render(ui.LegalDocumentBody, { content, format, ...extra });

  // Synthetic copy only; every URL uses the reserved .invalid TLD and is never fetched.
  const gateProps = (overrides = {}) => ({
    appName: 'FixtureRight', trustLine: 'Fixture trust line.', agreementStatement: 'I have read and agree to the fixture terms.', variant: 'first-run', onAgree: noop, onQuit: noop,
    documents: [{ title: 'License', url: 'https://example.invalid/license/' }, { title: 'Privacy', content: '# Privacy\n\nBody **bold**.', contentFormat: 'markdown' }, { title: 'Notices', onOpen: noop }],
    version: '2026-10-20.1', manifestSha256: 'a'.repeat(64),
    ...overrides,
  });
  const gate = overrides => render(ui.LegalGate, gateProps(overrides));

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('export-surface-exact-and-stylesheet-shipped', async () => {
    assert.deepEqual(Object.keys(ui).sort(), ['LegalDocumentBody', 'LegalGate', 'policyGateDocuments']);
    assert.deepEqual(Object.keys(manifest.exports), ['.', './legal-ui.css']);
    assert.deepEqual(manifest.sideEffects, ['*.css']);
    assert.deepEqual(Object.keys(manifest.peerDependencies), ['react']);
    assert.equal(manifest.dependencies, undefined);
    assert(css.includes(':root { --rk-legal-ui-css: 1; }'), 'the stylesheet carries the marker that stops runtime style injection');
    for (const selector of ['.rk-legal-gate a', '.rk-legal-gate__body', '.rk-legal-gate__doc', '.rk-legal-gate .rk-sr-only']) assert(css.includes(selector), selector);
    return { exports: Object.keys(ui).length, stylesheetBytes: css.length };
  });

  await run('policy-documents-map-to-inline-markdown', async () => {
    const rendered = [{ title: 'EULA', content: '# EULA\n\nText.' }, { title: 'Privacy', content: 'Notice.', extra: 'dropped' }];
    assert.deepEqual(ui.policyGateDocuments(rendered), [
      { title: 'EULA', content: '# EULA\n\nText.', contentFormat: 'markdown' },
      { title: 'Privacy', content: 'Notice.', contentFormat: 'markdown' },
    ]);
    assert.deepEqual(ui.policyGateDocuments(rendered, { Privacy: 'https://example.invalid/privacy/', Missing: 'https://example.invalid/x/' }), [
      { title: 'EULA', content: '# EULA\n\nText.', contentFormat: 'markdown' },
      { title: 'Privacy', url: 'https://example.invalid/privacy/', content: 'Notice.', contentFormat: 'markdown' },
    ]);
    assert.deepEqual(ui.policyGateDocuments([]), []);
    const html = gate({ documents: ui.policyGateDocuments(rendered) });
    assert.equal((html.match(/aria-expanded="false"/g) ?? []).length, 2, 'each mapped document renders as a collapsed inline region');
    return { mapped: rendered.length };
  });

  await run('document-body-markdown-renders-safe-elements-only', async () => {
    const source = '# Title\n\n## Sub\n\n### Third\n\n- a\n- b\n\n1. one\n2) two\n\n---\n\nPara **bold** *it* `code` [ok](https://example.invalid/a) [mail](mailto:person@example.invalid) [bad](javascript:alert(1))\n<script>alert(1)</script> & <b>raw</b>\nsecond line';
    const html = body(source, 'markdown');
    assert(html.startsWith('<h2>Title</h2><h3>Sub</h3><h4>Third</h4><ul><li>a</li><li>b</li></ul><ol><li>one</li><li>two</li></ol><hr/><p>'), 'headings are shifted down one level because the page owns h1');
    assert(html.includes('<strong>bold</strong> <em>it</em> <code>code</code>'));
    assert(html.includes('<a href="https://example.invalid/a" target="_blank" rel="noreferrer">ok</a>'));
    assert(html.includes('<a href="mailto:person@example.invalid" target="_blank" rel="noreferrer">mail</a>'));
    assert(!/href="javascript/i.test(html) && !html.includes('<script') && !html.includes('<b>raw'), 'only http(s) and mailto become links and no raw HTML is injected');
    assert(html.includes('&lt;script&gt;alert(1)&lt;/script&gt; &amp; &lt;b&gt;raw&lt;/b&gt; second line'), 'raw HTML is escaped text and soft line breaks join with a space');
    const titled = body(source, 'markdown', { title: 'Title' });
    assert(titled.startsWith('<h3>Sub</h3>'), 'a leading heading equal to the title is omitted');
    assert(body('## Other\n\n# Title', 'markdown', { title: 'Title' }).includes('<h2>Title</h2>'), 'only the leading heading is omitted');
    assert.equal(body('Line one\r\nline two\r\n\r\nNext', 'markdown'), '<p>Line one line two</p><p>Next</p>', 'CRLF input is normalised');
    assert.equal(body('', 'markdown'), '');
    assert.equal(body('###### Deep\n\n####### Too deep', 'markdown'), '<h6>Deep</h6><p>####### Too deep</p>', 'h6 is the deepest heading and seven hashes are text');
    assert.equal(body('x\n<!-- kept -->', 'markdown'), '<p>x &lt;!-- kept --&gt;</p>', 'legacy mode keeps HTML comments as escaped text');
    return { documentBytes: html.length };
  });

  await run('document-body-extended-markdown-and-text-format', async () => {
    assert.equal(body('Hello<!-- hidden -->\n\n- item\n  continued\n\nA *two\nlines* here  \nnext', 'markdown', { markdown: 'extended' }), '<p>Hello</p><ul><li>item continued</li></ul><p>A <em>two lines</em> here<br/>next</p>');
    assert.equal(body('- item\n  continued', 'markdown'), '<ul><li>item</li></ul><p>continued</p>', 'legacy mode ends the list at an indented continuation');
    assert.equal(body('A *two\nlines* here', 'markdown'), '<p>A <em>two lines</em> here</p>', 'emphasis spans soft breaks in both modes');
    assert.equal(body('line1\n<b>x</b>\n', 'text'), '<pre class="rk-legal-gate__doc-text">line1\n&lt;b&gt;x&lt;/b&gt;\n</pre>', 'text mode keeps line breaks and escapes markup');
    assert.equal(body('# not a heading', 'text', { markdown: 'extended' }), '<pre class="rk-legal-gate__doc-text"># not a heading</pre>', 'the markdown mode is ignored for text');
    return { modes: ['legacy', 'extended', 'text'] };
  });

  await run('gate-first-run-default-markup', async () => {
    const html = gate();
    assert(html.startsWith('<div class="rk-legal-gate" style="--rk-accent:AccentColor;font-family:inherit;min-height:100vh;width:100%;'), 'window layout fills the viewport');
    assert(!/data-theme|color-scheme/.test(html), 'the gate never forces light or dark');
    assert(html.includes('<form class="rk-legal-gate__body" aria-labelledby="'));
    assert(html.includes('>Fixture<span style="color:AccentColor">Right</span></h1>'), 'the Right suffix of the wordmark carries the accent');
    assert(html.includes('font-size:34px'), 'first-run uses the larger heading');
    assert(!html.includes('The terms below have changed'));
    assert(html.includes('>Fixture trust line.</p>'));
    assert(html.includes('<nav aria-label="FixtureRight legal documents">'));
    assert(html.includes('<a href="https://example.invalid/license/" target="_blank" rel="noreferrer" style="font-size:13.5px">License</a>'), 'a hosted document opens in a new window');
    assert.match(html, /<a href="#" aria-expanded="false" aria-controls="[^"]+-doc-1" style="font-size:13.5px">Privacy<\/a>/, 'bundled content is an inline disclosure');
    assert(html.includes('<a href="#" style="font-size:13.5px">Notices</a>'), 'onOpen documents link to the host handler');
    assert.equal((html.match(/role="region"/g) ?? []).length, 0, 'no document region is open before a click');
    assert(html.includes('<legend class="rk-sr-only">Who is this license for</legend>'));
    assert.equal((html.match(/type="radio" name="rk-legal-gate-eligibility"/g) ?? []).length, 2);
    assert(html.includes('value="individual"/>') && html.includes('value="enterprise"/>'));
    assert(!/checked/.test(html), 'nothing is pre-selected or pre-agreed');
    assert(html.includes('For myself<br/>') && html.includes('For an organization<br/>'));
    assert(html.includes('>I have read and agree to the fixture terms.</span>'));
    assert(html.includes('<button type="submit" class="rk-legal-gate__agree" disabled="" '), 'Agree is disabled until an eligibility choice and the agreement are made');
    assert(html.includes('>Agree &amp; continue</button>'));
    assert(!html.includes('aria-busy'));
    assert(html.includes('<button type="button" class="rk-legal-gate__quit" '));
    assert(html.includes('>Quit</button>'));
    assert.match(html, /<p class="rk-legal-gate__dba"[^>]*>Orthic Labs is a trading name of [^<]+\.<\/p>/, 'the default trading-name notice is shown');
    assert(html.includes('class="rk-legal-gate__lockup"') && html.includes('<svg viewBox="0 0 120 120"'), 'the default publisher lockup is inlined');
    assert(!html.includes('role="alert"'), 'no error before a failed save');
    assert.equal(gate(), html, 'rendering is deterministic');
    return { markupBytes: html.length };
  });

  await run('gate-re-acceptance-and-copy-customisation', async () => {
    const re = gate({ variant: 're-acceptance' });
    assert(re.includes('The terms below have changed. Agree to continue, or quit.'));
    assert(re.includes('font-size:26px') && !re.includes('font-size:34px'), 're-acceptance uses the smaller heading');
    const custom = gate({
      agreeLabel: 'Accept and continue', showQuit: false, quitLabel: 'Leave', eyebrow: 'Fixture Licence v1', heading: 'Before you use Fixture', tradingNameNotice: 'Fixture Trading is a trading name of Fixture Ltd.',
      footer: h('a', { href: 'mailto:legal@example.invalid' }, 'Contact legal'),
    });
    assert(custom.includes('>Accept and continue</button>') && !custom.includes('Agree &amp; continue'));
    assert(!custom.includes('rk-legal-gate__quit') && !custom.includes('Leave'), 'showQuit false removes the button');
    assert(custom.includes('class="rk-legal-gate__eyebrow"') && custom.includes('>Fixture Licence v1</div>'));
    assert(custom.includes('letter-spacing:-0.02em;margin:0;text-align:left">Before you use Fixture</h1>'), 'a heading replaces the wordmark');
    assert(!custom.includes('<span style="color:AccentColor">Right</span>'));
    assert(custom.includes('>Fixture Trading is a trading name of Fixture Ltd.</p>'));
    assert(custom.includes('<div class="rk-legal-gate__footer" style="margin:16px 0 0"><a href="mailto:legal@example.invalid">Contact legal</a></div>'));
    assert(gate({ quitLabel: 'Leave' }).includes('>Leave</button>'));
    assert(!gate({ tradingNameNotice: false }).includes('rk-legal-gate__dba'), 'tradingNameNotice false hides the line');
    assert(gate({ dbaLine: 'Custom DBA line.' }).includes('>Custom DBA line.</p>'));
    assert(gate({ dbaLine: 'Custom DBA line.', tradingNameNotice: 'Wins.' }).includes('>Wins.</p>'), 'tradingNameNotice supersedes dbaLine');
    for (const empty of [undefined, null, false, '']) {
      const plain = gate({ eyebrow: empty, footer: empty });
      assert(!plain.includes('rk-legal-gate__eyebrow') && !plain.includes('rk-legal-gate__footer'), `empty node ${String(empty)} renders nothing`);
    }
    const labels = gate({ eligibilityLabels: { legendVisible: true, legend: 'Licence holder', individual: { label: 'Me', description: '' }, enterprise: { description: 'Org seats' } } });
    assert(labels.includes('<legend class="rk-legal-gate__legend" '), 'a visible legend replaces the screen-reader-only one');
    assert(labels.includes('>Licence holder</legend>') && !labels.includes('rk-sr-only'));
    assert(labels.includes('>Me</span></label>'), 'an empty description removes the description element');
    assert(labels.includes('For an organization<br/><span style="color:color-mix(in srgb, currentColor 68%, transparent);font-size:12.5px">Org seats</span>'), 'a partial override keeps the default label');
    assert(gate({ eligibilityLabels: { individual: { description: undefined } } }).includes('One person, my own devices'), 'an omitted description keeps the default');
    return { variants: ['first-run', 're-acceptance'], overrides: ['agreeLabel', 'showQuit', 'quitLabel', 'eyebrow', 'heading', 'tradingNameNotice', 'dbaLine', 'footer', 'eligibilityLabels'] };
  });

  await run('gate-theme-layout-lockup-and-assets', async () => {
    const themed = gate({ layout: 'contained', theme: { bg: '#F7F3EC', surface: '#FFFFFF', border: '#E4DCDA', text: '#272322', muted: '#686160', accent: '#DF3400', accentContrast: '#FFFFFF', fontDisplay: 'Fixture Display, sans-serif', fontBody: 'Fixture Body, sans-serif' } });
    assert(themed.startsWith('<div class="rk-legal-gate" style="--rk-accent:#DF3400;background:#F7F3EC;color:#272322;font-family:Fixture Body, sans-serif;width:100%;'), 'tokens reach the root; contained layout drops the viewport minimum');
    assert(!themed.includes('min-height:100vh'));
    assert(themed.includes('<span style="color:#DF3400">Right</span>'));
    assert(themed.includes('background:#DF3400;color:#FFFFFF;'), 'the primary button takes the accent pair');
    assert(themed.includes('font-family:Fixture Display, sans-serif;font-weight:400;font-size:34px'));
    assert(!/data-theme|color-scheme/.test(themed));
    const partial = gate({ theme: { accent: '#336699' } });
    assert(partial.startsWith('<div class="rk-legal-gate" style="--rk-accent:#336699;font-family:inherit;min-height:100vh;'), 'omitted tokens inherit from the host');
    assert(partial.includes('color:AccentColorText'));
    const noLockup = gate({ lockup: null });
    assert(noLockup.includes('<div class="rk-legal-gate__lockup" style="display:flex;align-items:center;justify-content:center;gap:10px"></div>'), 'lockup null hides the publisher lockup');
    assert(!noLockup.includes('<svg'));
    assert(gate({ lockup: h('em', null, 'Host lockup') }).includes('gap:10px"><em>Host lockup</em></div>'), 'a host lockup replaces the default');
    const logo = gate({ parentLogoSrc: 'https://example.invalid/mark.svg' });
    assert(logo.includes('<img src="https://example.invalid/mark.svg" alt="" aria-hidden="true" style="height:24px;width:auto"/>') && !logo.includes('<svg'));
    assert.equal(gate({ wordmarkFontUrl: 'https://example.invalid/tanker.woff2' }), gate(), 'font registration is a client effect; server markup is unchanged');
    for (const [appName, base, suffix] of [['MailRight', 'Mail', 'Right'], ['Right', 'Right', ''], ['Studio', 'Studio', '']]) {
      const html = gate({ appName });
      assert(html.includes(`>${base}<span style="color:AccentColor">${suffix}</span></h1>`), appName);
    }
    return { layouts: ['window', 'contained'], wordmarks: 3 };
  });

  assert.equal(result.scenarios.length, 7);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
