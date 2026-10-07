# @rightkit/qa-chrome

Chrome for Testing for repository QA harnesses, with no runtime dependencies.

```js
import { resolveChromeForTesting, launchChromeForTesting } from '@rightkit/qa-chrome';

const b = await launchChromeForTesting({ args: ['--headless=new'] });
// connect to b.wsEndpoint / b.port
await b.close(); // CDP Browser.close, SIGTERM the group, SIGKILL last; temp profile removed
```

- Resolution: explicit path, then `RIGHTKIT_CHROME_FOR_TESTING`, then the app folders, the newest Puppeteer build and Playwright. It never falls back to regular Chrome; a miss throws `ChromeForTestingNotFoundError` with install steps.
- Use one browser per suite and a page per test.
- `@rightkit/qa` re-exports this module (`@rightkit/qa/chrome`). Depend on `@rightkit/qa-chrome` directly when you only need the browser, so you don't install WebdriverIO.
