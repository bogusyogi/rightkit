import { withQaPage } from "./helpers/qaBrowser";

await withQaPage("library-default", async (page) => {
    const closeBox = await page.$eval('button[aria-label="Close"]', (button) => {
      const rect = button.getBoundingClientRect();
      return {
        top: rect.top,
        rightGap: window.innerWidth - rect.right,
        height: rect.height,
        width: rect.width,
      };
    });
    if (
      closeBox.top !== 0 ||
      closeBox.rightGap !== 0 ||
      closeBox.height < 38 ||
      closeBox.width < 40
    ) {
      throw new Error(`Close hitbox is not flush top-right: ${JSON.stringify(closeBox)}`);
    }

    const deleteButtonsBefore = await page.$$('[aria-label^="Delete transcript"]');
    if (deleteButtonsBefore.length !== 6) {
      throw new Error(`Expected 6 delete buttons, found ${deleteButtonsBefore.length}`);
    }

    await deleteButtonsBefore[0].click();
    await page.waitForFunction(
      () => document.querySelector('[title="Click again to confirm deletion"]') !== null,
      { timeout: 5000 },
    );
    await deleteButtonsBefore[0].click();

    await page.waitForFunction(
      () => document.querySelectorAll('[aria-label^="Delete transcript"]').length === 5,
      { timeout: 5000 },
    );
    const openLabels = await page.$$eval('[aria-label^="Open transcript"]', (buttons) =>
      buttons.map((button) => button.getAttribute("aria-label") ?? ""),
    );
    if (openLabels.some((label) => label.includes("Timestamped transcript demo"))) {
      throw new Error("Deleted transcript still appears in Library rows");
    }

    console.log(
      `library delete interaction passed: closeBox=${JSON.stringify(closeBox)} remainingRows=${openLabels.length}`,
    );
});
