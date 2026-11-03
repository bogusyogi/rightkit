# rightkit-connector-gen

Workflow-automation connectors (n8n, Zapier, Make) generated from an app's engine contract instead of hand-written per app (rightkit#1243, seen in Postiz; design decided here from the `rightkit-job-queue` contract). Catalog capability: `workflow-connector-gen` (declaration-only). std only.

## Use

Describe the engine once as a `Contract` (engine name, connector version, and per method a summary and typed params; these are what `Dispatcher::methods()` and `Dispatcher::method_meta()` carry), then render:

- `n8n::render` -> `package.json`, `tsconfig.json`, a credentials class and a declarative node (`nodes/<Name>/<Name>.node.ts`).
- `zapier::render` -> `package.json` and `index.js` (one create action per method, token auth, `doctor` as the connection test).
- `make::render` -> `make-app.json` (one action module per method).

Each render returns `GeneratedFile { path, contents }` values; the caller writes them (a build script or xtask in the app). Nothing here reads files or touches the network.

## The HTTP convention

Connectors call the app's HTTP bridge, never the engine directly: `POST {baseUrl}/rpc/{method}`, params as the JSON body, `Authorization: Bearer {token}`, the engine `result` as the JSON response, `{"error":{"code","message"}}` with a 4xx or 5xx status on failure. Serving that bridge from a `Dispatcher` (over `rightkit-daemon` or the app's own HTTP surface) is the app's.

## Known limits

- The Make definition follows the Make Apps SDK concepts but has not been imported into Make; the first import is its acceptance run. The n8n and Zapier sources have not been built or published either (device or service run).
- JSON params travel as text and are parsed by the connector (Zapier) or sent as the node's JSON value (n8n).
