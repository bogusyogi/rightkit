// Test-only recording fake of the App Store Connect endpoints the TestFlight distribution uses. Not shipped: lives under test-support/.
const APP_MARKETING = "1.0.214";
const APP_BUILD = "2026101801";
// A recording fake of the App Store Connect endpoints used by the distribution.
export function fakeAsc({ builds, groups, assigned = {}, externalState = "READY_FOR_BETA_SUBMISSION", submissions = [], failAssign = false, failStatus, partSize = 4, uploadStates = ["PROCESSING", "COMPLETE"], failPut = false, uploadErrors = [], upload = {}, apps = [{ id: "6806854037", bundleId: "app.heardright.ios.native" }] } = {}) {
  // upload: { existing: [{ id, state, marketing?, build?, files?: [{ id, fileName, fileSize, deliveryState?, operations? }] }],
  //   putStatuses: [503, 200...] consumed per part PUT, retryAfter: "2", opHost, expiration, urlExpires, headers, flaky: { "GET /v1/buildUploads/bu1": [503, 429] } }
  const opHost = upload.opHost ?? "store-030.blobstore.apple.com";
  const flaky = new Map(Object.entries(upload.flaky ?? {}).map(([key, list]) => [key, [...list]]));
  const putStatuses = [...(upload.putStatuses ?? [])];
  const requests = [];
  const state = { builds: builds ?? [{ id: "b1", version: APP_BUILD, marketing: APP_MARKETING, states: ["VALID"] }], assigned: new Map(Object.entries(assigned).map(([id, list]) => [id, new Set(list)])), submissions: [...submissions], externalState };
  const groupList = groups ?? [{ id: "g1", name: "Internal Dogfood", internal: true }, { id: "g2", name: "Friends", internal: false }];
  let buildPolls = 0;
  let uploadPolls = 0;
  const puts = [];
  const fileSizes = new Map();
  const reply = (status, body, headers = {}) => ({ ok: status < 400, status, json: async () => body, headers: { get: (name) => headers[name.toLowerCase()] ?? null } });
  const fetchImpl = async (url, init = {}) => {
    const parsed = new URL(url);
    const method = init.method ?? "GET";
    requests.push({ method, url, path: parsed.pathname, headers: init.headers, body: typeof init.body === "string" ? JSON.parse(init.body) : undefined });
    // Pre-signed upload parts: no bearer token may reach them.
    if (parsed.host !== "api.appstoreconnect.apple.com") {
      puts.push({ method, url, host: parsed.host, headers: init.headers ?? {}, body: Buffer.from(init.body ?? []) });
      const status = putStatuses.length ? putStatuses.shift() : failPut ? 403 : 200;
      return reply(status, {}, upload.retryAfter && status >= 429 ? { "retry-after": upload.retryAfter } : {});
    }
    const flakyList = flaky.get(`${method} ${parsed.pathname}`);
    if (flakyList?.length) { const status = flakyList.shift(); return reply(status, { errors: [{ detail: "try later" }] }, upload.retryAfter ? { "retry-after": upload.retryAfter } : {}); }
    const operationsFor = (size) => {
      const operations = [];
      for (let offset = 0, part = 1; offset < size; offset += partSize, part += 1) {
        operations.push({
          method: "PUT", url: `https://${opHost}/part/${part}?sig=secret-signature${upload.urlExpires ? `&Expires=${upload.urlExpires}` : ""}`, length: Math.min(partSize, size - offset), offset, partNumber: part,
          ...(upload.expiration ? { expiration: upload.expiration } : {}), requestHeaders: upload.headers ?? [{ name: "Content-Type", value: "application/octet-stream" }],
        });
      }
      return operations;
    };
    if (parsed.pathname === "/v1/apps" && method === "GET") {
      // like the real filter, a prefix match: the caller must keep only the exact bundle id
      const wanted = parsed.searchParams.get("filter[bundleId]") ?? "";
      return reply(200, { data: apps.filter((app) => app.bundleId.startsWith(wanted)).map((app) => ({ id: app.id, type: "apps", attributes: { bundleId: app.bundleId } })) });
    }
    if (/^\/v1\/apps\/\d+\/buildUploads$/.test(parsed.pathname) && method === "GET") {
      return reply(200, { data: (upload.existing ?? []).map((item) => ({ id: item.id, type: "buildUploads", attributes: { cfBundleShortVersionString: item.marketing ?? APP_MARKETING, cfBundleVersion: item.build ?? APP_BUILD, platform: "IOS", state: { state: item.state }, ...(item.createdDate ? { createdDate: item.createdDate } : {}) } })) });
    }
    const filesList = /^\/v1\/buildUploads\/([^/]+)\/buildUploadFiles$/.exec(parsed.pathname);
    if (filesList && method === "GET") {
      const item = (upload.existing ?? []).find((candidate) => candidate.id === filesList[1]);
      return reply(200, { data: (item?.files ?? []).map((file) => ({ id: file.id, type: "buildUploadFiles", attributes: { fileName: file.fileName, fileSize: file.fileSize, ...(file.checksum ? { sourceFileChecksums: { file: file.checksum } } : {}), ...(file.deliveryState ? { assetDeliveryState: { state: file.deliveryState } } : {}), uploadOperations: file.operations ?? operationsFor(file.fileSize) } })) });
    }
    if (parsed.pathname === "/v1/buildUploads" && method === "POST") return reply(201, { data: { id: "bu1", type: "buildUploads", attributes: { state: { state: "AWAITING_UPLOAD" } } } });
    if (parsed.pathname === "/v1/buildUploadFiles" && method === "POST") {
      const size = JSON.parse(init.body).data.attributes.fileSize;
      fileSizes.set("bf1", size);
      return reply(201, { data: { id: "bf1", type: "buildUploadFiles", attributes: { uploadOperations: operationsFor(size) } } });
    }
    if (/^\/v1\/buildUploadFiles\/[^/]+$/.test(parsed.pathname) && method === "PATCH") return reply(200, { data: { id: "bf1" } });
    const uploadGet = /^\/v1\/buildUploads\/([^/]+)$/.exec(parsed.pathname);
    if (uploadGet && method === "GET") {
      const current = uploadStates[Math.min(uploadPolls, uploadStates.length - 1)];
      uploadPolls += 1;
      return reply(200, { data: { id: uploadGet[1], attributes: { state: { state: current, errors: current === "FAILED" ? uploadErrors : [] } } } });
    }
    if (failStatus) return reply(failStatus, { errors: [{ title: "Forbidden", detail: `no access Bearer ${"x".repeat(40)}` }] });
    if (parsed.pathname === "/v1/builds" && method === "GET") {
      const index = Math.min(buildPolls, state.builds[0].states.length - 1);
      buildPolls += 1;
      const data = state.builds.map((build) => ({ id: build.id, type: "builds", attributes: { version: build.version, processingState: build.states[Math.min(index, build.states.length - 1)] }, relationships: { preReleaseVersion: { data: { id: `pre-${build.id}` } } } }));
      const included = state.builds.map((build) => ({ id: `pre-${build.id}`, type: "preReleaseVersions", attributes: { version: build.marketing } }));
      return reply(200, { data, included });
    }
    if (/^\/v1\/apps\/\d+\/betaGroups$/.test(parsed.pathname)) return reply(200, { data: groupList.map((group) => ({ id: group.id, attributes: { name: group.name, isInternalGroup: group.internal, ...(group.allBuilds !== undefined ? { hasAccessToAllBuilds: group.allBuilds } : {}) } })) });
    const groupBuilds = /^\/v1\/betaGroups\/([^/]+)\/relationships\/builds$/.exec(parsed.pathname);
    if (groupBuilds && method === "GET") return reply(200, { data: [...(state.assigned.get(groupBuilds[1]) ?? [])].map((id) => ({ id, type: "builds" })) });
    if (groupBuilds && method === "POST") {
      if (failAssign) return reply(409, { errors: [{ detail: "conflict" }] });
      const set = state.assigned.get(groupBuilds[1]) ?? new Set();
      for (const item of JSON.parse(init.body).data) set.add(item.id);
      state.assigned.set(groupBuilds[1], set);
      return reply(204, {});
    }
    if (/\/buildBetaDetail$/.test(parsed.pathname)) return reply(200, { data: { attributes: { externalBuildState: state.externalState } } });
    if (parsed.pathname === "/v1/betaAppReviewSubmissions" && method === "GET") return reply(200, { data: state.submissions });
    if (parsed.pathname === "/v1/betaAppReviewSubmissions" && method === "POST") {
      state.submissions.push({ id: "sub1", attributes: { betaReviewState: "WAITING_FOR_REVIEW" } });
      state.externalState = "WAITING_FOR_BETA_REVIEW";
      return reply(201, { data: { id: "sub1" } });
    }
    return reply(404, { errors: [{ detail: "unknown" }] });
  };
  return { fetchImpl, requests, state, puts };
}
