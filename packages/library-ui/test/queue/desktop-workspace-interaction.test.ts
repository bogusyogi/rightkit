import assert from "node:assert/strict";
import type { QaPage as Page } from "@rightkit/qa";
import { withQaPage } from "./helpers/qaBrowser";

async function selectSource(page: Page, title: string) {
  const selector = `[aria-label="Source list"] [aria-label="Select ${title}"]`;
  await page.waitForSelector(selector);
  await page.click(selector);
}
async function menuAction(page: Page, trigger: string, label: string) {
  await page.click(trigger);
  await page.evaluate((expected) => {
    const button = [...document.querySelectorAll<HTMLButtonElement>("button")].find((candidate) =>
      candidate.textContent?.trim().startsWith(expected),
    );
    if (!button) throw new Error(`Menu action missing: ${expected}`);
    button.click();
  }, label);
}
async function clickButtonText(page: Page, label: string) {
  await page.evaluate((expected) => {
    const button = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
      (candidate) => candidate.textContent?.trim() === expected,
    );
    if (!button) throw new Error(`Button missing: ${expected}`);
    button.click();
  }, label);
}
async function clearSearch(page: Page) {
  await page.$eval('input[aria-label="Search sources"]', (input) => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, "");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function selectSourceFilter(page: Page, label: string) {
  await menuAction(page, 'summary[aria-label^="Show:"]', label);
  await page.waitForFunction(
    (expected) =>
      document.querySelector('summary[aria-label^="Show:"]')?.textContent?.includes(expected),
    {},
    label,
  );
}
async function sourceOptionLabels(page: Page) {
  return page.$$eval('[aria-label="Source list"] [role="option"]', (nodes) =>
    nodes.map((node) => node.getAttribute("aria-label") ?? ""),
  );
}
async function sourceRowTexts(page: Page) {
  return page.$$eval('[aria-label="Source list"] .row', (nodes) =>
    nodes.map((node) => (node.textContent ?? "").replace(/\s+/g, " ").trim()),
  );
}
async function openRuntimeAbout(page: Page) {
  await page.waitForSelector(".settings-dialog");
  await page.click('[data-section-id="about"]');
  await page.waitForFunction(() =>
    document
      .querySelector("#claude-settings-panel-about")
      ?.textContent?.includes("Parakeet TDT v3"),
  );
  return page.$eval("#claude-settings-panel-about", (node) => node.textContent ?? "");
}
await withQaPage("queue-default", async (page) => {
  await selectSource(page, "Timestamped transcript demo");
  const transcript = '[aria-label="Source reader"] .claude-body';
  await page.waitForFunction(
    (selector) =>
      document
        .querySelector(selector)
        ?.textContent?.includes("App keeps queue work visible"),
    {},
    transcript,
  );
  await page.click('[aria-label="Collapse sources sidebar"]');
  await page.waitForSelector('.sidebar[data-collapsed="true"]');
  assert.match(
    await page.$eval(transcript, (node) => node.textContent ?? ""),
    /App keeps queue work visible/,
  );
  await page.click('[aria-label="Expand sources sidebar"]');
  await page.waitForSelector('.sidebar[data-collapsed="false"]');
  await page.type('input[aria-label="Search sources"]', "Carousel OCR capture");
  await page.waitForFunction(
    () => document.querySelectorAll('[aria-label="Source list"] [role="option"]').length === 1,
  );
  assert.match(
    await page.$eval(transcript, (node) => node.textContent ?? ""),
    /App keeps queue work visible/,
    "filtering retains selected reading",
  );
  await selectSource(page, "Carousel OCR capture");
  await page.waitForFunction(() =>
    document
      .querySelector('[aria-label="Source reader"]')
      ?.textContent?.includes("Queue work stays visible"),
  );
  await page.waitForSelector('[aria-label="Source reader"] img[alt^="Slide "]');
  assert.equal(await page.$$('img[alt^="Slide "]').then((images) => images.length), 2);
  assert.match(
    await page.$eval('[aria-label="Source reader"]', (node) => node.textContent ?? ""),
    /Slide 1 of 2/,
  );
  await clearSearch(page);
  await page.type('input[aria-label="Search sources"]', "timestamped exports");
  await page.waitForFunction(
    () => document.querySelectorAll('[aria-label="Source list"] [role="option"]').length === 1,
  );
  assert.deepEqual(await sourceOptionLabels(page), ["Select Timestamped transcript demo"]);
  await clearSearch(page);
  await page.type('input[aria-label="Search sources"]', "sample-carousel.png");
  await page.waitForFunction(
    () => document.querySelectorAll('[aria-label="Source list"] [role="option"]').length === 1,
  );
  assert.deepEqual(await sourceOptionLabels(page), ["Select Pasted image"]);
  await clearSearch(page);
  const sourceRows = await sourceRowTexts(page);
  assert.ok(
    sourceRows.some((label) => label.includes("Carousel")),
    "carousel label is visible",
  );
  await selectSource(page, "Timestamped transcript demo");
  await page.waitForSelector('[aria-label="Timestamps"]:not(:disabled)');
  await page.waitForFunction(() => {
    const icons = [...document.querySelectorAll<HTMLImageElement>(".source-platform-icon")];
    return icons.length > 0 && icons.every((icon) => icon.complete && icon.naturalWidth > 0);
  });
  assert.ok(await page.$('[data-source-platform="youtube"]'));
  assert.ok(await page.$('[data-source-platform="instagram"]'));
  if (
    (await page.$eval('[aria-label="Timestamps"]', (button) =>
      button.getAttribute("aria-pressed"),
    )) !== "true"
  )
    await page.click('[aria-label="Timestamps"]');
  assert.equal(
    await page.$eval(transcript, (node) => node.classList.contains("has-timestamps")),
    true,
  );
  assert.ok(await page.$(`${transcript} p[data-time="00:00:14"]`));
  await page.setViewport({ width: 1440, height: 760, deviceScaleFactor: 1 });
  const readerLayout = await page.evaluate(() => {
    const pane = document.querySelector('[aria-label="Source reader"]')?.getBoundingClientRect();
    const documentArea = document.querySelector(".claude-doc")?.getBoundingClientRect();
    const body = document.querySelector(".claude-body")?.getBoundingClientRect();
    return {
      paneRight: pane?.right ?? 0,
      scrollRight: documentArea?.right ?? 0,
      textWidth: body?.width ?? 0,
      duplicateBrand: document.querySelector(".titlebar .wtitle") !== null,
      intakeLeft: document.querySelector(".entry")?.getBoundingClientRect().left ?? 0,
      planeLeft: document.querySelector(".plane")?.getBoundingClientRect().left ?? -1,
      sideHead:
        document.querySelector('.titlebar .side-head [aria-label="Collapse sources sidebar"]') !==
        null,
      planeBarTools: document.querySelector(".plane-bar .claude-toolbar") !== null,
    };
  });
  assert.ok(
    Math.abs(readerLayout.paneRight - readerLayout.scrollRight) <= 1,
    "scrollbar at pane edge",
  );
  assert.ok(readerLayout.textWidth > 900, "transcript uses available desktop width");
  assert.equal(readerLayout.duplicateBrand, false);
  assert.ok(
    Math.abs(readerLayout.intakeLeft - readerLayout.planeLeft) <= 1,
    "intake starts where the content plane starts",
  );
  assert.ok(readerLayout.sideHead, "sidebar toggle sits in the titlebar above the sidebar");
  assert.ok(readerLayout.planeBarTools, "reader tools sit in the plane toolbar");
  await page.click('button[aria-label="Search transcript"]');
  await page.waitForSelector('input[aria-label="Find in transcript"]');
  await page.type('input[aria-label="Find in transcript"]', "Timestamped text");
  await page.waitForSelector(`${transcript} mark`);
  await page.evaluate(
    `Object.defineProperty(navigator, "clipboard", {configurable:true,value:{writeText:async (text)=>{document.documentElement.dataset.qaCopiedText=text;}}})`,
  );
  await page.click('button[aria-label="Copy text"]');
  await page.waitForFunction(() =>
    document.documentElement.dataset.qaCopiedText?.includes("[00:00:14]"),
  );
  await page.click('button[aria-label="Save"]');
  const exports = await page.$eval(
    '[aria-label="Save transcript"]',
    (node) => node.textContent ?? "",
  );
  assert.match(exports, /Plain Text/);
  assert.match(exports, /Markdown/);
  assert.match(exports, /Subtitles/);
  await page.keyboard.press("Escape");
  const barActions = await page.$eval(".plane-bar", (node) => node.textContent ?? "");
  for (const action of [
    "Open Source",
    "Show in Folder",
    "Open File",
    "Copy Link",
    "Get Text Again",
  ])
    assert.ok(barActions.includes(action), `bar shows ${action}`);
  const barBox = await page.$eval(".plane-bar", (node) => {
    const bar = node.getBoundingClientRect();
    const plane = document.querySelector(".plane")?.getBoundingClientRect();
    return { barBottom: bar.bottom, planeBottom: plane?.bottom ?? 0 };
  });
  assert.ok(Math.abs(barBox.barBottom - barBox.planeBottom) <= 1, "action bar sits at the bottom");
  await page.click('button[aria-label="More"]');
  const actions = await page.$eval(
    '[aria-label="More transcript actions"]',
    (node) => node.textContent ?? "",
  );
  assert.ok(actions.includes("Remove from Queue"), "Remove from Queue");
  await page.keyboard.press("Escape");
  // The owner AppShell resize handle: ArrowRight grows the sidebar by its 8 px step (288 to 296).
  await page.focus('[aria-label="Resize sidebar"]');
  await page.keyboard.press("ArrowRight");
  assert.equal(
    await page.$eval('[aria-label="Resize sidebar"]', (node) => node.getAttribute("aria-valuenow")),
    "296",
  );
});
await withQaPage("queue-display-label", async (page) => {
  await page.waitForFunction(() =>
    [...document.querySelectorAll('[aria-label="Source list"] .row')].some((node) =>
      node.textContent?.includes("Post"),
    ),
  );
  const rows = await sourceRowTexts(page);
  assert.ok(
    rows.some((label) => label.includes("Post")),
    "instagram post label is visible",
  );
});
await withQaPage("queue-live-progress", async (page) => {
  await page.waitForFunction(() =>
    [...document.querySelectorAll('[aria-label="Source list"] [role="option"]')].some((node) =>
      node.textContent?.includes("capturing carousel slide"),
    ),
  );
  await page.type('input[aria-label="Search sources"]', "capturing carousel slide");
  await page.waitForFunction(
    () => document.querySelectorAll('[aria-label="Source list"] [role="option"]').length === 1,
  );
  assert.deepEqual(await sourceOptionLabels(page), [
    "Select https://www.instagram.com/p/DZAFFV-k6xb/",
  ]);
});
await withQaPage("queue-terminal-race", async (page) => {
  const row = '[aria-label="Select Terminal race demo"]';
  await page.waitForSelector(row);
  await page.click(row);
  await new Promise((resolve) => setTimeout(resolve, 1_000));
  const text = await page.$eval(row, (node) => node.textContent ?? "");
  assert.doesNotMatch(text, /Downloading|Transcribing/);
});
await withQaPage("queue-stale-delete", async (page) => {
  await selectSource(page, "Timestamped transcript demo");
  await page.waitForSelector('button[aria-label="More"]');
  await menuAction(page, 'button[aria-label="More"]', "Remove from Queue");
  await page.waitForFunction(
    () =>
      ![...document.querySelectorAll("[data-job-card]")].some((node) =>
        node.textContent?.includes("Timestamped transcript demo"),
      ),
  );
  const queueCards = await page.$$eval("[data-job-card]", (nodes) =>
    nodes.map((node) => node.textContent ?? ""),
  );
  assert.ok(
    !queueCards.some((text) => text.includes("Timestamped transcript demo")),
    "stale get_jobs snapshot does not resurrect removed queue card",
  );
});
await withQaPage("queue-dedup", async (page) => {
  await selectSourceFilter(page, "Needs Attention");
  assert.deepEqual(await sourceOptionLabels(page), [
    "Select https://www.instagram.com/p/private-demo/",
  ]);
  await selectSourceFilter(page, "In Progress");
  assert.equal((await sourceOptionLabels(page)).length, 2);
  await selectSourceFilter(page, "Saved Items");
  const savedLabels = await sourceOptionLabels(page);
  assert.equal(savedLabels.length, 6);
  assert.equal(new Set(savedLabels).size, savedLabels.length, "queue/history IDs are deduplicated");
  for (const title of [
    "Timestamped transcript demo",
    "Downloaded audio demo",
    "Downloaded video demo",
    "Video plus transcript demo",
    "Carousel OCR capture",
  ])
    assert.ok(savedLabels.includes(`Select ${title}`), `saved filter shows ${title}`);
  assert.equal(
    savedLabels.filter((label) => label === "Select Timestamped transcript demo").length,
    1,
    "queue-owned ID appears once despite duplicate history title",
  );
  assert.ok(
    !savedLabels.includes("Select Duplicate history title must stay hidden"),
    "duplicate history title stays hidden",
  );
});
await withQaPage("queue-error", async (page) => {
  await selectSourceFilter(page, "Needs Attention");
  const attentionLabels = await sourceOptionLabels(page);
  assert.equal(attentionLabels.length, 2);
  assert.ok(
    attentionLabels.includes("Select https://www.instagram.com/p/no-media-demo/"),
    "failed job remains in Needs Attention",
  );
  assert.ok(
    attentionLabels.includes("Select https://www.instagram.com/p/private-demo/"),
    "auth job remains in Needs Attention",
  );
});
await withQaPage("queue-default", async (page) => {
  await selectSource(page, "Timestamped transcript demo");
  await page.waitForSelector('button[aria-label="More"]');
  await menuAction(page, 'button[aria-label="More"]', "Remove from Queue");
  await page.waitForFunction(
    () => document.querySelector('[aria-label="Select Timestamped transcript demo"]') !== null,
  );
  await menuAction(page, ".fbtn", "Clear queue");
  await page.waitForFunction(() => document.querySelectorAll("[data-job-card]").length === 0);
  const titles = await page.$$eval('[aria-label="Source list"] [role="option"]', (nodes) =>
    nodes.map((node) => node.textContent).join("\n"),
  );
  assert.match(titles, /Timestamped transcript demo/);
  assert.match(titles, /Carousel OCR capture/);
  await page.click('button[aria-label="Settings"]');
  await page.waitForSelector(".settings-dialog");
  await page.click('[data-section-id="files"]');
  await page.waitForSelector("#settings-output-dir");
  const tabLabels = await page.$$eval("button[data-section-id]", (nodes) =>
    nodes.map((node) => node.getAttribute("data-section-id") ?? ""),
  );
  assert.deepEqual(tabLabels, ["general", "files", "accounts", "pro", "about"]);
  const filesText = await page.$eval(
    "#claude-settings-panel-files",
    (node) => node.textContent ?? "",
  );
  for (const label of [
    "Transcripts",
    "Downloads",
    "Video quality",
    "Audio quality",
    "Cookies file",
  ])
    assert.ok(filesText.includes(label), `files section shows ${label}`);
  assert.ok(!filesText.includes("Save Changes"), "settings save on change");
  await page.click('[data-section-id="accounts"]');
  const accountsText = await page.$eval(
    "#claude-settings-panel-accounts",
    (node) => node.textContent ?? "",
  );
  for (const name of ["Instagram", "Threads", "X", "TikTok"])
    assert.ok(accountsText.includes(name), `accounts shows ${name}`);
  await page.click('[data-section-id="pro"]');
  const proText = await page.$eval("#claude-settings-panel-pro", (node) => node.textContent ?? "");
  assert.match(proText, /Get Pro/);
  assert.match(proText, /Unlimited transcript length/);
  assert.match(proText, /Activate/);
  assert.ok(!/\bPlus\b/.test(proText), "no Plus tier copy");
  await page.type("#settings-license-key", "SR-QA-LICENSE");
  await page.click('#settings-pro button[type="submit"]');
  await page.waitForFunction(() =>
    document.querySelector("#settings-pro")?.textContent?.includes("Pro active"),
  );
  const activePlan = await page.$eval("#settings-pro", (node) => node.textContent ?? "");
  assert.match(activePlan, /Lifetime/);
  assert.match(activePlan, /Deactivate this device/);
  assert.equal(await page.$("#settings-pro .claude-settings__plans"), null);
  assert.ok(!/Get Pro|FOUNDERS|\$149|Free plan/.test(activePlan));
  await page.click('[data-section-id="about"]');
  await page.waitForFunction(() =>
    document
      .querySelector("#claude-settings-panel-about")
      ?.textContent?.includes("Parakeet TDT v3"),
  );
  const runtimeText = await page.$eval(
    "#claude-settings-panel-about",
    (node) => node.textContent ?? "",
  );
  assert.match(runtimeText, /Parakeet TDT v3\s+\(ready\)/);
  assert.match(runtimeText, /ONNX Runtime \(bundled\)/);
  assert.match(runtimeText, /Ready/);
  await page.click('[data-section-id="general"]');
  await page.click('button[aria-label="Copy includes timestamps"]');
  assert.equal(
    await page.$eval('button[aria-label="Copy includes timestamps"]', (n) =>
      n.getAttribute("aria-checked"),
    ),
    "true",
  );
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => !document.querySelector(".settings-dialog"));
  assert.equal(
    await page.evaluate(() => document.activeElement?.getAttribute("aria-label")),
    "Settings",
    "Escape restores trigger focus",
  );
  await selectSource(page, "Timestamped transcript demo");
  await page.waitForSelector('button[aria-label="Copy text"]');
  for (const width of [1200, 900, 800]) {
    await page.setViewport({ width, height: 760, deviceScaleFactor: 1 });
    const layout = await page.evaluate(() => {
      const entry = document.querySelector(".entry")?.getBoundingClientRect();
      const caps = document.querySelector(".titlebar .caps")?.getBoundingClientRect();
      const tools = document.querySelector(".plane-bar .reader-toolbar")?.getBoundingClientRect();
      const plane = document.querySelector(".plane")?.getBoundingClientRect();
      return {
        overflow: document.documentElement.scrollWidth - innerWidth,
        overlap: Math.max(
          entry && caps ? entry.right - caps.left : 0,
          tools && plane ? plane.left - tools.left : 0,
        ),
        copyWidth:
          document.querySelector('button[aria-label="Copy text"]')?.getBoundingClientRect().width ??
          0,
      };
    });
    assert.ok(layout.overflow <= 1, JSON.stringify(layout));
    assert.ok(layout.overlap <= 1, `toolbar overlap at ${width}: ${JSON.stringify(layout)}`);
    assert.ok(layout.copyWidth > 0);
  }
});
await withQaPage("queue-attachments", async (page) => {
  await selectSource(page, "launch-demo.mov");
  const cancel = '[aria-label="Cancel launch-demo.mov"]';
  await page.waitForSelector(cancel);
  await page.click(cancel);
  await page.waitForFunction(
    (selector) => {
      const button = document.querySelector(selector) as HTMLButtonElement | null;
      return button?.disabled && button.textContent?.includes("Cancelling");
    },
    {},
    cancel,
  );
  assert.ok(
    await page.$('[aria-label="Select launch-demo.mov"]'),
    "retain job until cancellation acknowledgement",
  );
});
await withQaPage("settings-runtime-macos", async (page) => {
  const runtimeText = await openRuntimeAbout(page);
  assert.match(runtimeText, /Parakeet TDT v3\s+\(ready\)/);
  assert.match(runtimeText, /CoreML \(built in\)/);
  assert.doesNotMatch(runtimeText, /\(missing\)/);
});
await withQaPage("settings-runtime-heardright", async (page) => {
  const runtimeText = await openRuntimeAbout(page);
  assert.match(runtimeText, /Ready — using App’s model/);
  assert.match(runtimeText, /App/);
  assert.doesNotMatch(runtimeText, /Not installed/);
});
await withQaPage("settings-runtime-missing", async (page) => {
  const runtimeText = await openRuntimeAbout(page);
  assert.match(runtimeText, /Parakeet TDT v3\s+\(missing\)/);
});
await withQaPage("settings-update-patch", async (page) => {
  await page.waitForSelector("button", { timeout: 10_000 });
  await clickButtonText(page, "Check for Updates");
  await page.waitForFunction(() => document.body.textContent?.includes("Patch 0.1.2 available"));
  await clickButtonText(page, "Install and restart");
  await page.waitForFunction(() =>
    document.body.textContent?.includes("Update installed — restarting"),
  );
  assert.equal(
    await page.$$eval("button", (buttons) =>
      buttons.some((button) => button.textContent?.trim() === "Install and restart"),
    ),
    false,
  );
});
await withQaPage("settings-update-feature", async (page) => {
  await clickButtonText(page, "Check for Updates");
  await page.waitForFunction(() =>
    document.body.textContent?.includes("Feature update 0.2.0 requires Pro"),
  );
  assert.equal(
    await page.$$eval("button", (buttons) =>
      buttons.some((button) => button.textContent?.trim() === "Install and restart"),
    ),
    false,
  );
});
await withQaPage("settings-update-pro", async (page) => {
  await clickButtonText(page, "Check for Updates");
  await page.waitForFunction(() =>
    document.body.textContent?.includes("Pro update 0.2.0 available"),
  );
  await clickButtonText(page, "Install and restart");
  await page.waitForFunction(() =>
    document.body.textContent?.includes("Update installed — restarting"),
  );
});
console.log("desktop workspace interaction passed");
