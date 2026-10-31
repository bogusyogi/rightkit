// Preload for a child node process of a test (see simulated-host.mjs): reads RK_TEST_SIMULATED_PLATFORM and makes `process.platform` read as it. Test-only; never imported by product code.
const platform = process.env.RK_TEST_SIMULATED_PLATFORM;
if (platform === "darwin" || platform === "win32") Object.defineProperty(process, "platform", { ...Object.getOwnPropertyDescriptor(process, "platform"), value: platform });
